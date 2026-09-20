//! SRT delivery.
//!
//! SRT is what a contribution feed uses when the path is not a clean one: it
//! recovers lost packets within a latency budget you choose, so a stream over
//! a congested link arrives intact rather than breaking up. That is the reason
//! to have it alongside RTMP rather than instead of it — RTMP is what the big
//! platforms ingest, SRT is what gets the signal to them.
//!
//! The payload is a transport stream, built by [`crate::mpegts`].

use std::time::Instant;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use srt_tokio::{SrtSocket, SrtListener};

use crate::mpegts::{self, TsMuxer, CLOCK_HZ};

/// SRT sends a fixed payload of seven transport packets.
///
/// This is not arbitrary: 7 × 188 is 1316, the largest multiple of the packet
/// size that fits an Ethernet frame once the headers are counted. Sending
/// anything else works but fragments, which is exactly what SRT exists to
/// avoid.
pub const PAYLOAD_SIZE: usize = 7 * mpegts::PACKET_SIZE;

/// Default latency budget. SRT trades this delay for the ability to ask for
/// lost packets again; 120 ms covers most real links.
pub const DEFAULT_LATENCY_MS: u64 = 120;

#[derive(Debug, thiserror::Error)]
pub enum SrtError {
    #[error("{0} is not an SRT address")]
    BadUrl(String),
    #[error("could not reach {0}: {1}")]
    Connect(String, String),
    #[error("the SRT connection failed: {0}")]
    Send(String),
}

/// Which end opens the connection.
///
/// Caller dials out and is what a contribution feed usually wants; listener
/// waits to be dialled, which is what a receiver wants. They are symmetrical
/// and the choice is about which side is reachable, not about direction of
/// video.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SrtMode {
    #[default]
    Caller,
    Listener,
}

/// A parsed `srt://` address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrtUrl {
    pub host: String,
    pub port: u16,
    pub mode: SrtMode,
    /// Identifies the stream to the far end, which is how one receiver
    /// distinguishes several senders.
    pub stream_id: Option<String>,
    pub passphrase: Option<String>,
    pub latency_ms: u64,
}

impl SrtUrl {
    /// Parses `srt://host:port?mode=caller&latency=120&streamid=…`.
    ///
    /// The query parameters are the ones every other SRT tool uses, spelled
    /// the same way, so an address copied from elsewhere works here.
    pub fn parse(input: &str) -> Result<Self, SrtError> {
        let bad = || SrtError::BadUrl(input.to_string());

        let rest = input.strip_prefix("srt://").ok_or_else(bad)?;
        let (authority, query) = match rest.split_once('?') {
            Some((a, q)) => (a, Some(q)),
            None => (rest, None),
        };
        let authority = authority.trim_end_matches('/');

        // rsplit, not split: an IPv6 literal is full of colons and only the
        // last one separates the port.
        let (host, port) = authority.rsplit_once(':').ok_or_else(bad)?;
        let host = host.trim_matches(|c| c == '[' || c == ']');
        if host.is_empty() {
            return Err(bad());
        }
        let port: u16 = port.parse().map_err(|_| bad())?;
        if port == 0 {
            return Err(bad());
        }

        let mut url = Self {
            host: host.to_string(),
            port,
            mode: SrtMode::Caller,
            stream_id: None,
            passphrase: None,
            latency_ms: DEFAULT_LATENCY_MS,
        };

        for pair in query.unwrap_or("").split('&').filter(|s| !s.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            match key.to_ascii_lowercase().as_str() {
                "mode" => {
                    url.mode = match value.to_ascii_lowercase().as_str() {
                        "listener" => SrtMode::Listener,
                        // "rendezvous" is a real SRT mode this does not
                        // implement; treating it as caller would connect to
                        // the wrong thing, so it is refused.
                        "caller" => SrtMode::Caller,
                        _ => return Err(bad()),
                    }
                }
                "latency" | "rcvlatency" | "peerlatency" => {
                    if let Ok(ms) = value.parse::<u64>() {
                        url.latency_ms = ms;
                    }
                }
                "streamid" => {
                    if !value.is_empty() {
                        url.stream_id = Some(value.to_string());
                    }
                }
                "passphrase" => {
                    if !value.is_empty() {
                        url.passphrase = Some(value.to_string());
                    }
                }
                // Unknown parameters are ignored rather than refused: SRT has
                // a long tail of them and most do not apply to sending.
                _ => {}
            }
        }

        Ok(url)
    }

    pub fn address(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// The address with the passphrase removed, for logs and the interface.
    ///
    /// A passphrase in a status line ends up in a screenshot.
    pub fn redacted(&self) -> String {
        let mut out = format!("srt://{}", self.address());
        let mut first = true;
        let mut add = |part: String| {
            out.push(if first { '?' } else { '&' });
            out.push_str(&part);
            first = false;
        };
        add(format!(
            "mode={}",
            match self.mode {
                SrtMode::Caller => "caller",
                SrtMode::Listener => "listener",
            }
        ));
        add(format!("latency={}", self.latency_ms));
        if let Some(id) = &self.stream_id {
            add(format!("streamid={id}"));
        }
        if self.passphrase.is_some() {
            add("passphrase=***".to_string());
        }
        out
    }
}

/// Sends a transport stream over SRT.
pub struct SrtPublisher {
    socket: SrtSocket,
    muxer: TsMuxer,
    /// Transport packets waiting to make up a full SRT payload.
    pending: Vec<u8>,
    bytes_sent: u64,
}

impl SrtPublisher {
    /// Opens the connection described by `url`.
    ///
    /// In listener mode this waits for the far end to dial in, which is the
    /// point of that mode, so it can block for as long as it takes.
    pub async fn connect(url: &SrtUrl, has_audio: bool) -> Result<Self, SrtError> {
        let latency = std::time::Duration::from_millis(url.latency_ms);

        let socket = match url.mode {
            SrtMode::Caller => {
                let mut builder = SrtSocket::builder().latency(latency);
                if let Some(passphrase) = &url.passphrase {
                    builder = builder
                        .encryption(0, passphrase.clone());
                }
                builder
                    .call(url.address().as_str(), url.stream_id.as_deref())
                    .await
                    .map_err(|e| SrtError::Connect(url.redacted(), e.to_string()))?
            }
            SrtMode::Listener => {
                let mut builder = SrtListener::builder().latency(latency);
                if let Some(passphrase) = &url.passphrase {
                    builder = builder.encryption(0, passphrase.clone());
                }
                let (_listener, mut incoming) = builder
                    .bind(url.address().as_str())
                    .await
                    .map_err(|e| SrtError::Connect(url.redacted(), e.to_string()))?;

                let request = incoming
                    .incoming()
                    .next()
                    .await
                    .ok_or_else(|| {
                        SrtError::Connect(url.redacted(), "nothing connected".to_string())
                    })?;
                request
                    .accept(None)
                    .await
                    .map_err(|e| SrtError::Connect(url.redacted(), e.to_string()))?
            }
        };

        Ok(Self {
            socket,
            muxer: TsMuxer::new(has_audio),
            pending: Vec::with_capacity(PAYLOAD_SIZE * 2),
            bytes_sent: 0,
        })
    }

    /// Sends one access unit of H.264, given as Annex-B.
    pub async fn send_video(
        &mut self,
        annexb: &[u8],
        pts: u64,
        dts: u64,
        keyframe: bool,
    ) -> Result<(), SrtError> {
        self.muxer.video(annexb, pts, dts, keyframe, &mut self.pending);
        self.flush_full().await
    }

    /// Sends one AAC frame, which must already carry an ADTS header.
    pub async fn send_audio(&mut self, adts: &[u8], pts: u64) -> Result<(), SrtError> {
        self.muxer.audio(adts, pts, &mut self.pending);
        self.flush_full().await
    }

    /// Timestamp in the 90 kHz units the muxer wants, from a frame number.
    pub fn timestamp(frame: u64, fps: u32) -> u64 {
        frame * CLOCK_HZ / fps.max(1) as u64
    }

    pub fn bytes_sent(&self) -> u64 {
        self.bytes_sent
    }

    /// Sends every complete payload, keeping any remainder.
    ///
    /// Partial payloads are held rather than padded: padding would insert
    /// packets the demuxer has to skip on every single send.
    async fn flush_full(&mut self) -> Result<(), SrtError> {
        while self.pending.len() >= PAYLOAD_SIZE {
            let chunk: Vec<u8> = self.pending.drain(..PAYLOAD_SIZE).collect();
            self.bytes_sent += chunk.len() as u64;
            self.socket
                .send((Instant::now(), Bytes::from(chunk)))
                .await
                .map_err(|e| SrtError::Send(e.to_string()))?;
        }
        Ok(())
    }

    /// Sends whatever is left and closes the connection.
    pub async fn close(&mut self) -> Result<(), SrtError> {
        if !self.pending.is_empty() {
            // The tail is padded to a whole payload here, at the very end,
            // where one skipped packet costs nothing.
            let mut tail: Vec<u8> = std::mem::take(&mut self.pending);
            while tail.len() % PAYLOAD_SIZE != 0 {
                tail.extend_from_slice(&null_packet());
            }
            self.bytes_sent += tail.len() as u64;
            for chunk in tail.chunks(PAYLOAD_SIZE) {
                self.socket
                    .send((Instant::now(), Bytes::copy_from_slice(chunk)))
                    .await
                    .map_err(|e| SrtError::Send(e.to_string()))?;
            }
        }
        self.socket.close().await.map_err(|e| SrtError::Send(e.to_string()))
    }
}

/// A null transport packet, which every demuxer discards.
///
/// PID 0x1fff is reserved for exactly this: filling space without saying
/// anything.
fn null_packet() -> [u8; mpegts::PACKET_SIZE] {
    let mut packet = [0xffu8; mpegts::PACKET_SIZE];
    packet[0] = 0x47;
    packet[1] = 0x1f;
    packet[2] = 0xff;
    packet[3] = 0x10;
    packet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_address_parses_with_sensible_defaults() {
        let url = SrtUrl::parse("srt://198.51.100.7:9000").expect("should parse");
        assert_eq!(url.host, "198.51.100.7");
        assert_eq!(url.port, 9000);
        assert_eq!(url.mode, SrtMode::Caller);
        assert_eq!(url.latency_ms, DEFAULT_LATENCY_MS);
        assert!(url.stream_id.is_none());
    }

    #[test]
    fn every_parameter_is_read() {
        let url = SrtUrl::parse(
            "srt://example.net:1234?mode=listener&latency=400&streamid=studio-a&passphrase=hunter2",
        )
        .expect("should parse");

        assert_eq!(url.host, "example.net");
        assert_eq!(url.port, 1234);
        assert_eq!(url.mode, SrtMode::Listener);
        assert_eq!(url.latency_ms, 400);
        assert_eq!(url.stream_id.as_deref(), Some("studio-a"));
        assert_eq!(url.passphrase.as_deref(), Some("hunter2"));
    }

    #[test]
    fn parameter_names_are_not_case_sensitive() {
        // Addresses get copied out of other tools, which spell these various
        // ways.
        let url = SrtUrl::parse("srt://host:5000?MODE=Listener&Latency=250").expect("should parse");
        assert_eq!(url.mode, SrtMode::Listener);
        assert_eq!(url.latency_ms, 250);
    }

    #[test]
    fn the_receiver_latency_spelling_is_accepted_too() {
        let url = SrtUrl::parse("srt://host:5000?rcvlatency=300").expect("should parse");
        assert_eq!(url.latency_ms, 300);
    }

    #[test]
    fn an_ipv6_literal_keeps_its_colons() {
        // Splitting on the first colon rather than the last turns an IPv6
        // address into nonsense.
        let url = SrtUrl::parse("srt://[2001:db8::1]:9000").expect("should parse");
        assert_eq!(url.host, "2001:db8::1");
        assert_eq!(url.port, 9000);
        assert_eq!(url.address(), "[2001:db8::1]:9000");
    }

    #[test]
    fn an_address_that_is_not_srt_is_refused() {
        for bad in [
            "rtmp://a.rtmp.youtube.com/live2",
            "http://example.com:9000",
            "example.com:9000",
            "",
        ] {
            assert!(SrtUrl::parse(bad).is_err(), "{bad} should not have parsed");
        }
    }

    #[test]
    fn a_missing_or_impossible_port_is_refused() {
        // Connecting to port 0 fails in a way that is hard to read; refusing
        // it here says what is actually wrong.
        assert!(SrtUrl::parse("srt://example.com").is_err());
        assert!(SrtUrl::parse("srt://example.com:0").is_err());
        assert!(SrtUrl::parse("srt://example.com:notaport").is_err());
        assert!(SrtUrl::parse("srt://:9000").is_err());
    }

    #[test]
    fn an_unsupported_mode_is_refused_rather_than_assumed() {
        // Rendezvous is a real mode this does not implement. Quietly treating
        // it as caller would connect to the wrong place and look like a
        // network fault.
        assert!(SrtUrl::parse("srt://host:9000?mode=rendezvous").is_err());
    }

    #[test]
    fn unknown_parameters_are_ignored() {
        // SRT has a long tail of options, most irrelevant to sending. An
        // address carrying one should still work.
        let url = SrtUrl::parse("srt://host:9000?pbkeylen=16&tlpktdrop=1&latency=200")
            .expect("should parse");
        assert_eq!(url.latency_ms, 200);
    }

    #[test]
    fn a_trailing_slash_is_tolerated() {
        let url = SrtUrl::parse("srt://host:9000/").expect("should parse");
        assert_eq!(url.port, 9000);
    }

    #[test]
    fn the_passphrase_never_appears_in_the_redacted_form() {
        let url = SrtUrl::parse("srt://host:9000?passphrase=verysecret&streamid=a")
            .expect("should parse");
        let shown = url.redacted();
        assert!(!shown.contains("verysecret"), "the passphrase leaked: {shown}");
        assert!(shown.contains("passphrase=***"));
        // The rest stays, because it is what makes the line useful.
        assert!(shown.contains("host:9000") && shown.contains("streamid=a"));
    }

    #[test]
    fn a_payload_is_a_whole_number_of_transport_packets() {
        // SRT fragments anything larger, which is what it exists to avoid.
        assert_eq!(PAYLOAD_SIZE % mpegts::PACKET_SIZE, 0);
        assert_eq!(PAYLOAD_SIZE, 1316);
    }

    #[test]
    fn a_null_packet_is_one_a_demuxer_discards() {
        let packet = null_packet();
        assert_eq!(packet[0], 0x47);
        let pid = (((packet[1] & 0x1f) as u16) << 8) | packet[2] as u16;
        assert_eq!(pid, 0x1fff, "padding must use the reserved null PID");
    }

    #[test]
    fn timestamps_run_at_ninety_kilohertz() {
        assert_eq!(SrtPublisher::timestamp(0, 30), 0);
        assert_eq!(SrtPublisher::timestamp(30, 30), CLOCK_HZ);
        assert_eq!(SrtPublisher::timestamp(60, 30), CLOCK_HZ * 2);
        // A silly frame rate must not divide by zero.
        assert_eq!(SrtPublisher::timestamp(1, 0), CLOCK_HZ);
    }
}
