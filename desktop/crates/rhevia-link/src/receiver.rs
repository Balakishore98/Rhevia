//! WebRTC receive side: turns a paired phone into a media source.
//!
//! This owns the peer connection and nothing else. Negotiation messages go in,
//! media events come out, so the same code serves the desktop app, the
//! headless test harness and a CLI ingest tool.
//!
//! Decoding is deliberately not here. This layer proves bytes are arriving and
//! surfaces encoded RTP; NVDEC and the GPU pipeline sit behind it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rtc::interceptor::Registry;
use rtc::rtp_transceiver::rtp_sender::RtpCodecKind;
use rtc::rtp_transceiver::{RTCRtpTransceiverDirection, RTCRtpTransceiverInit};
use tokio::sync::mpsc;
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{
    register_default_interceptors, MediaEngine, PeerConnection, PeerConnectionBuilder,
    PeerConnectionEventHandler, RTCConfigurationBuilder, RTCIceCandidateInit,
    RTCIceConnectionState, RTCIceGatheringState, RTCIceServer, RTCPeerConnectionIceEvent,
    RTCPeerConnectionState, RTCSessionDescription,
};

use crate::protocol::{IceServer, SignalPayload};

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("webrtc failed: {0}")]
    Webrtc(String),
    #[error("the offer from the camera was not usable: {0}")]
    BadOffer(String),
}

impl From<webrtc::error::Error> for MediaError {
    fn from(e: webrtc::error::Error) -> Self {
        MediaError::Webrtc(e.to_string())
    }
}

/// What the ingest layer reports upward.
///
/// Local ICE candidates are deliberately *not* here: they are signaling
/// traffic, and they flow on their own channel so the caller can pump them to
/// the signaling client while the UI consumes media events independently.
#[derive(Debug, Clone)]
pub enum MediaEvent {
    /// A media track started arriving.
    TrackStarted { kind: String, codec: String },
    /// Periodic throughput, for the operator's connection indicator.
    Throughput {
        kind: String,
        packets: u64,
        bytes: u64,
        bitrate_kbps: u64,
    },
    /// Peer connection state. `Failed` is what the UI shows as a dead camera.
    ConnectionState(String),
    /// ICE state, finer grained than the above and useful in diagnostics.
    IceState(String),
    /// The camera stopped sending this track.
    TrackEnded { kind: String },
}

struct Handler {
    events: mpsc::UnboundedSender<MediaEvent>,
    candidates: mpsc::UnboundedSender<SignalPayload>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        match event.candidate.to_json() {
            Ok(init) => {
                let _ = self.candidates.send(SignalPayload::Candidate {
                    candidate: init.candidate,
                    sdp_mid: init.sdp_mid,
                    sdp_mline_index: init.sdp_mline_index,
                });
            }
            Err(e) => tracing::warn!(error = %e, "could not serialise a local candidate"),
        }
    }

    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        // The peer needs an explicit end-of-candidates or it keeps waiting for
        // a better path that is never coming.
        if state == RTCIceGatheringState::Complete {
            let _ = self.candidates.send(SignalPayload::CandidateEnd);
        }
    }

    async fn on_ice_connection_state_change(&self, state: RTCIceConnectionState) {
        let _ = self.events.send(MediaEvent::IceState(state.to_string()));
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        let _ = self
            .events
            .send(MediaEvent::ConnectionState(state.to_string()));
    }

    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let kind = track.kind().await.to_string();
        let codec = match track.ssrcs().await.first() {
            Some(&ssrc) => track
                .codec(ssrc)
                .await
                .map(|c| c.mime_type)
                .unwrap_or_else(|| "unknown".into()),
            None => "unknown".into(),
        };

        let _ = self.events.send(MediaEvent::TrackStarted {
            kind: kind.clone(),
            codec,
        });
        tokio::spawn(drain_track(track, kind, self.events.clone()));
    }
}

pub struct CameraReceiver {
    pc: Arc<dyn PeerConnection>,
    // Mutexed receivers rather than `&mut self`: the desktop shares one
    // receiver between a signaling pump and the UI, and those run concurrently.
    media: tokio::sync::Mutex<mpsc::UnboundedReceiver<MediaEvent>>,
    candidates: tokio::sync::Mutex<mpsc::UnboundedReceiver<SignalPayload>>,
}

impl CameraReceiver {
    /// Builds a receive-only peer connection from the ICE servers signaling
    /// handed us, minted TURN credentials included.
    ///
    /// Binds every interface. Use [`Self::new_bound`] to restrict that.
    pub async fn new(ice_servers: &[IceServer]) -> Result<Self, MediaError> {
        Self::new_bound(ice_servers, vec!["0.0.0.0:0".to_string()]).await
    }

    /// As [`Self::new`], but binds specific local addresses.
    ///
    /// Needed because ICE only pairs candidates it can actually route between:
    /// a peer on loopback cannot reach a wildcard-bound peer that never
    /// gathered a loopback candidate. Tests bind both ends to 127.0.0.1; a
    /// multi-homed studio machine can pin the production interface here.
    pub async fn new_bound(
        ice_servers: &[IceServer],
        udp_addrs: Vec<String>,
    ) -> Result<Self, MediaError> {
        let mut media = MediaEngine::default();
        // H.264, VP8, VP9, Opus. The phone picks from these, and H.264 is what
        // hardware encoders on both mobile platforms emit.
        media.register_default_codecs()?;

        // NACK and RTCP reports. Without these a single lost packet corrupts
        // the picture until the next keyframe, which on a mobile link means
        // constant visible damage.
        let registry = register_default_interceptors(Registry::new(), &mut media)?;

        let config = RTCConfigurationBuilder::default()
            .with_ice_servers(
                ice_servers
                    .iter()
                    .map(|s| RTCIceServer {
                        urls: s.urls.clone(),
                        username: s.username.clone().unwrap_or_default(),
                        credential: s.credential.clone().unwrap_or_default(),
                    })
                    .collect(),
            )
            .build();

        let (media_tx, media_rx) = mpsc::unbounded_channel();
        let (cand_tx, cand_rx) = mpsc::unbounded_channel();
        let handler = Arc::new(Handler {
            events: media_tx,
            candidates: cand_tx,
        });

        let pc = PeerConnectionBuilder::new()
            .with_configuration(config)
            .with_media_engine(media)
            .with_interceptor_registry(registry)
            .with_handler(handler as Arc<dyn PeerConnectionEventHandler>)
            .with_udp_addrs(udp_addrs)
            .build()
            .await?;

        // Declare recvonly explicitly rather than letting the answer imply it:
        // a camera input never sends back, and saying so keeps the SDP honest.
        let recvonly = || {
            Some(RTCRtpTransceiverInit {
                direction: RTCRtpTransceiverDirection::Recvonly,
                ..Default::default()
            })
        };
        pc.add_transceiver_from_kind(RtpCodecKind::Video, recvonly())
            .await?;
        pc.add_transceiver_from_kind(RtpCodecKind::Audio, recvonly())
            .await?;

        Ok(Self {
            pc: Arc::new(pc),
            media: tokio::sync::Mutex::new(media_rx),
            candidates: tokio::sync::Mutex::new(cand_rx),
        })
    }

    /// Consumes the camera's offer and returns our answer SDP to relay back.
    pub async fn accept_offer(&self, sdp: &str) -> Result<String, MediaError> {
        let offer = RTCSessionDescription::offer(sdp.to_string())
            .map_err(|e| MediaError::BadOffer(e.to_string()))?;
        self.pc.set_remote_description(offer).await?;

        let answer = self.pc.create_answer(None).await?;
        let sdp = answer.sdp.clone();
        self.pc.set_local_description(answer).await?;
        Ok(sdp)
    }

    /// Adds a remote ICE candidate received over signaling.
    pub async fn add_remote_candidate(&self, payload: &SignalPayload) -> Result<(), MediaError> {
        let SignalPayload::Candidate {
            candidate,
            sdp_mid,
            sdp_mline_index,
        } = payload
        else {
            // candidate-end needs no action here: the sender's gathering state
            // is its own concern.
            return Ok(());
        };

        self.pc
            .add_ice_candidate(RTCIceCandidateInit {
                candidate: candidate.clone(),
                sdp_mid: sdp_mid.clone(),
                sdp_mline_index: *sdp_mline_index,
                // `url` is meaningful only for *local* srflx/relay candidates,
                // and this is a remote one, so it stays None.
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    /// Next media event. Drives the operator's picture and health indicators.
    pub async fn next_media_event(&self) -> Option<MediaEvent> {
        self.media.lock().await.recv().await
    }

    /// Next local ICE candidate to relay to the camera over signaling.
    /// Returns `CandidateEnd` once gathering completes.
    pub async fn next_local_candidate(&self) -> Option<SignalPayload> {
        self.candidates.lock().await.recv().await
    }

    pub async fn close(&self) -> Result<(), MediaError> {
        self.pc.close().await?;
        Ok(())
    }
}

/// Reads RTP until the track ends, reporting throughput as it goes.
///
/// Draining is not optional: an unread track stalls its interceptor chain, so
/// even a receiver that discards media has to consume it.
async fn drain_track(
    track: Arc<dyn TrackRemote>,
    kind: String,
    tx: mpsc::UnboundedSender<MediaEvent>,
) {
    let mut packets = 0u64;
    let mut bytes = 0u64;
    let mut window_bytes = 0u64;
    let mut last_report = Instant::now();

    while let Some(event) = track.poll().await {
        if let TrackRemoteEvent::OnRtpPacket(packet) = event {
            packets += 1;
            let len = packet.payload.len() as u64;
            bytes += len;
            window_bytes += len;

            // Reporting is driven off arriving packets rather than a timer, so
            // a silent track costs nothing and a stalled one simply stops
            // reporting — which is itself the signal the UI needs.
            let elapsed = last_report.elapsed();
            if elapsed >= Duration::from_secs(1) {
                let bitrate_kbps = (window_bytes * 8) / elapsed.as_millis().max(1) as u64;
                if tx
                    .send(MediaEvent::Throughput {
                        kind: kind.clone(),
                        packets,
                        bytes,
                        bitrate_kbps,
                    })
                    .is_err()
                {
                    return; // nobody is listening any more
                }
                window_bytes = 0;
                last_report = Instant::now();
            }
        }
    }

    let _ = tx.send(MediaEvent::TrackEnded { kind });
}
