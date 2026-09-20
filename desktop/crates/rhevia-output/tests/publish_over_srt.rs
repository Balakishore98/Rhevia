//! Sends a real stream over SRT to ffmpeg and checks what arrives decodes.
//!
//! The transport stream is already checked against a demuxer elsewhere. What
//! this adds is the part that only shows up on a socket: payload sizing,
//! ordering, and whether the connection is closed cleanly enough that the
//! receiver finishes its file instead of leaving a truncated one.
//!
//! Skipped with a clear message if ffmpeg lacks SRT support.

use std::net::UdpSocket;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use rhevia_output::h264::{self, ParameterSets};
use rhevia_output::mpegts::{self, CLOCK_HZ};
use rhevia_output::{AacEncoder, SrtPublisher, SrtUrl};

const FPS: u32 = 30;
const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const SECONDS: u32 = 2;
const SAMPLE_RATE: u32 = 48_000;

fn have_srt() -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-protocols"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("srt"))
        .unwrap_or(false)
}

fn workdir() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/srt-tests");
    std::fs::create_dir_all(&p).ok();
    p.canonicalize().unwrap_or(p)
}

/// A free UDP port. SRT runs over UDP, so a TCP probe would prove nothing.
fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Kills the receiver if the test panics, so a failure leaves nothing running.
struct Receiver(Child);
impl Drop for Receiver {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn make_fixture(path: &PathBuf) {
    let status = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi",
            "-i", &format!("testsrc=size={WIDTH}x{HEIGHT}:rate={FPS}"),
            "-t", &SECONDS.to_string(),
            "-c:v", "libx264",
            "-preset", "ultrafast",
            "-g", "30",
            "-bsf:v", "h264_mp4toannexb",
            "-f", "h264",
        ])
        .arg(path)
        .status()
        .expect("ffmpeg should run");
    assert!(status.success(), "could not build the fixture");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stream_sent_over_srt_arrives_and_decodes() {
    if !have_srt() {
        eprintln!("SKIP: this ffmpeg has no SRT support");
        return;
    }

    let dir = workdir();
    let fixture = dir.join("fixture.h264");
    make_fixture(&fixture);

    let received = dir.join("received.ts");
    let _ = std::fs::remove_file(&received);

    let port = free_port();

    // ffmpeg listens; Rhevia dials out, which is the usual direction for a
    // contribution feed.
    let receiver = Receiver(
        Command::new("ffmpeg")
            .args([
                "-hide_banner", "-loglevel", "error", "-y",
                "-i", &format!("srt://127.0.0.1:{port}?mode=listener&latency=120"),
                "-c", "copy",
                "-f", "mpegts",
            ])
            .arg(&received)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("ffmpeg should start"),
    );

    // Connect with retries rather than a fixed sleep: the listener takes an
    // unpredictable moment to bind, and a sleep long enough to be safe makes
    // every run slow.
    let url = SrtUrl::parse(&format!("srt://127.0.0.1:{port}?latency=120")).expect("url");
    let mut publisher = None;
    for _ in 0..40 {
        match SrtPublisher::connect(&url, true).await {
            Ok(p) => {
                publisher = Some(p);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    }
    let mut publisher = publisher.expect("could not reach the SRT receiver");

    // ---- send -----------------------------------------------------------
    let raw = std::fs::read(&fixture).expect("fixture should be readable");
    let units = h264::split_annexb(&raw);
    let access_units = h264::split_access_units(&units);
    assert!(!access_units.is_empty(), "the fixture produced no frames");

    let mut sets = ParameterSets::default();
    let mut aac = AacEncoder::new(SAMPLE_RATE, 2, 128_000).expect("aac encoder");
    let mut audio_samples: u64 = 0;

    // Paced at the frame rate, as a real sender is. SRT is a live protocol
    // with a send buffer sized to its latency budget: handed a whole file at
    // once it drops most of it, correctly, because those packets could never
    // have been played on time. Sending as fast as the loop runs is the bug,
    // not the transport.
    let started = std::time::Instant::now();
    let frame_interval = Duration::from_secs_f64(1.0 / FPS as f64);

    for (index, unit) in access_units.iter().enumerate() {
        let due = frame_interval * index as u32;
        if let Some(wait) = due.checked_sub(started.elapsed()) {
            tokio::time::sleep(wait).await;
        }

        sets.absorb(unit);
        let keyframe = h264::is_keyframe(unit);
        let timestamp = index as u64 * CLOCK_HZ / FPS as u64;

        let annexb = mpegts::prepare_video(unit, &sets);
        publisher
            .send_video(&annexb, timestamp, timestamp, keyframe)
            .await
            .expect("send video");

        let block = SAMPLE_RATE as usize / FPS as usize;
        let mut samples = Vec::with_capacity(block * 2);
        for n in 0..block {
            let t = (index * block + n) as f32 / SAMPLE_RATE as f32;
            let value = 0.3 * (std::f32::consts::TAU * 440.0 * t).sin();
            samples.push(value);
            samples.push(value);
        }
        for frame in aac.push(&samples).expect("encode audio") {
            let pts = audio_samples * CLOCK_HZ / SAMPLE_RATE as u64;
            audio_samples += frame.samples as u64;
            let adts = mpegts::adts_wrap(&frame.data, SAMPLE_RATE, 2);
            publisher.send_audio(&adts, pts).await.expect("send audio");
        }
    }

    assert!(publisher.bytes_sent() > 0, "nothing was sent");
    publisher.close().await.expect("close cleanly");

    // Let the receiver finish and close its file. Killing it here would leave
    // a truncated file that looks exactly like a transport failure.
    let mut receiver = receiver;
    for _ in 0..60 {
        if receiver.0.try_wait().ok().flatten().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // ---- judge ----------------------------------------------------------
    let size = std::fs::metadata(&received).map(|m| m.len()).unwrap_or(0);
    assert!(size > 0, "the receiver wrote nothing");

    let probe = Command::new("ffprobe")
        .args([
            "-hide_banner", "-loglevel", "error",
            "-show_entries", "stream=codec_type,codec_name,width,height",
            "-of", "default=noprint_wrappers=1",
        ])
        .arg(&received)
        .output()
        .expect("ffprobe should run");
    let report = String::from_utf8_lossy(&probe.stdout);
    eprintln!("received {size} bytes\n{report}");

    assert!(
        report.contains("codec_name=h264"),
        "no video arrived over SRT: {report}"
    );
    assert!(
        report.contains(&format!("width={WIDTH}")),
        "the picture arrived the wrong size: {report}"
    );
    assert!(
        report.contains("codec_name=aac"),
        "no audio arrived over SRT: {report}"
    );

    // Decodable, not merely present.
    let counted = Command::new("ffprobe")
        .args([
            "-hide_banner", "-loglevel", "error",
            "-select_streams", "v:0",
            "-count_frames",
            "-show_entries", "stream=nb_read_frames",
            "-of", "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(&received)
        .output()
        .expect("ffprobe should run");
    let decoded: usize = String::from_utf8_lossy(&counted.stdout)
        .lines()
        .find_map(|l| l.trim().parse().ok())
        .unwrap_or(0);

    // SRT is allowed to drop under its latency budget; a few frames short is
    // the transport working as designed, an empty file is not.
    assert!(
        decoded >= access_units.len() * 9 / 10,
        "too much went missing: sent {}, decoded {decoded}",
        access_units.len()
    );
}
