//! What a real file's sound actually looks like coming out of the decoder.
//!
//! Reported as a "cracker pop" over audio that is clean in the file, with the
//! channel latching a clip while peaking well below full scale -- which only
//! happens if isolated samples are landing past 1.0. This reads the samples
//! and says what is in them, rather than reasoning about what should be.
//!
//! Skipped unless RHEVIA_TEST_CLIP points at a file.

use std::time::{Duration, Instant};

use rhevia_media::{MediaSource, CHANNELS, SAMPLE_RATE};

#[test]
#[ignore = "diagnostic; run with --ignored and RHEVIA_TEST_CLIP"]
fn a_real_clip_produces_samples_that_stay_in_range_and_do_not_jump() {
    let Ok(path) = std::env::var("RHEVIA_TEST_CLIP") else {
        eprintln!("SKIP: set RHEVIA_TEST_CLIP to a file");
        return;
    };
    if !rhevia_media::available() {
        eprintln!("SKIP: ffmpeg not installed");
        return;
    }

    let source =
        MediaSource::open(&path, 1920, 1080, 30.0).expect("the clip should open");

    // Drained the way the engine does: a picture and a block of sound per
    // tick, at real time.
    let tick = Duration::from_secs_f64(1.0 / 30.0);
    let block = SAMPLE_RATE as usize / 30;
    let mut taken: Vec<f32> = Vec::new();
    let start = Instant::now();
    let mut next = Instant::now();
    let mut reported = 0u64;
    while start.elapsed() < Duration::from_secs(45) {
        next += tick;
        let _ = source.take_frame();
        taken.extend(source.take_audio(block));
        // How the buffer behaves over time is the whole question: a cushion
        // that keeps growing has to be thrown away eventually, and throwing
        // sound away in the middle of a waveform is a pop.
        let seconds = start.elapsed().as_secs();
        if seconds > reported {
            reported = seconds;
            if seconds % 5 == 0 {
                let (padded, trimmed) = source.audio_faults();
                eprintln!(
                    "    {seconds:>2}s: {:>6} frames buffered ({:>5.0} ms),                      short {padded}, trimmed {trimmed}",
                    source.audio_buffered(),
                    source.audio_buffered() as f32 / SAMPLE_RATE as f32 * 1000.0,
                );
            }
        }
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }

    // The first moments are the decoder filling its pipeline; the complaint
    // is about steady playback.
    let settled = &taken[(SAMPLE_RATE as usize * CHANNELS).min(taken.len())..];
    assert!(!settled.is_empty(), "no sound came out at all");

    let loudest = settled.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let over = settled.iter().filter(|s| s.abs() > 1.0).count();
    let wild = settled.iter().filter(|s| s.abs() > 2.0).count();

    // Steps on one channel at a time: interleaved stereo alternates sides, so
    // neighbouring samples in the slice are not neighbouring in time.
    let mut worst_step = 0.0f32;
    let mut big_steps = 0;
    for w in settled.chunks_exact(2).collect::<Vec<_>>().windows(2) {
        let step = (w[1][0] - w[0][0]).abs();
        worst_step = worst_step.max(step);
        // A 20 kHz full-scale tone steps by about 0.26 between samples at
        // 48 kHz, so anything past half of full scale is not music.
        if step > 0.5 {
            big_steps += 1;
        }
    }

    // Runs of exact zeros in the middle of sound are spliced silence.
    let mut runs = 0;
    let mut run = 0;
    for &s in settled {
        if s == 0.0 {
            run += 1;
        } else {
            if run > 16 {
                runs += 1;
            }
            run = 0;
        }
    }

    let (padded, trimmed) = source.audio_faults();
    eprintln!("  {} frames over ~44 s", settled.len() / CHANNELS);
    eprintln!("    loudest sample        {loudest:.4}");
    eprintln!("    samples past 1.0      {over}");
    eprintln!("    samples past 2.0      {wild}");
    eprintln!("    worst step            {worst_step:.4}");
    eprintln!("    steps past 0.5        {big_steps}");
    eprintln!("    runs of silence       {runs}");
    eprintln!("    handed short / trimmed {padded} / {trimmed}");
    eprintln!("    still buffered        {} frames", source.audio_buffered());
}
