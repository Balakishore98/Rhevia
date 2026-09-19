//! `rhevia-stream` — publishes an H.264 file to an RTMP destination.
//!
//! This is the passthrough path from `docs/07-roadmap.md`: it proves the whole
//! output half (mux, handshake, platform ingest, pacing) while the compositor
//! does not exist yet. The same publisher will carry compositor output later
//! without changing.
//!
//!     rhevia-stream <file.h264> <rtmp-url> [stream-key] [--fps N] [--loop]

use std::process::ExitCode;
use std::time::{Duration, Instant};

use rhevia_output::flv;
use rhevia_output::h264::{self, ParameterSets};
use rhevia_output::{RtmpPublisher, RtmpUrl};

const USAGE: &str = "\
rhevia-stream — publish H.264 to an RTMP destination

USAGE:
    rhevia-stream <file.h264> <rtmp-url> [stream-key] [options]

OPTIONS:
    --fps <n>     Frame rate of the source (default 30)
    --loop        Repeat the file until interrupted
    -h, --help

EXAMPLES:
    rhevia-stream clip.h264 rtmp://a.rtmp.youtube.com/live2 xxxx-xxxx-xxxx
    rhevia-stream clip.h264 rtmp://127.0.0.1/live/test --loop

The file must be Annex-B H.264. To produce one:
    ffmpeg -i input.mp4 -c:v libx264 -bsf:v h264_mp4toannexb -f h264 clip.h264
";

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(());
    }

    let mut positional = Vec::new();
    let mut fps: u32 = 30;
    let mut repeat = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--fps" => {
                i += 1;
                fps = args
                    .get(i)
                    .ok_or("--fps needs a number")?
                    .parse()
                    .map_err(|_| "--fps needs a number")?;
                if fps == 0 {
                    return Err("--fps must be greater than zero".into());
                }
            }
            "--loop" => repeat = true,
            other => positional.push(other.to_string()),
        }
        i += 1;
    }

    let path = positional.first().ok_or("missing input file\n\n{USAGE}")?;
    let url_text = positional.get(1).ok_or("missing RTMP URL")?;
    let url = RtmpUrl::parse_with_key(url_text, positional.get(2).map(|s| s.as_str()))
        .map_err(|e| e.to_string())?;

    let annexb = std::fs::read(path).map_err(|e| format!("could not read {path}: {e}"))?;
    let units = h264::split_annexb(&annexb);
    let access_units = h264::split_access_units(&units);
    if access_units.is_empty() {
        return Err(format!(
            "{path} contains no H.264 access units — is it Annex-B? See --help"
        ));
    }

    println!(
        "publishing {} frames at {fps} fps to rtmp://{}:{}/{}",
        access_units.len(),
        url.host,
        url.port,
        url.app
    );

    let mut publisher = RtmpPublisher::connect(&url)
        .await
        .map_err(|e| e.to_string())?;
    println!("connected; the server accepted the stream key");

    let frame_interval = Duration::from_nanos(1_000_000_000 / u64::from(fps));
    let started = Instant::now();
    let mut sets = ParameterSets::default();
    let mut sent_config = false;
    let mut frame_number: u64 = 0;

    loop {
        for unit in &access_units {
            let refs: Vec<&[u8]> = unit.to_vec();
            sets.absorb(&refs);

            // Must precede the first frame, or nothing downstream can decode.
            if !sent_config {
                if let Some(config) = sets.to_avc_decoder_config() {
                    publisher
                        .send_video(flv::avc_sequence_header(&config), 0, false)
                        .await
                        .map_err(|e| format!("sending the decoder config: {e}"))?;
                    sent_config = true;
                } else {
                    // Frames before the parameter sets arrive are undecodable,
                    // so skip rather than publish something unplayable.
                    continue;
                }
            }

            let avcc = h264::annexb_to_avcc(&refs);
            if avcc.is_empty() {
                continue;
            }
            let keyframe = h264::is_keyframe(&refs);
            let timestamp_ms = (frame_number * 1000 / u64::from(fps)) as u32;

            publisher
                .send_video(flv::avc_frame(&avcc, keyframe, 0), timestamp_ms, false)
                .await
                .map_err(|e| format!("sending frame {frame_number}: {e}"))?;

            frame_number += 1;

            // Pace to real time. Sending as fast as the file reads would
            // overrun any platform's ingest buffer and get the stream dropped.
            let target = started + frame_interval * (frame_number as u32);
            if let Some(wait) = target.checked_duration_since(Instant::now()) {
                tokio::time::sleep(wait).await;
            }

            if frame_number % u64::from(fps) == 0 {
                print!("\r  {} seconds sent", frame_number / u64::from(fps));
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
        }

        if !repeat {
            break;
        }
    }

    println!("\nfinished; closing the stream");
    publisher.close().await.map_err(|e| e.to_string())?;
    Ok(())
}
