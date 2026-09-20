//! Muxes real H.264 and AAC into a transport stream and makes a demuxer read it.
//!
//! A transport stream is easy to write plausibly and hard to write correctly.
//! Continuity counters, section CRCs, PES lengths and the clock reference all
//! produce a file that looks right in a hex dump and is rejected — or worse,
//! silently truncated — by an actual player. The only convincing proof is an
//! independent implementation demuxing our bytes, so ffprobe is the judge.
//!
//! Skipped with a clear message if ffmpeg is not installed.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use rhevia_output::h264::{self, ParameterSets};
use rhevia_output::mpegts::{self, TsMuxer, CLOCK_HZ};
use rhevia_output::AacEncoder;

const FPS: u32 = 30;
const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const SECONDS: u32 = 3;
const SAMPLE_RATE: u32 = 48_000;

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
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/ts-tests");
    std::fs::create_dir_all(&p).ok();
    p.canonicalize().unwrap_or(p)
}

/// Encodes a test pattern to Annex-B H.264, to have something real to mux.
fn make_fixture(path: &PathBuf) {
    let status = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
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
            "-g",
            "30",
            "-bsf:v",
            "h264_mp4toannexb",
            "-f",
            "h264",
        ])
        .arg(path)
        .status()
        .expect("ffmpeg should run");
    assert!(status.success(), "could not build the fixture");
}

fn probe(path: &PathBuf, args: &[&str]) -> String {
    let out = Command::new("ffprobe")
        .args(["-hide_banner", "-loglevel", "error"])
        .args(args)
        .arg(path)
        .output()
        .expect("ffprobe should run");
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn a_muxed_transport_stream_demuxes_to_video_and_audio() {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("SKIP: ffmpeg/ffprobe not installed");
        return;
    }

    let dir = workdir();
    let fixture = dir.join("fixture.h264");
    make_fixture(&fixture);

    let raw = std::fs::read(&fixture).expect("fixture should be readable");
    let units = h264::split_annexb(&raw);
    let access_units = h264::split_access_units(&units);
    assert!(!access_units.is_empty(), "the fixture produced no frames");

    let mut sets = ParameterSets::default();
    let mut muxer = TsMuxer::new(true);
    let mut aac = AacEncoder::new(SAMPLE_RATE, 2, 128_000).expect("aac encoder");
    let mut stream: Vec<u8> = Vec::new();
    let mut audio_samples: u64 = 0;
    let mut keyframes = 0usize;

    for (index, unit) in access_units.iter().enumerate() {
        sets.absorb(unit);
        let keyframe = h264::is_keyframe(unit);
        if keyframe {
            keyframes += 1;
        }

        // 90 kHz, derived from the frame number rather than a clock, so the
        // stream is reproducible.
        let timestamp = index as u64 * CLOCK_HZ as u64 / FPS as u64;
        let annexb = mpegts::prepare_video(unit, &sets);
        muxer.video(&annexb, timestamp, timestamp, keyframe, &mut stream);

        // One video frame worth of audio per picture, as the engine drives it.
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
            muxer.audio(&adts, pts, &mut stream);
        }
    }

    assert!(keyframes > 0, "the fixture had no keyframes to mux");
    assert!(audio_samples > 0, "no audio was muxed");
    assert_eq!(stream.len() % 188, 0, "the stream is not whole packets");

    let path = dir.join("muxed.ts");
    std::fs::write(&path, &stream).expect("should write the stream");

    // ---- the judgement -------------------------------------------------
    let report = probe(
        &path,
        &[
            "-show_entries",
            "stream=codec_type,codec_name,width,height",
            "-of",
            "default=noprint_wrappers=1",
        ],
    );
    eprintln!("{report}");

    assert!(
        report.contains("codec_name=h264"),
        "no H.264 video was found in the stream: {report}"
    );
    assert!(
        report.contains(&format!("width={WIDTH}")) && report.contains(&format!("height={HEIGHT}")),
        "the picture came back the wrong size: {report}"
    );
    assert!(
        report.contains("codec_type=audio") && report.contains("codec_name=aac"),
        "no AAC audio was found in the stream: {report}"
    );

    // ---- and it actually decodes ---------------------------------------
    // Present-and-well-formed is not the same as decodable. Counting frames
    // forces a full decode of every one.
    let counted = probe(
        &path,
        &[
            "-select_streams",
            "v:0",
            "-count_frames",
            "-show_entries",
            "stream=nb_read_frames",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ],
    );
    // ffprobe repeats the value per program, so take the first line rather
    // than trying to parse the whole block as one number.
    let decoded: usize = counted
        .lines()
        .find_map(|line| line.trim().parse().ok())
        .unwrap_or(0);
    assert!(
        decoded >= access_units.len() - 2,
        "frames went missing in the mux: put in {}, got back {decoded}",
        access_units.len()
    );

    // ---- and nothing is corrupt ----------------------------------------
    // ffmpeg reports continuity errors and bad PES lengths on stderr even
    // when it manages to decode, so silence here is the real result.
    let decode = Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-i"])
        .arg(&path)
        .args(["-f", "null", "-"])
        .output()
        .expect("ffmpeg should run");
    let complaints = String::from_utf8_lossy(&decode.stderr);
    assert!(
        complaints.trim().is_empty(),
        "the demuxer complained about the stream:\n{complaints}"
    );
}
