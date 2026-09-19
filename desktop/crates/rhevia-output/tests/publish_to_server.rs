//! Publishes real H.264 to a real RTMP server and checks the result decodes.
//!
//! FLV and RTMP are unforgiving in ways that unit tests cannot catch: a wrong
//! configuration record, a mislabelled keyframe or a bad chunk size produces a
//! stream that connects, reports healthy, and shows nothing. The only
//! convincing proof is an independent implementation receiving our bytes and
//! decoding them, so this runs ffmpeg as the server and ffprobe as the judge.
//!
//! Skipped with a clear message if ffmpeg is not installed.

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use rhevia_output::flv;
use rhevia_output::h264::{self, ParameterSets};
use rhevia_output::{RtmpPublisher, RtmpUrl};

const FPS: u32 = 30;
const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const SECONDS: u32 = 2;

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn workdir() -> PathBuf {
    // Under target/, so it is already gitignored and cleaned by `cargo clean`.
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/rtmp-tests")
        .canonicalize()
        .unwrap_or_else(|_| {
            let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/rtmp-tests");
            std::fs::create_dir_all(&p).ok();
            p
        });
    std::fs::create_dir_all(&dir).ok();
    dir
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Kills the server if the test panics, so a failure does not leave it running.
struct Listener(Child);
impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Encodes a test pattern to Annex-B H.264.
fn make_fixture(path: &PathBuf) {
    let status = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=size={WIDTH}x{HEIGHT}:rate={FPS}"),
            "-t",
            &SECONDS.to_string(),
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            // A keyframe every second, so the test exercises both keyframe and
            // inter-frame tagging rather than only the first frame.
            "-g",
            &FPS.to_string(),
            "-pix_fmt",
            "yuv420p",
            "-f",
            "h264",
            "-y",
        ])
        .arg(path)
        .status()
        .expect("ffmpeg should run");
    assert!(status.success(), "could not build the H.264 fixture");
}

#[tokio::test]
async fn publishes_h264_that_a_real_server_can_decode() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("SKIP: ffmpeg/ffprobe not on PATH");
        return;
    }

    let dir = workdir();
    let fixture = dir.join("pattern.h264");
    let received = dir.join("received.flv");
    let _ = std::fs::remove_file(&received);
    make_fixture(&fixture);

    let port = free_port();
    let url = RtmpUrl::parse(&format!("rtmp://127.0.0.1:{port}/live/rheviatest")).unwrap();

    // ffmpeg as the RTMP server. It writes whatever we publish, unmodified, so
    // the file it produces is exactly our muxing.
    let listener = Listener(
        Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-listen", "1", "-i"])
            .arg(format!("rtmp://127.0.0.1:{port}/live/rheviatest"))
            .args(["-c", "copy", "-y"])
            .arg(&received)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("ffmpeg should start"),
    );

    // ---- publish -----------------------------------------------------------
    let annexb = std::fs::read(&fixture).expect("read the fixture");
    let units = h264::split_annexb(&annexb);
    let access_units = h264::split_access_units(&units);
    assert!(
        access_units.len() > 30,
        "fixture should have plenty of frames, got {}",
        access_units.len()
    );

    // Retry the real connection rather than probing the port first: `-listen 1`
    // accepts exactly one connection, so a TCP probe consumes the server's
    // only slot and it exits before the client ever arrives.
    let mut publisher = {
        let mut attempt = Err(rhevia_output::RtmpError::Closed);
        for _ in 0..60 {
            attempt = RtmpPublisher::connect(&url).await;
            if attempt.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        attempt.expect("connect and publish")
    };

    let mut sets = ParameterSets::default();
    let mut sent_config = false;
    let mut keyframes = 0usize;

    for (index, unit) in access_units.iter().enumerate() {
        let refs: Vec<&[u8]> = unit.to_vec();
        sets.absorb(&refs);

        // The decoder configuration must precede the first frame, or nothing
        // downstream can start decoding.
        if !sent_config {
            if let Some(config) = sets.to_avc_decoder_config() {
                publisher
                    .send_video(flv::avc_sequence_header(&config), 0, false)
                    .await
                    .expect("send the sequence header");
                sent_config = true;
            }
        }

        let keyframe = h264::is_keyframe(&refs);
        if keyframe {
            keyframes += 1;
        }
        let avcc = h264::annexb_to_avcc(&refs);
        if avcc.is_empty() {
            continue;
        }

        let timestamp_ms = (index as u32 * 1000) / FPS;
        publisher
            .send_video(flv::avc_frame(&avcc, keyframe, 0), timestamp_ms, false)
            .await
            .expect("send a frame");
    }

    assert!(sent_config, "never produced a decoder configuration record");
    assert!(keyframes >= 2, "expected several keyframes, got {keyframes}");

    publisher.close().await.ok();
    drop(publisher);

    // ---- verify what the server actually received --------------------------
    for _ in 0..100 {
        if listener.0.id() == 0 {
            break;
        }
        match std::fs::metadata(&received) {
            Ok(m) if m.len() > 10_000 => break,
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    drop(listener);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let meta = std::fs::metadata(&received).expect("the server should have written a file");
    assert!(
        meta.len() > 10_000,
        "the server received almost nothing: {} bytes",
        meta.len()
    );

    // ffprobe is the independent judge: if it can read the stream parameters,
    // our configuration record and frame tagging are correct.
    let probe = Command::new("ffprobe")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-select_streams",
            "v:0",
            "-count_frames",
            "-show_entries",
            "stream=codec_name,width,height,nb_read_frames",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(&received)
        .output()
        .expect("ffprobe should run");

    let report = String::from_utf8_lossy(&probe.stdout);
    eprintln!("ffprobe says:\n{report}");

    assert!(report.contains("codec_name=h264"), "not decodable as H.264: {report}");
    assert!(
        report.contains(&format!("width={WIDTH}")),
        "wrong width — the decoder configuration record is bad: {report}"
    );
    assert!(
        report.contains(&format!("height={HEIGHT}")),
        "wrong height — the decoder configuration record is bad: {report}"
    );

    let frames: u32 = report
        .lines()
        .find_map(|l| l.strip_prefix("nb_read_frames="))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    assert!(
        frames as usize >= access_units.len() - 2,
        "frames went missing in transit: sent {}, server decoded {frames}",
        access_units.len()
    );
}
