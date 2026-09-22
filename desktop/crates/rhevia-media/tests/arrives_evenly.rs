//! Whether a clip's pictures arrive evenly, and in step with its sound.
//!
//! Frames per second is an average, and an average hides the thing an
//! operator actually sees. Thirty frames delivered as twenty-nine in one
//! burst and one late is thirty frames a second and looks broken. These
//! measure the spacing, not the count.
//!
//! Skipped with a clear message if ffmpeg is not installed.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use rhevia_media::{MediaSource, CHANNELS, SAMPLE_RATE};

const FPS: f32 = 30.0;

fn workdir() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/media-tests");
    std::fs::create_dir_all(&p).ok();
    p.canonicalize().unwrap_or(p)
}

/// A clip at the size and rate a real production runs at.
fn clip(name: &str, width: u32, height: u32, rate: &str, seconds: u32) -> PathBuf {
    let path = workdir().join(name);
    if path.exists() {
        return path;
    }
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", &format!("testsrc=size={width}x{height}:rate={rate}")])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000"])
        .args(["-t", &seconds.to_string()])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
        .args(["-c:a", "aac"])
        .arg(&path)
        .status()
        .expect("ffmpeg should run");
    assert!(status.success(), "could not build {name}");
    path
}

/// The spacing between pictures, as an operator's eye would judge it.
struct Spacing {
    taken: usize,
    /// Ticks where the engine asked and nothing new had arrived, so the last
    /// picture was shown twice.
    repeats: usize,
    worst_gap_ms: f64,
    late_ticks: usize,
    seconds: f64,
}

/// Polls at the rate the engine does and records what it would have seen.
fn sample(source: &MediaSource, seconds: f64) -> Spacing {
    let tick = Duration::from_secs_f64(1.0 / FPS as f64);
    let start = Instant::now();
    let deadline = start + Duration::from_secs_f64(seconds);

    let mut taken = 0usize;
    let mut repeats = 0usize;
    let mut worst_gap_ms = 0.0f64;
    let mut late_ticks = 0usize;
    let mut last_arrival = Instant::now();
    let mut next = Instant::now();

    while Instant::now() < deadline {
        next += tick;
        if source.take_frame().is_some() {
            taken += 1;
            let gap = last_arrival.elapsed().as_secs_f64() * 1000.0;
            if taken > 1 {
                worst_gap_ms = worst_gap_ms.max(gap);
                // More than two ticks without a picture is a visible hitch.
                if gap > 1000.0 / FPS as f64 * 2.0 {
                    late_ticks += 1;
                }
            }
            last_arrival = Instant::now();
        } else {
            repeats += 1;
        }
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }

    Spacing {
        taken,
        repeats,
        worst_gap_ms,
        late_ticks,
        seconds: start.elapsed().as_secs_f64(),
    }
}

fn wait_for_frame(source: &MediaSource, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if source.take_frame().is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn a_clip_delivers_a_picture_on_almost_every_tick() {
    if !rhevia_media::available() {
        eprintln!("SKIP: ffmpeg not installed");
        return;
    }
    // 1080p at 29.97, which is what a camera and most files actually are,
    // decoded into a 1080p production.
    let file = clip("even-1080p.mp4", 1920, 1080, "30000/1001", 20);
    let source = MediaSource::open(file.to_str().unwrap(), 1920, 1080, FPS)
        .expect("the clip should open");
    assert!(wait_for_frame(&source, Duration::from_secs(20)), "no first picture");
    // Past the start, where the decoder is still filling its pipeline.
    std::thread::sleep(Duration::from_secs(1));
    while source.take_frame().is_some() {}

    let seen = sample(&source, 8.0);
    let arrived = seen.taken as f64 / seen.seconds;
    let repeat_share = seen.repeats as f64 / (seen.taken + seen.repeats).max(1) as f64 * 100.0;
    eprintln!(
        "  {:.1} pictures a second over {:.1}s · {:.0}% of ticks repeated the last one \
         · worst gap {:.0} ms · {} hitches",
        arrived, seen.seconds, repeat_share, seen.worst_gap_ms, seen.late_ticks
    );

    assert!(
        arrived > FPS as f64 * 0.95,
        "only {arrived:.1} pictures a second reached the compositor out of {FPS}"
    );
    assert!(
        seen.worst_gap_ms < 1000.0 / FPS as f64 * 3.0,
        "a picture was {:.0} ms late, which is a visible hitch",
        seen.worst_gap_ms
    );
    assert!(
        seen.late_ticks <= 2,
        "{} hitches in {:.0} seconds",
        seen.late_ticks,
        seen.seconds
    );

    // And the decoder should be the one waiting, not the compositor. A queue
    // sitting empty means every tick is a race the decoder might lose, which
    // is the state this was in before it had a queue at all.
    let depth = source.queued();
    eprintln!("  {depth} pictures ready when the compositor stopped asking");
    assert!(depth > 0, "the decoder is only just keeping up, with nothing in hand");
}

#[test]
fn the_sound_keeps_up_with_the_picture() {
    // Two decoders, one file. Nothing holds them together, so if one runs
    // slow against the other the clip drifts out of sync as it plays --
    // which looks exactly like lag on the picture.
    if !rhevia_media::available() {
        eprintln!("SKIP: ffmpeg not installed");
        return;
    }
    let file = clip("even-sync.mp4", 1280, 720, "30", 30);
    let source = MediaSource::open(file.to_str().unwrap(), 1920, 1080, FPS)
        .expect("the clip should open");
    assert!(wait_for_frame(&source, Duration::from_secs(20)), "no first picture");
    std::thread::sleep(Duration::from_secs(1));

    // Drain both the way the engine does: a picture and a block of sound per
    // tick, for long enough for any drift to show.
    let tick = Duration::from_secs_f64(1.0 / FPS as f64);
    let block = (SAMPLE_RATE as f64 / FPS as f64) as usize;
    let mut pictures = 0usize;
    let mut sound_frames = 0usize;
    let mut silence = 0usize;
    let mut next = Instant::now();
    let start = Instant::now();

    while start.elapsed() < Duration::from_secs(10) {
        next += tick;
        if source.take_frame().is_some() {
            pictures += 1;
        }
        let taken = source.take_audio(block);
        // Padding means the decoder did not keep up and the mixer was handed
        // silence, which is a hole in the sound.
        if taken.iter().rev().take(block / 4 * CHANNELS).all(|&s| s == 0.0) {
            silence += 1;
        }
        sound_frames += block;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }

    let seconds = start.elapsed().as_secs_f64();
    let picture_seconds = pictures as f64 / FPS as f64;
    let sound_seconds = sound_frames as f64 / SAMPLE_RATE as f64;
    eprintln!(
        "  over {seconds:.1}s: {picture_seconds:.2}s of picture, {sound_seconds:.2}s of sound, \
         {silence} ticks padded with silence"
    );
    assert!(
        (picture_seconds - sound_seconds).abs() < 0.5,
        "the picture and the sound drifted apart by {:.2}s in {seconds:.0}s",
        (picture_seconds - sound_seconds).abs()
    );
    assert!(
        silence < 15,
        "{silence} of {} ticks had no sound to hand the mixer",
        (seconds * FPS as f64) as usize
    );
}
