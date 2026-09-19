//! `rhevia-relay` — pair a camera, stream it live.
//!
//! Shows a pairing code, waits for a camera anywhere on the internet to redeem
//! it, then republishes what arrives to an RTMP destination without decoding.
//!
//!     rhevia-relay --signaling wss://link.rhevia.app/ws \
//!                  --rtmp rtmp://a.rtmp.youtube.com/live2 --key xxxx-xxxx

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use rhevia_link::{CameraReceiver, ClientInfo, LinkEvent, SignalPayload, SignalingClient};
use rhevia_output::{RtmpPublisher, RtmpUrl};
use rhevia_pipeline::passthrough;

const USAGE: &str = "\
rhevia-relay — pair a camera and stream it live

USAGE:
    rhevia-relay --signaling <ws-url> --rtmp <rtmp-url> [--key <stream-key>]

OPTIONS:
    --signaling <url>   RheviaLink signaling server (ws:// or wss://)
    --rtmp <url>        Destination, e.g. rtmp://a.rtmp.youtube.com/live2
    --key <key>         Stream key, if not already part of the RTMP URL
    --name <name>       How this machine appears to the camera
    --idle <seconds>    Give up after this long with no video (default 30)
    -h, --help
";

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rhevia_relay=info,rhevia_pipeline=info".into()),
        )
        .init();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
}

async fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(());
    }

    let signaling_url = flag(&args, "--signaling").ok_or("missing --signaling")?;
    let rtmp_url = flag(&args, "--rtmp").ok_or("missing --rtmp")?;
    let destination =
        RtmpUrl::parse_with_key(rtmp_url, flag(&args, "--key")).map_err(|e| e.to_string())?;
    let idle: u64 = flag(&args, "--idle")
        .unwrap_or("30")
        .parse()
        .map_err(|_| "--idle needs a number of seconds")?;
    let name = flag(&args, "--name").unwrap_or("Rhevia").to_string();

    // ---- pair --------------------------------------------------------------
    let signaling = Arc::new(
        SignalingClient::connect_receiver(
            signaling_url,
            ClientInfo {
                name,
                platform: std::env::consts::OS.to_string(),
                app_version: env!("CARGO_PKG_VERSION").to_string(),
            },
        )
        .await
        .map_err(|e| e.to_string())?,
    );

    let receiver = Arc::new(
        CameraReceiver::new(signaling.ice_servers())
            .await
            .map_err(|e| e.to_string())?,
    );

    signaling.request_code().map_err(|e| e.to_string())?;

    // Local ICE candidates go to the camera as they are gathered, rather than
    // waiting for gathering to finish — that is what keeps connection setup to
    // a second or two instead of ten.
    {
        let sig = Arc::clone(&signaling);
        let rx = Arc::clone(&receiver);
        tokio::spawn(async move {
            while let Some(payload) = rx.next_local_candidate().await {
                if sig.signal(payload).is_err() {
                    break;
                }
            }
        });
    }

    println!("waiting for a camera…");
    let mut publisher: Option<RtmpPublisher> = None;

    loop {
        let Some(event) = signaling.next_event().await else {
            return Err("signaling connection ended before a camera connected".into());
        };

        match event {
            LinkEvent::CodeReady { code, pair_url, .. } => {
                let pretty = format!("{} {}", &code[..3], &code[3..]);
                println!("\n  pairing code:  {pretty}");
                println!("  or open:       {pair_url}\n");
            }

            LinkEvent::CameraPaired(peer) | LinkEvent::CameraRejoined(peer) => {
                println!("camera connected: {} ({})", peer.name, peer.platform);
            }

            LinkEvent::Signal(SignalPayload::Offer { sdp }) => {
                let answer = receiver.accept_offer(&sdp).await.map_err(|e| e.to_string())?;
                signaling
                    .signal(SignalPayload::Answer { sdp: answer })
                    .map_err(|e| e.to_string())?;

                // Connect to the destination only once a camera is actually
                // negotiating. Holding an idle RTMP session open beforehand
                // makes platforms show the stream as live with no picture.
                if publisher.is_none() {
                    println!("connecting to {}…", destination.host);
                    publisher = Some(
                        RtmpPublisher::connect(&destination)
                            .await
                            .map_err(|e| e.to_string())?,
                    );
                    println!("streaming");
                }
            }

            LinkEvent::Signal(payload) => {
                let _ = receiver.add_remote_candidate(&payload).await;
            }

            LinkEvent::CameraReconnecting { deadline, .. } => {
                let seconds = (deadline - now_millis()).max(0) / 1000;
                println!("camera dropped — holding the slot for {seconds}s");
            }

            LinkEvent::CameraLeft(reason) => {
                println!("camera gone ({reason:?})");
                break;
            }

            LinkEvent::ServerError { message } => eprintln!("signaling: {message}"),
            LinkEvent::Disconnected => {
                println!("signaling disconnected; media may still be flowing");
                break;
            }
            LinkEvent::Resumed(_) => {}
        }

        // Once publishing, hand over to the passthrough loop. It runs until the
        // camera stops sending, which is the normal end of a shot.
        if let Some(mut pubr) = publisher.take() {
            let stats = passthrough(&receiver, &mut pubr, Duration::from_secs(idle))
                .await
                .map_err(|e| e.to_string())?;
            println!(
                "\npublished {} frames ({} keyframes, {:.1} MB)",
                stats.frames_published,
                stats.keyframes,
                stats.bytes_published as f64 / 1_048_576.0
            );
            pubr.close().await.ok();
            break;
        }
    }

    Ok(())
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
