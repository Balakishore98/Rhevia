//! RTMP publishing.
//!
//! RTMP is what YouTube, Twitch and Facebook accept for contribution, so this
//! is the path that makes Rhevia a streaming application rather than a library
//! that receives video.

use std::time::Duration;

use bytes::Bytes;
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
/// Re-exported so a caller can describe a stream without depending on
/// rml_rtmp directly.
pub use rml_rtmp::sessions::StreamMetadata;

use rml_rtmp::sessions::{
    ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult,
    PublishRequestType,
};
use rml_rtmp::time::RtmpTimestamp;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Debug, thiserror::Error)]
pub enum RtmpError {
    #[error("{0} is not a usable RTMP URL: {1}")]
    BadUrl(String, &'static str),
    #[error("could not reach {0}: {1}")]
    Connect(String, #[source] std::io::Error),
    #[error("network error while streaming: {0}")]
    Io(#[from] std::io::Error),
    #[error("RTMP protocol error: {0}")]
    Protocol(String),
    #[error("the server rejected the connection: {0}")]
    ConnectionRejected(String),
    #[error("the server rejected the stream key: {0}")]
    PublishRejected(String),
    #[error("the server closed the connection")]
    Closed,
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
}

/// A parsed RTMP destination.
///
/// Platforms present these as a "server URL" plus a separate "stream key",
/// which concatenate into one URL. Accepting both forms means users can paste
/// whichever their dashboard gave them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtmpUrl {
    pub host: String,
    pub port: u16,
    pub app: String,
    pub stream_key: String,
}

impl RtmpUrl {
    pub fn parse(url: &str) -> Result<Self, RtmpError> {
        Self::parse_with_key(url, None)
    }

    /// Parses a server URL, optionally with the stream key supplied separately.
    pub fn parse_with_key(url: &str, stream_key: Option<&str>) -> Result<Self, RtmpError> {
        let bad = |why| RtmpError::BadUrl(url.to_string(), why);

        let rest = url
            .strip_prefix("rtmp://")
            .ok_or_else(|| bad("only rtmp:// is supported; rtmps:// needs TLS, not yet wired"))?;

        let (authority, path) = match rest.split_once('/') {
            Some((a, p)) => (a, p),
            None => (rest, ""),
        };
        if authority.is_empty() {
            return Err(bad("no host"));
        }

        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => (
                h.to_string(),
                p.parse().map_err(|_| bad("port is not a number"))?,
            ),
            None => (authority.to_string(), 1935),
        };

        // The first path segment is the application; everything after it is
        // the stream key, which legitimately contains slashes on some platforms.
        let path = path.trim_end_matches('/');
        let (app, key_from_path) = match path.split_once('/') {
            Some((a, k)) => (a.to_string(), k.to_string()),
            None => (path.to_string(), String::new()),
        };
        if app.is_empty() {
            return Err(bad("no application name in the path"));
        }

        let stream_key = match stream_key {
            Some(k) if !k.is_empty() => k.to_string(),
            _ => key_from_path,
        };
        if stream_key.is_empty() {
            return Err(bad("no stream key, in the URL or supplied separately"));
        }

        Ok(Self {
            host,
            port,
            app,
            stream_key,
        })
    }

    fn tc_url(&self) -> String {
        format!("rtmp://{}:{}/{}", self.host, self.port, self.app)
    }
}

/// How long to wait for each negotiation step before giving up.
const STEP_TIMEOUT: Duration = Duration::from_secs(15);

pub struct RtmpPublisher {
    stream: TcpStream,
    session: ClientSession,
    read_buf: Vec<u8>,
}

impl RtmpPublisher {
    /// Connects, handshakes, and gets as far as the server accepting our
    /// stream key. Returns ready to publish.
    pub async fn connect(url: &RtmpUrl) -> Result<Self, RtmpError> {
        let addr = format!("{}:{}", url.host, url.port);
        let mut stream = TcpStream::connect(&addr)
            .await
            .map_err(|e| RtmpError::Connect(addr, e))?;
        // Video is latency-sensitive and our writes are already framed, so
        // Nagle only adds delay.
        stream.set_nodelay(true)?;

        let leftover = Self::handshake(&mut stream).await?;

        let mut config = ClientSessionConfig::new();
        config.tc_url = Some(url.tc_url());
        // The 128-byte default fragments every video frame into many chunks.
        config.chunk_size = 4096;

        let (session, initial) = ClientSession::new(config)
            .map_err(|e| RtmpError::Protocol(format!("could not start a session: {e:?}")))?;

        let mut publisher = Self {
            stream,
            session,
            read_buf: vec![0u8; 8192],
        };
        publisher.dispatch(initial).await?;

        // Any bytes that arrived glued to the end of the handshake belong to
        // the session, and dropping them corrupts the first chunk.
        if !leftover.is_empty() {
            let results = publisher
                .session
                .handle_input(&leftover)
                .map_err(|e| RtmpError::Protocol(format!("{e:?}")))?;
            publisher.dispatch(results).await?;
        }

        publisher.request_connection(&url.app).await?;
        publisher.request_publishing(&url.stream_key).await?;
        Ok(publisher)
    }

    async fn handshake(stream: &mut TcpStream) -> Result<Vec<u8>, RtmpError> {
        let mut handshake = Handshake::new(PeerType::Client);
        let p0_p1 = handshake
            .generate_outbound_p0_and_p1()
            .map_err(|e| RtmpError::Protocol(format!("handshake start failed: {e:?}")))?;
        stream.write_all(&p0_p1).await?;
        stream.flush().await?;

        let mut buf = vec![0u8; 4096];
        loop {
            let read = tokio::time::timeout(STEP_TIMEOUT, stream.read(&mut buf))
                .await
                .map_err(|_| RtmpError::Timeout("the RTMP handshake"))??;
            if read == 0 {
                return Err(RtmpError::Closed);
            }

            match handshake
                .process_bytes(&buf[..read])
                .map_err(|e| RtmpError::Protocol(format!("handshake failed: {e:?}")))?
            {
                HandshakeProcessResult::InProgress { response_bytes } => {
                    if !response_bytes.is_empty() {
                        stream.write_all(&response_bytes).await?;
                        stream.flush().await?;
                    }
                }
                HandshakeProcessResult::Completed {
                    response_bytes,
                    remaining_bytes,
                } => {
                    if !response_bytes.is_empty() {
                        stream.write_all(&response_bytes).await?;
                        stream.flush().await?;
                    }
                    return Ok(remaining_bytes);
                }
            }
        }
    }

    async fn request_connection(&mut self, app: &str) -> Result<(), RtmpError> {
        let result = self
            .session
            .request_connection(app.to_string())
            .map_err(|e| RtmpError::Protocol(format!("{e:?}")))?;
        self.dispatch(vec![result]).await?;


        self.pump_until("the server to accept the connection", |event| match event {
            ClientSessionEvent::ConnectionRequestAccepted => Some(Ok(())),
            ClientSessionEvent::ConnectionRequestRejected { description } => {
                Some(Err(RtmpError::ConnectionRejected(description.clone())))
            }
            _ => None,
        })
        .await
    }

    async fn request_publishing(&mut self, stream_key: &str) -> Result<(), RtmpError> {
        let result = self
            .session
            .request_publishing(stream_key.to_string(), PublishRequestType::Live)
            .map_err(|e| RtmpError::Protocol(format!("{e:?}")))?;
        self.dispatch(vec![result]).await?;

        self.pump_until("the server to accept the stream key", |event| match event {
            ClientSessionEvent::PublishRequestAccepted => Some(Ok(())),
            // A wrong or expired key is by far the most common setup failure,
            // so it must not surface as a generic timeout.
            ClientSessionEvent::ConnectionRequestRejected { description } => {
                Some(Err(RtmpError::PublishRejected(description.clone())))
            }
            _ => None,
        })
        .await
    }

    /// Describes the stream, before any of it is sent.
    ///
    /// This is `@setDataFrame`/`onMetaData`, and it is not decoration. A
    /// platform's ingest uses it to set up the transcode before the first
    /// picture arrives; without it YouTube accepts the connection, reports
    /// the health as excellent, counts the megabits, and shows black. A
    /// local ffmpeg acting as a server does not care -- it reads the
    /// bitstream and works it out -- which is exactly why a stream can pass
    /// every test here and still show nothing on air.
    pub async fn send_metadata(&mut self, metadata: &StreamMetadata) -> Result<(), RtmpError> {
        let result = self
            .session
            .publish_metadata(metadata)
            .map_err(|e| RtmpError::Protocol(format!("{e:?}")))?;
        self.dispatch(vec![result]).await.map(|_| ())
    }

    /// Publishes one FLV video tag body at a presentation time in milliseconds.
    ///
    /// `droppable` must be false for the sequence header and for keyframes:
    /// losing either leaves the decoder unable to start.
    pub async fn send_video(
        &mut self,
        tag: Vec<u8>,
        timestamp_ms: u32,
        droppable: bool,
    ) -> Result<(), RtmpError> {
        let result = self
            .session
            .publish_video_data(Bytes::from(tag), RtmpTimestamp::new(timestamp_ms), droppable)
            .map_err(|e| RtmpError::Protocol(format!("{e:?}")))?;
        self.dispatch(vec![result]).await.map(|_| ())
    }

    /// Publishes one FLV audio tag body.
    pub async fn send_audio(&mut self, tag: Vec<u8>, timestamp_ms: u32) -> Result<(), RtmpError> {
        let result = self
            .session
            .publish_audio_data(Bytes::from(tag), RtmpTimestamp::new(timestamp_ms), false)
            .map_err(|e| RtmpError::Protocol(format!("{e:?}")))?;
        self.dispatch(vec![result]).await.map(|_| ())
    }

    pub async fn close(&mut self) -> Result<(), RtmpError> {
        if let Ok(results) = self.session.stop_publishing() {
            let _ = self.dispatch(results).await;
        }
        self.stream.shutdown().await?;
        Ok(())
    }

    /// Writes outbound packets and surfaces events.
    async fn dispatch(
        &mut self,
        results: Vec<ClientSessionResult>,
    ) -> Result<Vec<ClientSessionEvent>, RtmpError> {
        let mut events = Vec::new();
        for result in results {
            match result {
                // Order matters and must be preserved: RTMP chunk headers are
                // compressed against the previous packet, so reordering or
                // dropping one corrupts everything after it.
                ClientSessionResult::OutboundResponse(packet) => {
                    self.stream.write_all(&packet.bytes).await?;
                }
                ClientSessionResult::RaisedEvent(event) => events.push(event),
                ClientSessionResult::UnhandleableMessageReceived(_) => {
                    tracing::debug!("ignoring an RTMP message we do not handle");
                }
            }
        }
        self.stream.flush().await?;
        Ok(events)
    }

    /// Reads until `decide` resolves an event, or the step times out.
    async fn pump_until<F>(&mut self, what: &'static str, decide: F) -> Result<(), RtmpError>
    where
        F: Fn(&ClientSessionEvent) -> Option<Result<(), RtmpError>>,
    {
        let deadline = tokio::time::Instant::now() + STEP_TIMEOUT;
        loop {
            let read = tokio::time::timeout_at(deadline, self.stream.read(&mut self.read_buf))
                .await
                .map_err(|_| RtmpError::Timeout(what))??;
            if read == 0 {
                return Err(RtmpError::Closed);
            }

            let input = self.read_buf[..read].to_vec();
            let results = self
                .session
                .handle_input(&input)
                .map_err(|e| RtmpError::Protocol(format!("{e:?}")))?;

            for event in self.dispatch(results).await? {
                if let Some(outcome) = decide(&event) {
                    return outcome;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_url_with_the_key_in_the_path() {
        let url = RtmpUrl::parse("rtmp://a.rtmp.youtube.com/live2/abcd-1234-efgh").unwrap();
        assert_eq!(url.host, "a.rtmp.youtube.com");
        assert_eq!(url.port, 1935);
        assert_eq!(url.app, "live2");
        assert_eq!(url.stream_key, "abcd-1234-efgh");
        assert_eq!(url.tc_url(), "rtmp://a.rtmp.youtube.com:1935/live2");
    }

    #[test]
    fn accepts_the_server_url_and_key_given_separately() {
        // This is how every platform dashboard actually presents them.
        let url = RtmpUrl::parse_with_key("rtmp://a.rtmp.youtube.com/live2", Some("secret-key"))
            .unwrap();
        assert_eq!(url.app, "live2");
        assert_eq!(url.stream_key, "secret-key");
    }

    #[test]
    fn an_explicit_key_overrides_one_in_the_path() {
        let url =
            RtmpUrl::parse_with_key("rtmp://host/live2/in-path", Some("explicit")).unwrap();
        assert_eq!(url.stream_key, "explicit");
    }

    #[test]
    fn keeps_slashes_inside_a_stream_key() {
        // Some platforms issue keys containing slashes; splitting on the last
        // one would silently truncate them.
        let url = RtmpUrl::parse("rtmp://host/app/key/with/slashes").unwrap();
        assert_eq!(url.app, "app");
        assert_eq!(url.stream_key, "key/with/slashes");
    }

    #[test]
    fn honours_a_non_default_port() {
        let url = RtmpUrl::parse("rtmp://localhost:19350/live/key").unwrap();
        assert_eq!(url.port, 19350);
        assert_eq!(url.host, "localhost");
    }

    #[test]
    fn rejects_urls_that_cannot_work() {
        for bad in [
            "http://host/app/key",
            "rtmp://",
            "rtmp://host",
            "rtmp://host/onlyapp",
            "rtmp://host:notaport/app/key",
        ] {
            assert!(
                RtmpUrl::parse(bad).is_err(),
                "{bad} should not parse — a bad destination must fail before going live, not during"
            );
        }
    }

    #[test]
    fn rtmps_is_rejected_clearly_rather_than_silently_downgraded() {
        let err = RtmpUrl::parse("rtmps://host/app/key").unwrap_err();
        assert!(
            err.to_string().contains("rtmps"),
            "the error should say why, got: {err}"
        );
    }
}
