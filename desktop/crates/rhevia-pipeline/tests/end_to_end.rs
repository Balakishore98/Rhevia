//! The whole product, end to end, with nothing mocked.
//!
//! A camera pairs by six-digit code through the real signaling server, sends
//! real H.264 over WebRTC, and Rhevia republishes it to a real RTMP server
//! which an independent decoder then verifies.
//!
//!     camera → WebRTC → depacketise → FLV → RTMP → ffmpeg → ffprobe
//!
//! Every seam in that chain has broken at least once during development, and
//! each one is invisible from either side alone: a wrong depacketiser emits
//! frames the muxer accepts and no decoder can read.

mod common;

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use common::{free_port, have, info, next, start_server, workdir, Killer};
use rhevia_link::{CameraReceiver, LinkEvent, SignalPayload, SignalingClient};
use rhevia_output::{RtmpPublisher, RtmpUrl};
use rhevia_pipeline::passthrough;

use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::media_engine::MIME_TYPE_H264;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind,
};
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::peer_connection::{
    register_default_interceptors, MediaEngine, PeerConnection, PeerConnectionBuilder,
    PeerConnectionEventHandler, RTCConfigurationBuilder, RTCIceCandidateInit,
    RTCIceGatheringState, RTCPeerConnectionIceEvent,
};

const FPS: u32 = 30;
const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const SECONDS: u32 = 3;
const H264_PAYLOAD_TYPE: u8 = 102;

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

fn make_fixture(path: &std::path::Path) {
    let status = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
            &format!("testsrc=size={WIDTH}x{HEIGHT}:rate={FPS}"),
            "-t", &SECONDS.to_string(),
            "-c:v", "libx264", "-preset", "ultrafast",
            "-g", &FPS.to_string(), "-pix_fmt", "yuv420p",
            "-f", "h264", "-y",
        ])
        .arg(path)
        .status()
        .expect("ffmpeg should run");
    assert!(status.success(), "could not build the H.264 fixture");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paired_camera_is_republished_as_a_live_rtmp_stream() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("SKIP: ffmpeg/ffprobe not on PATH");
        return;
    }
    let Some(server) = start_server().await else {
        return;
    };

    let dir = workdir();
    let fixture = dir.join("source.h264");
    let received = dir.join("relayed.flv");
    let _ = std::fs::remove_file(&received);
    make_fixture(&fixture);

    // ---- Rhevia: pair and prepare to receive -------------------------------
    let desktop_sig = Arc::new(
        SignalingClient::connect_receiver(&server.url(), info("Rhevia", "win32"))
            .await
            .expect("desktop signaling"),
    );
    let receiver = Arc::new(
        CameraReceiver::new_bound(&[], vec!["127.0.0.1:0".to_string()])
            .await
            .expect("receiver"),
    );
    desktop_sig.request_code().unwrap();
    let LinkEvent::CodeReady { code, .. } = next(&desktop_sig, "the pairing code").await else {
        panic!("expected a pairing code");
    };

    // ---- the camera --------------------------------------------------------
    let camera_sig = Arc::new(
        SignalingClient::connect_sender(&server.url(), info("Test Cam", "android"))
            .await
            .expect("camera signaling"),
    );
    camera_sig.join(&code).unwrap();
    next(&camera_sig, "pairing").await;
    next(&desktop_sig, "pairing").await;

    let (cand_tx, mut cand_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut media = MediaEngine::default();
    media.register_default_codecs().unwrap();
    let registry =
        register_default_interceptors(rtc::interceptor::Registry::new(), &mut media).unwrap();

    let camera_pc = Arc::new(
        PeerConnectionBuilder::new()
            .with_configuration(RTCConfigurationBuilder::default().build())
            .with_media_engine(media)
            .with_interceptor_registry(registry)
            .with_handler(Arc::new(CameraHandler { candidates: cand_tx })
                as Arc<dyn PeerConnectionEventHandler>)
            .with_udp_addrs(vec!["127.0.0.1:0".to_string()])
            .build()
            .await
            .expect("camera peer connection"),
    );

    let ssrc = 0x5245_5601;
    let track = Arc::new(
        TrackLocalStaticSample::new(MediaStreamTrack::new(
            "rhevia-e2e".into(),
            "rhevia-e2e-video".into(),
            "Test Cam".into(),
            RtpCodecKind::Video,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters {
                    ssrc: Some(ssrc),
                    ..Default::default()
                },
                codec: RTCRtpCodec {
                    mime_type: MIME_TYPE_H264.to_owned(),
                    clock_rate: 90_000,
                    channels: 0,
                    sdp_fmtp_line:
                        "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
                            .to_owned(),
                    rtcp_feedback: vec![],
                },
                ..Default::default()
            }],
        ))
        .expect("local track"),
    );
    let sender = camera_pc
        .add_track(Arc::clone(&track) as Arc<dyn TrackLocal>)
        .await
        .expect("add track");

    // ---- negotiate through the real signaling server -----------------------
    let offer = camera_pc.create_offer(None).await.unwrap();
    camera_pc.set_local_description(offer.clone()).await.unwrap();
    camera_sig
        .signal(SignalPayload::Offer { sdp: offer.sdp })
        .unwrap();

    let LinkEvent::Signal(SignalPayload::Offer { sdp }) = next(&desktop_sig, "the offer").await
    else {
        panic!("expected the offer");
    };
    let answer = receiver.accept_offer(&sdp).await.expect("answer");
    desktop_sig
        .signal(SignalPayload::Answer { sdp: answer })
        .unwrap();

    let LinkEvent::Signal(SignalPayload::Answer { sdp }) = next(&camera_sig, "the answer").await
    else {
        panic!("expected the answer");
    };
    camera_pc
        .set_remote_description(rtc::peer_connection::sdp::RTCSessionDescription::answer(sdp).unwrap())
        .await
        .unwrap();

    // ---- trickle ICE both ways ---------------------------------------------
    {
        let sig = Arc::clone(&camera_sig);
        tokio::spawn(async move {
            while let Some(p) = cand_rx.recv().await {
                if sig.signal(p).is_err() {
                    break;
                }
            }
        });
    }
    {
        let sig = Arc::clone(&desktop_sig);
        let rx = Arc::clone(&receiver);
        tokio::spawn(async move {
            while let Some(event) = sig.next_event().await {
                if let LinkEvent::Signal(payload) = event {
                    let _ = rx.add_remote_candidate(&payload).await;
                }
            }
        });
    }
    {
        let rx = Arc::clone(&receiver);
        let pc = Arc::clone(&camera_pc);
        tokio::spawn(async move {
            while let Some(payload) = rx.next_local_candidate().await {
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

    // ---- the destination ---------------------------------------------------
    // Opened only now. `ffmpeg -listen 1` waits for exactly one connection and
    // gives up after a while, so starting it before pairing means it is gone by
    // the time there is anything to publish.
    let rtmp_port = free_port();
    let destination =
        RtmpUrl::parse(&format!("rtmp://127.0.0.1:{rtmp_port}/live/relaytest")).unwrap();
    let listener = Killer(
        Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "info", "-listen", "1", "-i"])
            .arg(format!("rtmp://127.0.0.1:{rtmp_port}/live/relaytest"))
            .args(["-c", "copy", "-y"])
            .arg(&received)
            .stdout(Stdio::null())
            .stderr(Stdio::from(
                std::fs::File::create(dir.join("ffmpeg.log")).expect("log file"),
            ))
            .spawn()
            .expect("ffmpeg should start"),
    );

    let mut publisher = {
        // Retry rather than probing the port: a probe would consume the
        // server's only connection slot.
        let mut attempt = Err(rhevia_output::RtmpError::Closed);
        for _ in 0..60 {
            attempt = RtmpPublisher::connect(&destination).await;
            if attempt.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        attempt.expect("connect to the RTMP destination")
    };

    // ---- the camera shoots -------------------------------------------------
    let annexb = std::fs::read(&fixture).expect("fixture");
    let frames = rhevia_output::h264::split_access_units(&rhevia_output::h264::split_annexb(&annexb));
    let sent_frames = frames.len();
    assert!(sent_frames > 60, "fixture should be substantial");

    let payload_type = sender
        .get_parameters()
        .await
        .ok()
        .and_then(|p| p.rtp_parameters.codecs.first().map(|c| c.payload_type))
        .unwrap_or(H264_PAYLOAD_TYPE);

    {
        // Rebuild each access unit as Annex-B for the packetiser. Keyframes at
        // this size exceed the MTU, so this exercises FU-A fragmentation for
        // real rather than in a unit test.
        let mut payloads: Vec<Vec<u8>> = Vec::with_capacity(frames.len());
        for unit in &frames {
            let mut buf = Vec::new();
            for nal in unit {
                buf.extend_from_slice(&[0, 0, 0, 1]);
                buf.extend_from_slice(nal);
            }
            payloads.push(buf);
        }

        let track = Arc::clone(&track);
        tokio::spawn(async move {
            for data in payloads {
                let _ = track
                    .sample_writer(ssrc, payload_type)
                    .write_sample(&Sample {
                        data: data.into(),
                        duration: Duration::from_millis(1000 / u64::from(FPS)),
                        ..Default::default()
                    })
                    .await;
                tokio::time::sleep(Duration::from_millis(1000 / u64::from(FPS))).await;
            }
        });
    }

    // ---- Rhevia republishes it ---------------------------------------------
    let stats = passthrough(&receiver, &mut publisher, Duration::from_secs(8))
        .await
        .expect("passthrough should publish frames");
    publisher.close().await.ok();

    eprintln!(
        "relayed {} frames ({} keyframes, {} bytes); source had {sent_frames}",
        stats.frames_published, stats.keyframes, stats.bytes_published
    );
    assert!(
        stats.frames_published > 30,
        "too few frames survived the round trip: {}",
        stats.frames_published
    );
    assert!(stats.keyframes >= 1, "no keyframe was ever published");

    // ---- an independent decoder judges the result --------------------------
    // Let the server finish on its own: killing it can leave the output file
    // unopened, which looks identical to never having received anything.
    let mut listener = listener;
    let mut exited = false;
    for _ in 0..50 {
        if matches!(listener.0.try_wait(), Ok(Some(_))) {
            exited = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if !exited {
        eprintln!("the RTMP server did not exit on its own; killing it");
    }
    drop(listener);
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Kept for diagnosis: when this chain breaks, the server's own account of
    // what it received is the fastest way to find which seam gave way.
    let ffmpeg_log = std::fs::read_to_string(dir.join("ffmpeg.log")).unwrap_or_default();
    let meta = std::fs::metadata(&received)
        .unwrap_or_else(|e| panic!("the RTMP server wrote no file ({e}); its log said:
{ffmpeg_log}"));
    assert!(meta.len() > 10_000, "server received {} bytes", meta.len());

    let probe = Command::new("ffprobe")
        .args([
            "-hide_banner", "-loglevel", "error", "-select_streams", "v:0",
            "-count_frames", "-show_entries",
            "stream=codec_name,width,height,nb_read_frames",
            "-of", "default=noprint_wrappers=1",
        ])
        .arg(&received)
        .output()
        .expect("ffprobe should run");
    let report = String::from_utf8_lossy(&probe.stdout);
    eprintln!("ffprobe says:\n{report}");

    assert!(report.contains("codec_name=h264"), "not decodable: {report}");
    assert!(
        report.contains(&format!("width={WIDTH}")) && report.contains(&format!("height={HEIGHT}")),
        "resolution survived neither WebRTC nor the remux: {report}"
    );

    let decoded: u32 = report
        .lines()
        .find_map(|l| l.strip_prefix("nb_read_frames="))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    assert!(
        decoded >= 30,
        "the relayed stream decoded only {decoded} frames: {report}"
    );
}
