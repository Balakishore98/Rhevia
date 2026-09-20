//! Decodes real files of several formats and checks frames and audio come out.
//!
//! The parsing is unit tested; what this covers is the part that only shows up
//! against a real decoder: pipe framing, pixel format, sample format, and
//! whether a clip that has been asked to loop actually keeps producing.
//!
//! Skipped with a clear message if ffmpeg is not installed.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use rhevia_media::{MediaSource, CHANNELS, SAMPLE_RATE};

const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const FPS: f32 = 30.0;

fn workdir() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/media-tests");
    std::fs::create_dir_all(&p).ok();
    p.canonicalize().unwrap_or(p)
}

/// Builds a short clip in `container`, with or without sound.
fn make(name: &str, args: &[&str]) -> PathBuf {
    let path = workdir().join(name);
    let mut command = Command::new("ffmpeg");
    command.args(["-hide_banner", "-loglevel", "error", "-y"]);
    command.args(args);
    let status = command.arg(&path).status().expect("ffmpeg should run");
    assert!(status.success(), "could not build {name}");
    path
}

/// Waits for the first frame, so a slow decoder start is not read as failure.
fn wait_for_frame(source: &MediaSource, within: Duration) -> Option<rhevia_engine::Frame> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Some(frame) = source.take_frame() {
            return Some(frame);
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    None
}

#[test]
fn an_mp4_with_sound_produces_both_pictures_and_audio() {
    if !rhevia_media::available() {
        eprintln!("SKIP: ffmpeg not installed");
        return;
    }

    let path = make(
        "clip.mp4",
        &[
            "-f", "lavfi", "-i", "testsrc=size=320x240:rate=25",
            "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
            "-t", "2",
            "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
            "-c:a", "aac",
        ],
    );

    let source = MediaSource::open(path.to_str().unwrap(), WIDTH, HEIGHT, FPS)
        .expect("should open an mp4");

    assert!(source.info.has_video, "no video stream was found");
    assert!(source.info.has_audio, "no audio stream was found");
    assert_eq!(source.info.width, 320, "probe should report the file size");

    // ---- pictures ------------------------------------------------------
    let frame = wait_for_frame(&source, Duration::from_secs(10)).expect("no frame decoded");
    assert_eq!(frame.width, WIDTH as usize, "frames should arrive at the programme size");
    assert_eq!(frame.height, HEIGHT as usize);
    assert_eq!(
        frame.data.len(),
        WIDTH as usize * HEIGHT as usize * 4,
        "frame buffer should be RGBA8"
    );
    assert!(
        frame.data.chunks_exact(4).all(|p| p[3] == 255),
        "every pixel should be opaque"
    );
    // The test pattern is not a flat colour, so a frame that is all one value
    // means the pipe is handing back something other than a picture.
    let first = frame.data[0];
    assert!(
        frame.data.iter().any(|&b| b != first),
        "the decoded frame is a flat colour"
    );

    // ---- sound ---------------------------------------------------------
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut heard = false;
    while Instant::now() < deadline {
        let block = source.take_audio(1024);
        assert_eq!(block.len(), 1024 * CHANNELS, "blocks must be a fixed size");
        if block.iter().any(|&s| s.abs() > 0.01) {
            heard = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    assert!(heard, "no audio was decoded from a file that has a tone in it");
    assert!(source.audio_produced() > 0);
}

#[test]
fn several_containers_all_open() {
    if !rhevia_media::available() {
        eprintln!("SKIP: ffmpeg not installed");
        return;
    }

    // The point of driving ffmpeg is that the container does not matter. If
    // one of these fails, that claim is wrong.
    for (name, codec) in [("clip.mkv", "libx264"), ("clip.webm", "libvpx"), ("clip.mov", "libx264")]
    {
        let path = make(
            name,
            &[
                "-f", "lavfi", "-i", "testsrc=size=160x120:rate=15",
                "-t", "1",
                "-c:v", codec, "-pix_fmt", "yuv420p",
            ],
        );

        let source = match MediaSource::open(path.to_str().unwrap(), WIDTH, HEIGHT, FPS) {
            Ok(source) => source,
            Err(e) => panic!("{name} would not open: {e}"),
        };
        assert!(source.info.has_video, "{name} reported no video");
        assert!(
            wait_for_frame(&source, Duration::from_secs(10)).is_some(),
            "{name} decoded no frames"
        );
    }
}

#[test]
fn an_audio_file_opens_as_sound_with_no_picture() {
    if !rhevia_media::available() {
        eprintln!("SKIP: ffmpeg not installed");
        return;
    }

    let path = make(
        "music.mp3",
        &["-f", "lavfi", "-i", "sine=frequency=330:sample_rate=44100", "-t", "2"],
    );

    let source =
        MediaSource::open(path.to_str().unwrap(), WIDTH, HEIGHT, FPS).expect("should open an mp3");

    assert!(source.info.has_audio);
    assert!(!source.info.has_video, "an mp3 must not become a video input");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut heard = false;
    while Instant::now() < deadline {
        if source.take_audio(1024).iter().any(|&s| s.abs() > 0.01) {
            heard = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    assert!(heard, "no audio came out of an audio file");
    assert!(source.take_frame().is_none(), "an audio file produced a picture");
}

#[test]
fn a_clip_keeps_playing_past_its_own_length() {
    if !rhevia_media::available() {
        eprintln!("SKIP: ffmpeg not installed");
        return;
    }

    // A holding clip or a sting has to loop. Stopping at the end would leave
    // a frozen frame on air.
    let path = make(
        "short.mp4",
        &[
            "-f", "lavfi", "-i", "testsrc=size=160x120:rate=25",
            "-t", "1",
            "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
        ],
    );

    let source = MediaSource::open(path.to_str().unwrap(), WIDTH, HEIGHT, FPS)
        .expect("should open");
    assert!(wait_for_frame(&source, Duration::from_secs(10)).is_some());

    // Well past the one-second length of the file.
    std::thread::sleep(Duration::from_millis(2500));
    assert!(
        wait_for_frame(&source, Duration::from_secs(5)).is_some(),
        "the clip stopped at the end of the file instead of looping"
    );
}

#[test]
fn dropping_a_source_stops_its_decoders() {
    if !rhevia_media::available() {
        eprintln!("SKIP: ffmpeg not installed");
        return;
    }

    // ffmpeg is looping forever and will never exit on its own. A source that
    // does not kill it leaks a process for every clip ever opened.
    let path = make(
        "leak.mp4",
        &[
            "-f", "lavfi", "-i", "testsrc=size=160x120:rate=25",
            "-t", "1",
            "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
        ],
    );

    // Tracked by process id rather than by counting: the other tests in this
    // file run at the same time and start decoders of their own, so a count
    // proves nothing about this source.
    let pids;
    {
        let source = MediaSource::open(path.to_str().unwrap(), WIDTH, HEIGHT, FPS)
            .expect("should open");
        assert!(wait_for_frame(&source, Duration::from_secs(10)).is_some());

        pids = source.decoder_pids();
        assert!(!pids.is_empty(), "no decoder was started");
        for pid in &pids {
            assert!(is_running(*pid), "decoder {pid} was not running while the source was open");
        }
    }

    // Give the kill a moment to land.
    std::thread::sleep(Duration::from_millis(800));
    for pid in pids {
        assert!(
            !is_running(pid),
            "decoder {pid} outlived the source that started it"
        );
    }
}

/// Whether a process id is still alive.
fn is_running(pid: u32) -> bool {
    Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!("if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ 1 }} else {{ 0 }}"),
        ])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
        .unwrap_or(false)
}

#[test]
fn the_sample_rate_matches_what_the_mixer_expects() {
    // A decoder handing back 44.1 kHz would play everything at the wrong
    // pitch once mixed.
    assert_eq!(SAMPLE_RATE, 48_000);
    assert_eq!(CHANNELS, 2);
}
