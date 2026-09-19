//! Cross-language interop: this Rust client against the real Node signaling server.
//!
//! The protocol is defined twice — once in `proto/signaling.ts` and once in
//! `protocol.rs` — and nothing checks that the two agree. A mismatched field
//! name does not fail to compile on either side; it shows up as a camera that
//! silently never connects. These tests are the only thing standing between
//! that class of bug and a live show, so they run the actual server.
//!
//! Skipped with a clear message if `npm run build` has not been run.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use rhevia_link::protocol::{ErrorCode, LeaveReason};
use rhevia_link::{ClientInfo, LinkEvent, SignalPayload, SignalingClient};

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is desktop/crates/rhevia-link
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repo root")
        .to_path_buf()
}

/// Kills the server when the test ends, including on panic.
struct ServerGuard {
    child: Child,
    port: u16,
}

impl ServerGuard {
    fn url(&self) -> String {
        format!("ws://127.0.0.1:{}", self.port)
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind an ephemeral port")
        .local_addr()
        .unwrap()
        .port()
}

/// Starts the real Node server, or returns None if it has not been built.
async fn start_server() -> Option<ServerGuard> {
    let root = repo_root();
    let entry = root.join("services/signaling/dist/index.js");
    if !entry.exists() {
        eprintln!(
            "SKIP: {} not found — run `npm install && npm run build` at the repo root",
            entry.display()
        );
        return None;
    }

    let port = free_port();
    let child = Command::new("node")
        .arg(&entry)
        .env("RHEVIA_PORT", port.to_string())
        .env("RHEVIA_HOST", "127.0.0.1")
        .env("RHEVIA_PAIR_URL_BASE", "https://link.test/pair")
        .env("RHEVIA_TURN_URLS", "turn:turn.test:3478")
        .env("RHEVIA_TURN_SECRET", "interop-test-secret")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("node should be on PATH");

    let guard = ServerGuard { child, port };

    // Poll rather than sleep a fixed amount: startup time varies and a flaky
    // test here would be worse than no test.
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return Some(guard);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("signaling server did not start listening within 5s");
}

fn info(name: &str, platform: &str) -> ClientInfo {
    ClientInfo {
        name: name.into(),
        platform: platform.into(),
        app_version: "0.1.0".into(),
    }
}

/// Awaits the next event, failing loudly rather than hanging the suite.
async fn next(client: &mut SignalingClient, what: &str) -> LinkEvent {
    match tokio::time::timeout(Duration::from_secs(5), client.next_event()).await {
        Ok(Some(event)) => event,
        Ok(None) => panic!("signaling closed while waiting for {what}"),
        Err(_) => panic!("timed out waiting for {what}"),
    }
}

macro_rules! skip_if_unbuilt {
    () => {
        match start_server().await {
            Some(server) => server,
            None => return,
        }
    };
}

#[tokio::test]
async fn full_pairing_and_negotiation_against_the_real_server() {
    let server = skip_if_unbuilt!();

    // --- desktop asks for a code -------------------------------------------
    let mut desktop = SignalingClient::connect_receiver(&server.url(), info("Studio PC", "win32"))
        .await
        .expect("desktop connects");

    // TURN credentials must arrive already minted, or the phone cannot relay.
    let turn = desktop
        .ice_servers()
        .iter()
        .find(|s| s.urls.iter().any(|u| u.starts_with("turn:")))
        .expect("a TURN server should be offered");
    let username = turn.username.clone().expect("TURN username");
    assert!(
        username.contains(':'),
        "coturn REST usernames are '<expiry>:<peer id>', got {username:?}"
    );
    assert!(turn.credential.is_some(), "TURN credential should be minted");

    desktop.request_code().unwrap();
    let code = match next(&mut desktop, "the pairing code").await {
        LinkEvent::CodeReady { code, pair_url, expires_at } => {
            assert_eq!(code.len(), 6, "code should be six digits, got {code:?}");
            assert!(code.chars().all(|c| c.is_ascii_digit()), "digits only: {code:?}");
            assert_eq!(pair_url, format!("https://link.test/pair?c={code}"));
            assert!(expires_at > 0);
            code
        }
        other => panic!("expected CodeReady, got {other:?}"),
    };

    // --- phone redeems it ---------------------------------------------------
    let mut phone = SignalingClient::connect_sender(&server.url(), info("Pixel 8 Pro", "android"))
        .await
        .expect("phone connects");
    phone.join(&code).unwrap();

    match next(&mut phone, "the phone's pairing confirmation").await {
        LinkEvent::CameraPaired(peer) => assert_eq!(peer.name, "Studio PC"),
        other => panic!("expected CameraPaired on the phone, got {other:?}"),
    }
    match next(&mut desktop, "the desktop's pairing notification").await {
        LinkEvent::CameraPaired(peer) => {
            assert_eq!(peer.name, "Pixel 8 Pro");
            assert_eq!(peer.platform, "android");
        }
        other => panic!("expected CameraPaired on the desktop, got {other:?}"),
    }

    // --- WebRTC negotiation relays intact -----------------------------------
    // The phone holds the media, so the phone offers.
    let offer_sdp = "v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\n";
    phone.signal(SignalPayload::Offer { sdp: offer_sdp.into() }).unwrap();
    match next(&mut desktop, "the offer").await {
        LinkEvent::Signal(SignalPayload::Offer { sdp }) => assert_eq!(sdp, offer_sdp),
        other => panic!("expected an offer, got {other:?}"),
    }

    desktop.signal(SignalPayload::Answer { sdp: "v=0\r\no=- 2 2 IN IP4 0.0.0.0\r\n".into() }).unwrap();
    match next(&mut phone, "the answer").await {
        LinkEvent::Signal(SignalPayload::Answer { sdp }) => assert!(sdp.starts_with("v=0")),
        other => panic!("expected an answer, got {other:?}"),
    }

    // Candidates carry WebRTC's exact field spelling through the Node server.
    phone
        .signal(SignalPayload::Candidate {
            candidate: "candidate:1 1 udp 2130706431 192.168.1.5 54321 typ host".into(),
            sdp_mid: Some("0".into()),
            sdp_mline_index: Some(0),
        })
        .unwrap();
    match next(&mut desktop, "an ICE candidate").await {
        LinkEvent::Signal(SignalPayload::Candidate { sdp_mid, sdp_mline_index, .. }) => {
            assert_eq!(sdp_mid.as_deref(), Some("0"));
            assert_eq!(sdp_mline_index, Some(0));
        }
        other => panic!("expected a candidate, got {other:?}"),
    }

    // Null-valued candidate fields are legal and must survive the relay.
    phone
        .signal(SignalPayload::Candidate {
            candidate: "candidate:2 1 udp 1 1.2.3.4 1 typ srflx".into(),
            sdp_mid: None,
            sdp_mline_index: None,
        })
        .unwrap();
    match next(&mut desktop, "a candidate with null fields").await {
        LinkEvent::Signal(SignalPayload::Candidate { sdp_mid, sdp_mline_index, .. }) => {
            assert!(sdp_mid.is_none() && sdp_mline_index.is_none());
        }
        other => panic!("expected a null-field candidate, got {other:?}"),
    }

    phone.signal(SignalPayload::CandidateEnd).unwrap();
    match next(&mut desktop, "end-of-candidates").await {
        LinkEvent::Signal(SignalPayload::CandidateEnd) => {}
        other => panic!("expected CandidateEnd, got {other:?}"),
    }

    // --- a clean disconnect is reported as such -----------------------------
    phone.bye().unwrap();
    match next(&mut desktop, "the camera leaving").await {
        LinkEvent::CameraLeft(LeaveReason::Bye) => {}
        other => panic!("expected CameraLeft(Bye), got {other:?}"),
    }
}

#[tokio::test]
async fn a_dropped_camera_holds_its_slot_and_can_resume() {
    let server = skip_if_unbuilt!();

    let mut desktop = SignalingClient::connect_receiver(&server.url(), info("Studio PC", "win32"))
        .await
        .unwrap();
    desktop.request_code().unwrap();
    let LinkEvent::CodeReady { code, .. } = next(&mut desktop, "the code").await else {
        panic!("expected CodeReady");
    };

    let mut phone = SignalingClient::connect_sender(&server.url(), info("Phone", "android"))
        .await
        .unwrap();
    phone.join(&code).unwrap();
    next(&mut phone, "pairing").await;
    next(&mut desktop, "pairing").await;

    // The token is what makes a live show survive a lift or a cell handover.
    let token = phone.resume_token().expect("the camera should be issued a resume token");

    // Drop without saying bye: the phone lost signal or the app was killed.
    drop(phone);

    // Crucially this is *not* a teardown. The input stays open.
    match next(&mut desktop, "the reconnect window").await {
        LinkEvent::CameraReconnecting { reason, deadline } => {
            assert_eq!(reason, LeaveReason::Transport);
            assert!(deadline > 0, "a reconnect deadline should be given");
        }
        other => panic!("expected CameraReconnecting, got {other:?}"),
    }

    // Phone comes back on a new socket and reclaims the same input.
    let mut revived = SignalingClient::connect_sender(&server.url(), info("Phone", "android"))
        .await
        .unwrap();
    revived.resume(&token).unwrap();

    match next(&mut revived, "our own resume confirmation").await {
        LinkEvent::Resumed(peer) => assert_eq!(peer.name, "Studio PC"),
        other => panic!("expected Resumed, got {other:?}"),
    }
    match next(&mut desktop, "the camera coming back").await {
        LinkEvent::CameraRejoined(peer) => assert_eq!(peer.name, "Phone"),
        other => panic!("expected CameraRejoined, got {other:?}"),
    }

    // Renegotiation must work over the restored link.
    revived
        .signal(SignalPayload::Offer { sdp: "v=0 RENEGOTIATED".into() })
        .unwrap();
    match next(&mut desktop, "the renegotiation offer").await {
        LinkEvent::Signal(SignalPayload::Offer { sdp }) => assert_eq!(sdp, "v=0 RENEGOTIATED"),
        other => panic!("expected an offer after resume, got {other:?}"),
    }
}

#[tokio::test]
async fn a_deliberate_stop_is_final_and_forfeits_the_reservation() {
    let server = skip_if_unbuilt!();

    let mut desktop = SignalingClient::connect_receiver(&server.url(), info("Studio PC", "win32"))
        .await
        .unwrap();
    desktop.request_code().unwrap();
    let LinkEvent::CodeReady { code, .. } = next(&mut desktop, "the code").await else {
        panic!("expected CodeReady");
    };

    let mut phone = SignalingClient::connect_sender(&server.url(), info("Phone", "android"))
        .await
        .unwrap();
    phone.join(&code).unwrap();
    next(&mut phone, "pairing").await;
    next(&mut desktop, "pairing").await;
    let token = phone.resume_token().expect("resume token");

    // Pressing Stop is not a dropout, so the input should close immediately
    // rather than sitting on "reconnecting" for 45 seconds.
    phone.bye().unwrap();
    match next(&mut desktop, "the deliberate stop").await {
        LinkEvent::CameraLeft(LeaveReason::Bye) => {}
        other => panic!("expected CameraLeft(Bye), got {other:?}"),
    }

    // And the forfeited token must not resurrect it.
    let mut ghost = SignalingClient::connect_sender(&server.url(), info("Phone", "android"))
        .await
        .unwrap();
    ghost.resume(&token).unwrap();
    match next(&mut ghost, "the refusal").await {
        LinkEvent::ServerError { .. } => {}
        other => panic!("a forfeited token should not resume: {other:?}"),
    }
}

#[tokio::test]
async fn a_wrong_code_is_reported_and_does_not_kill_the_connection() {
    let server = skip_if_unbuilt!();

    let mut phone = SignalingClient::connect_sender(&server.url(), info("Phone", "android"))
        .await
        .unwrap();

    phone.join("000000").unwrap();
    match next(&mut phone, "the rejection").await {
        LinkEvent::ServerError { message } => assert!(!message.is_empty()),
        other => panic!("expected ServerError, got {other:?}"),
    }

    // A typo must be recoverable without reconnecting, or the phone UX is awful.
    phone.join("111111").unwrap();
    match next(&mut phone, "the second rejection").await {
        LinkEvent::ServerError { .. } => {}
        other => panic!("expected a second ServerError, got {other:?}"),
    }
}

#[tokio::test]
async fn a_redeemed_code_cannot_be_reused_by_a_third_party() {
    let server = skip_if_unbuilt!();

    let mut desktop = SignalingClient::connect_receiver(&server.url(), info("Studio PC", "win32"))
        .await
        .unwrap();
    desktop.request_code().unwrap();
    let LinkEvent::CodeReady { code, .. } = next(&mut desktop, "the code").await else {
        panic!("expected CodeReady");
    };

    let mut phone = SignalingClient::connect_sender(&server.url(), info("Phone", "android"))
        .await
        .unwrap();
    phone.join(&code).unwrap();
    next(&mut phone, "pairing").await;
    next(&mut desktop, "pairing").await;

    // Someone who shoulder-surfed the code must not be able to take the slot.
    let mut intruder = SignalingClient::connect_sender(&server.url(), info("Intruder", "linux"))
        .await
        .unwrap();
    intruder.join(&code).unwrap();
    match next(&mut intruder, "the rejection").await {
        LinkEvent::ServerError { .. } => {}
        other => panic!("expected the replay to be rejected, got {other:?}"),
    }

    // And the legitimate pair must be undisturbed by the attempt.
    phone.signal(SignalPayload::Offer { sdp: "v=0\r\n".into() }).unwrap();
    match next(&mut desktop, "the offer still relaying").await {
        LinkEvent::Signal(SignalPayload::Offer { .. }) => {}
        other => panic!("the live pair was disturbed: {other:?}"),
    }
}

#[tokio::test]
async fn a_protocol_mismatch_fails_with_a_useful_error() {
    let server = skip_if_unbuilt!();

    // Hand-rolled hello with a bogus version, since the client always sends
    // the correct one. This is what an outdated phone app looks like.
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let (mut stream, _) = tokio_tungstenite::connect_async(server.url()).await.unwrap();
    stream
        .send(Message::Text(
            r#"{"t":"hello","protocol":9999,"role":"sender","client":{"name":"Old","platform":"android","appVersion":"0.0.1"}}"#
                .into(),
        ))
        .await
        .unwrap();

    let reply = stream.next().await.unwrap().unwrap();
    let parsed: rhevia_link::protocol::ServerMessage =
        serde_json::from_str(reply.to_text().unwrap()).unwrap();

    match parsed {
        rhevia_link::protocol::ServerMessage::Error { code, fatal, .. } => {
            // "Update your app" is actionable; a timeout is not.
            assert_eq!(code, ErrorCode::BadProtocolVersion);
            assert!(fatal, "a version mismatch should close the connection");
        }
        other => panic!("expected a protocol-version error, got {other:?}"),
    }
}

#[tokio::test]
async fn brute_forcing_codes_gets_the_attacker_throttled() {
    let server = skip_if_unbuilt!();

    let mut attacker = SignalingClient::connect_sender(&server.url(), info("Attacker", "linux"))
        .await
        .unwrap();

    // Six digits is 10^6 combinations. Without a throttle that is walkable in
    // minutes, which would make every live pairing hijackable.
    let mut throttled = false;
    for i in 0..15 {
        attacker.join(&format!("{:06}", 200_000 + i)).unwrap();
        match next(&mut attacker, "a rejection").await {
            LinkEvent::ServerError { message } => {
                if message.contains("too many") {
                    throttled = true;
                    break;
                }
            }
            LinkEvent::Disconnected => {
                throttled = true;
                break;
            }
            other => panic!("unexpected event while brute forcing: {other:?}"),
        }
    }
    assert!(throttled, "the server should throttle repeated wrong codes");
}
