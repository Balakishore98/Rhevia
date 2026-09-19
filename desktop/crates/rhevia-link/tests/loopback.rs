//! The RheviaLink vertical slice, end to end, with no phone required.
//!
//! A synthetic camera pairs through the real signaling server, negotiates
//! WebRTC, and streams H.264 to `CameraReceiver`. If this passes, the whole
//! path works: pairing, SDP relay, ICE, DTLS-SRTP, and RTP arriving as
//! countable media.
//!
//! The peer connections use host candidates only. Pointing them at a TURN
//! server that does not resolve would add DNS timeouts and test the network,
//! not the code; relay behaviour needs a real deployed relay to verify.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{info, next, start_server};
use rhevia_link::{CameraReceiver, LinkEvent, MediaEvent, SignalPayload, SignalingClient};

use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::media_engine::MIME_TYPE_H264;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters,
    RtpCodecKind,
};
use webrtc::rtp_transceiver::RtpSender;
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::peer_connection::{
    register_default_interceptors, MediaEngine, PeerConnection, PeerConnectionBuilder,
    PeerConnectionEventHandler, RTCConfigurationBuilder, RTCIceCandidateInit,
    RTCIceGatheringState, RTCPeerConnectionIceEvent,
};

const H264_PAYLOAD_TYPE: u8 = 102;

fn h264_codec() -> RTCRtpCodecParameters {
    RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_H264.to_owned(),
            clock_rate: 90_000,
            channels: 0,
            // Constrained Baseline 3.1, which is what phone hardware encoders
            // emit and what every browser will accept.
            sdp_fmtp_line:
                "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".to_owned(),
            rtcp_feedback: vec![],
        },
        payload_type: H264_PAYLOAD_TYPE,
        ..Default::default()
    }
}

/// Collects the synthetic camera's local ICE candidates for relaying.
struct CameraHandler {
    candidates: tokio::sync::mpsc::UnboundedSender<SignalPayload>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for CameraHandler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        if let Ok(init) = event.candidate.to_json() {
            let _ = self.candidates.send(SignalPayload::Candidate {
                candidate: init.candidate,
                sdp_mid: init.sdp_mid,
                sdp_mline_index: init.sdp_mline_index,
            });
        }
    }

    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.candidates.send(SignalPayload::CandidateEnd);
        }
    }
}

/// One H.264 access unit. The bytes need not decode — they must only survive
/// packetisation and arrive, which is what this test measures.
fn fake_access_unit(seq: u32) -> Vec<u8> {
    let mut nal = vec![
        0x00, 0x00, 0x00, 0x01, // Annex-B start code
        0x65, // IDR slice header
    ];
    // ~8 KB, so each frame spans several RTP packets and exercises
    // fragmentation rather than trivially fitting in one.
    nal.extend(std::iter::repeat((seq % 251) as u8).take(8_000));
    nal
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_camera_pairs_negotiates_and_streams_h264_to_the_receiver() {
    let Some(server) = start_server().await else {
        return;
    };

    // ---- desktop side ------------------------------------------------------
    let mut desktop_sig =
        SignalingClient::connect_receiver(&server.url(), info("Studio PC", "win32"))
            .await
            .expect("desktop signaling");
    desktop_sig.request_code().unwrap();
    let LinkEvent::CodeReady { code, .. } = next(&mut desktop_sig, "the pairing code").await else {
        panic!("expected a pairing code");
    };

    let receiver = Arc::new(
        // Both ends on loopback: ICE will not pair a 127.0.0.1 candidate with
        // a wildcard-bound peer that never gathered one.
        CameraReceiver::new_bound(&[], vec!["127.0.0.1:0".to_string()])
            .await
            .expect("build the receiver"),
    );

    // ---- synthetic camera --------------------------------------------------
    let mut camera_sig = SignalingClient::connect_sender(&server.url(), info("Test Cam", "android"))
        .await
        .expect("camera signaling");
    camera_sig.join(&code).unwrap();
    next(&mut camera_sig, "pairing").await;
    next(&mut desktop_sig, "pairing").await;

    let (cand_tx, mut cand_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut media = MediaEngine::default();
    media.register_default_codecs().unwrap();
    let registry = register_default_interceptors(rtc::interceptor::Registry::new(), &mut media)
        .expect("interceptors");

    let camera_pc = PeerConnectionBuilder::new()
        .with_configuration(RTCConfigurationBuilder::default().build())
        .with_media_engine(media)
        .with_interceptor_registry(registry)
        .with_handler(Arc::new(CameraHandler { candidates: cand_tx })
            as Arc<dyn PeerConnectionEventHandler>)
        .with_udp_addrs(vec!["127.0.0.1:0".to_string()])
        .build()
        .await
        .expect("camera peer connection");

    let ssrc = rand_ssrc();
    let track = Arc::new(
        TrackLocalStaticSample::new(MediaStreamTrack::new(
            "rhevia-test-stream".to_string(),
            "rhevia-test-video".to_string(),
            "Test Cam".to_string(),
            RtpCodecKind::Video,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters {
                    ssrc: Some(ssrc),
                    ..Default::default()
                },
                codec: h264_codec().rtp_codec.clone(),
                ..Default::default()
            }],
        ))
        .expect("build the local track"),
    );
    let sender = camera_pc
        .add_track(Arc::clone(&track) as Arc<dyn TrackLocal>)
        .await
        .expect("add the track");

    // ---- negotiation, relayed through the real signaling server ------------
    let offer = camera_pc.create_offer(None).await.expect("create offer");
    camera_pc
        .set_local_description(offer.clone())
        .await
        .expect("set local offer");
    camera_sig
        .signal(SignalPayload::Offer { sdp: offer.sdp })
        .unwrap();

    let LinkEvent::Signal(SignalPayload::Offer { sdp }) =
        next(&mut desktop_sig, "the offer").await
    else {
        panic!("expected the offer to arrive at the desktop");
    };
    eprintln!("[sdp] offer reached the desktop, {} bytes", sdp.len());
    let answer = receiver.accept_offer(&sdp).await.expect("answer the offer");
    eprintln!("[sdp] answer produced, {} bytes", answer.len());
    eprintln!("[sdp] answer m-lines: {:?}",
        answer.lines().filter(|l| l.starts_with("m=")).collect::<Vec<_>>());
    desktop_sig
        .signal(SignalPayload::Answer { sdp: answer })
        .unwrap();

    let LinkEvent::Signal(SignalPayload::Answer { sdp }) =
        next(&mut camera_sig, "the answer").await
    else {
        panic!("expected the answer to arrive at the camera");
    };
    camera_pc
        .set_remote_description(
            rtc::peer_connection::sdp::RTCSessionDescription::answer(sdp).unwrap(),
        )
        .await
        .expect("set remote answer");

    // ---- trickle ICE both ways ---------------------------------------------
    let camera_sig = Arc::new(camera_sig);
    {
        let sig = Arc::clone(&camera_sig);
        tokio::spawn(async move {
            while let Some(payload) = cand_rx.recv().await {
                eprintln!("[ice] camera -> desktop: {payload:?}");
                let _ = sig.signal(payload);
            }
        });
    }

    let receiver_for_ice = Arc::clone(&receiver);
    let desktop_sig = Arc::new(tokio::sync::Mutex::new(desktop_sig));
    {
        let sig = Arc::clone(&desktop_sig);
        let rx = Arc::clone(&receiver_for_ice);
        tokio::spawn(async move {
            loop {
                let event = { sig.lock().await.next_event().await };
                match event {
                    Some(LinkEvent::Signal(payload)) => {
                        let _ = rx.add_remote_candidate(&payload).await;
                    }
                    Some(_) => continue,
                    None => break,
                }
            }
        });
    }

    let camera_pc = Arc::new(camera_pc);
    {
        // The desktop's own candidates go back to the camera.
        let rx = Arc::clone(&receiver);
        let pc = Arc::clone(&camera_pc);
        tokio::spawn(async move {
            while let Some(payload) = rx.next_local_candidate().await {
                eprintln!("[ice] desktop -> camera: {payload:?}");
                if let SignalPayload::Candidate {
                    candidate,
                    sdp_mid,
                    sdp_mline_index,
                } = payload
                {
                    let _ = pc
                        .add_ice_candidate(RTCIceCandidateInit {
                            candidate,
                            sdp_mid,
                            sdp_mline_index,
                            ..Default::default()
                        })
                        .await;
                }
            }
        });
    }

    // ---- stream ------------------------------------------------------------
    let payload_type = negotiated_payload_type(&sender).await;
    eprintln!("[rtp] negotiated payload type = {payload_type}");
    {
        let track = Arc::clone(&track);
        tokio::spawn(async move {
            for seq in 0..300u32 {
                let _ = track
                    .sample_writer(ssrc, payload_type)
                    .write_sample(&Sample {
                        data: fake_access_unit(seq).into(),
                        duration: Duration::from_millis(33),
                        ..Default::default()
                    })
                    .await;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
    }

    // ---- assert media actually arrived -------------------------------------
    let mut saw_track = false;
    let mut total_packets = 0u64;
    let mut total_bytes = 0u64;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline {
        let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_secs(5), receiver.next_media_event()).await
        else {
            break;
        };

        eprintln!("[media] {event:?}");
        match event {
            MediaEvent::TrackStarted { kind, codec } => {
                if kind.to_lowercase().contains("video") {
                    assert!(
                        codec.to_lowercase().contains("h264"),
                        "expected the negotiated codec to be H.264, got {codec:?}"
                    );
                    saw_track = true;
                }
            }
            MediaEvent::Throughput { packets, bytes, .. } => {
                total_packets = packets;
                total_bytes = bytes;
                // Enough traffic to prove this is real RTP, not one stray packet.
                if total_packets > 50 {
                    break;
                }
            }
            MediaEvent::ConnectionState(state) => {
                assert_ne!(state.to_lowercase(), "failed", "the peer connection failed");
            }
            _ => {}
        }
    }

    assert!(saw_track, "the receiver never saw a video track start");
    assert!(
        total_packets > 50,
        "expected a real RTP flow, got {total_packets} packets / {total_bytes} bytes"
    );

    receiver.close().await.ok();
    camera_pc.close().await.ok();
}

async fn negotiated_payload_type(sender: &Arc<dyn RtpSender>) -> u8 {
    // write_sample stamps this on every packet, and rtc rejects a payload type
    // that was not negotiated for this sender.
    sender
        .get_parameters()
        .await
        .ok()
        .and_then(|p| p.rtp_parameters.codecs.first().map(|c| c.payload_type))
        .unwrap_or(H264_PAYLOAD_TYPE)
}

fn rand_ssrc() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    nanos | 1
}
