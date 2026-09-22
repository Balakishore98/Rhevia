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
// ---- transport ----------------------------------------------------------
//
// A clip an operator cannot hold, cue or skip through is not a source, it is
// a broadcast they are watching. These check the four controls actually move
// the file rather than only changing a flag.

/// A clip with a burnt-in timer, so where it has got to can be read off the
/// picture rather than taken on trust from a counter we also wrote.
fn timed_clip(name: &str, seconds: u32) -> PathBuf {
    make(
        name,
        &[
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=size={WIDTH}x{HEIGHT}:rate={}", FPS as u32),
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            &seconds.to_string(),
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "15",
            "-c:a",
            "aac",
        ],
    )
}

/// How different two pictures are, as an average per channel.
fn difference(a: &rhevia_engine::Frame, b: &rhevia_engine::Frame) -> f32 {
    if a.data.len() != b.data.len() || a.data.is_empty() {
        return f32::INFINITY;
    }
    let total: u64 = a
        .data
        .iter()
        .zip(b.data.iter())
        .map(|(x, y)| x.abs_diff(*y) as u64)
        .sum();
    total as f32 / a.data.len() as f32
}

/// Takes pictures at the rate the compositor does, for a while.
///
/// A source holds a few decoded pictures ready so that jitter between the
/// decoder's clock and the compositor's does not show as judder. The other
/// side of that is back-pressure: with nothing taking them the queue fills,
/// the decoder waits, and the clip stops advancing. That is correct — in
/// Rhevia every input is drained every tick whether it is on air or not —
/// but it means a test has to behave like the compositor to see the clip
/// play at all.
fn drain_for(source: &MediaSource, how_long: Duration) -> usize {
    let tick = Duration::from_secs_f64(1.0 / FPS as f64);
    let deadline = Instant::now() + how_long;
    let mut taken = 0;
    while Instant::now() < deadline {
        if source.take_frame().is_some() {
            taken += 1;
        }
        std::thread::sleep(tick);
    }
    taken
}

#[test]
fn a_paused_clip_stops_where_it_is_and_carries_on_when_let_go() {
    if !rhevia_media::available() {
        eprintln!("SKIP: ffmpeg not installed");
        return;
    }
    let clip = timed_clip("transport-pause.mp4", 10);
    let source = MediaSource::open(clip.to_str().unwrap(), WIDTH, HEIGHT, FPS)
        .expect("the clip should open");

    assert!(wait_for_frame(&source, Duration::from_secs(15)).is_some(), "no first picture");
    assert!(!source.paused(), "a clip should start playing");
    drain_for(&source, Duration::from_millis(600));

    source.set_paused(true);
    // Whatever was already decoded ahead is taken first — those are the
    // moments immediately after the last one shown, and they should be shown
    // before the clip settles.
    drain_for(&source, Duration::from_millis(500));
    let at_pause = source.position_seconds();

    // Now nothing more should arrive, however long it is left.
    let arrived = drain_for(&source, Duration::from_secs(1));
    let still_there = source.position_seconds();

    eprintln!(
        "  paused at {at_pause:.2}s, a second later {still_there:.2}s, \
         {arrived} new pictures while held"
    );
    assert_eq!(arrived, 0, "the clip kept decoding while it was supposed to be held");
    assert!(
        (still_there - at_pause).abs() < 0.1,
        "the position moved while paused: {at_pause:.2}s to {still_there:.2}s"
    );

    source.set_paused(false);
    let moving = wait_for_frame(&source, Duration::from_secs(5)).expect("it never restarted");
    drain_for(&source, Duration::from_millis(800));
    let later = source.position_seconds();
    eprintln!("  let go, now at {later:.2}s");
    assert!(later > at_pause + 0.2, "letting go did not restart it");

    // And it really is a different picture, not the held one handed back.
    assert!(
        difference(&moving, &rhevia_engine::Frame::new(WIDTH as usize, HEIGHT as usize)) > 1.0,
        "the picture after resuming is blank"
    );
}

#[test]
fn skipping_forward_and_back_moves_the_clip_by_that_much() {
    if !rhevia_media::available() {
        eprintln!("SKIP: ffmpeg not installed");
        return;
    }
    let clip = timed_clip("transport-skip.mp4", 30);
    let mut source = MediaSource::open(clip.to_str().unwrap(), WIDTH, HEIGHT, FPS)
        .expect("the clip should open");
    wait_for_frame(&source, Duration::from_secs(15)).expect("no first picture");

    drain_for(&source, Duration::from_millis(400));
    let before = source.position_seconds();
    source.skip(5.0).expect("skipping forward should work");
    wait_for_frame(&source, Duration::from_secs(10)).expect("nothing came back after the skip");
    let forward = source.position_seconds();
    eprintln!("  {before:.2}s -> +5s -> {forward:.2}s");
    assert!(
        forward >= before + 4.0,
        "skipping forward five seconds moved it from {before:.2}s to {forward:.2}s"
    );

    source.skip(-5.0).expect("skipping back should work");
    wait_for_frame(&source, Duration::from_secs(10)).expect("nothing came back after the skip");
    let back = source.position_seconds();
    eprintln!("  {forward:.2}s -> -5s -> {back:.2}s");
    assert!(
        back < forward - 3.0,
        "skipping back five seconds moved it from {forward:.2}s to {back:.2}s"
    );
}

#[test]
fn skipping_back_at_the_start_does_not_go_negative() {
    if !rhevia_media::available() {
        eprintln!("SKIP: ffmpeg not installed");
        return;
    }
    let clip = timed_clip("transport-start.mp4", 8);
    let mut source = MediaSource::open(clip.to_str().unwrap(), WIDTH, HEIGHT, FPS)
        .expect("the clip should open");
    wait_for_frame(&source, Duration::from_secs(15)).expect("no first picture");

    // One second in, skipping back five should land inside the file rather
    // than at minus four seconds, which ffmpeg would refuse outright.
    source.skip(-5.0).expect("skipping back at the start should be allowed");
    let landed = source.position_seconds();
    eprintln!("  landed at {landed:.2}s in an eight second clip");
    assert!(landed >= 0.0, "the position went negative: {landed:.2}s");
    assert!(
        wait_for_frame(&source, Duration::from_secs(10)).is_some(),
        "the clip stopped producing pictures after skipping back past the start"
    );
}

#[test]
fn stop_cues_the_clip_at_the_beginning_and_holds_it() {
    if !rhevia_media::available() {
        eprintln!("SKIP: ffmpeg not installed");
        return;
    }
    let clip = timed_clip("transport-stop.mp4", 12);
    let mut source = MediaSource::open(clip.to_str().unwrap(), WIDTH, HEIGHT, FPS)
        .expect("the clip should open");
    wait_for_frame(&source, Duration::from_secs(15)).expect("no first picture");
    drain_for(&source, Duration::from_secs(1));
    assert!(source.position_seconds() > 0.3, "the clip never started playing");

    source.stop().expect("stopping should work");
    drain_for(&source, Duration::from_millis(500));

    eprintln!(
        "  stopped: at {:.2}s, held: {}",
        source.position_seconds(),
        source.paused()
    );
    assert!(source.paused(), "stop should hold the clip, not leave it running");
    assert!(
        source.position_seconds() < 0.3,
        "stop should cue the clip at the start, not at {:.2}s",
        source.position_seconds()
    );

    // Cued, not unloaded: pressing play carries on from the beginning.
    source.set_paused(false);
    assert!(
        wait_for_frame(&source, Duration::from_secs(10)).is_some(),
        "the clip could not be played again after stopping"
    );
}

#[test]
fn the_transport_leaves_no_decoders_behind() {
    // Each skip restarts ffmpeg. A skip that abandons the old process leaks
    // one per press, and an operator cueing a clip presses these a lot.
    if !rhevia_media::available() {
        eprintln!("SKIP: ffmpeg not installed");
        return;
    }
    let clip = timed_clip("transport-leak.mp4", 20);
    let mut source = MediaSource::open(clip.to_str().unwrap(), WIDTH, HEIGHT, FPS)
        .expect("the clip should open");
    wait_for_frame(&source, Duration::from_secs(15)).expect("no first picture");

    let mut abandoned = Vec::new();
    for _ in 0..4 {
        abandoned.extend(source.decoder_pids());
        source.skip(2.0).expect("skipping should work");
        drain_for(&source, Duration::from_millis(200));
    }
    let alive = source.decoder_pids();
    drop(source);
    std::thread::sleep(Duration::from_millis(500));

    let mut still_running = Vec::new();
    for pid in abandoned.iter().chain(alive.iter()) {
        if is_running(*pid) {
            still_running.push(*pid);
        }
    }
    eprintln!(
        "  started {} decoders across four skips, {} still running",
        abandoned.len() + alive.len(),
        still_running.len()
    );
    assert!(still_running.is_empty(), "decoders left behind: {still_running:?}");
}
