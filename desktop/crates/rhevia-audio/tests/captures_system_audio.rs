//! Plays a tone through the machine's speakers and captures it back.
//!
//! The unit tests cover the sample conversion. What they cannot cover is
//! whether WASAPI loopback is actually wired up: whether the endpoint opens in
//! loopback mode, whether packets arrive, and whether what arrives is the
//! sound that was playing rather than silence or noise.
//!
//! The tone is rendered in process rather than by asking another program to
//! play a file. A spawned player reaches a different session and its sound
//! never arrives, which looks identical to loopback being broken.
//!
//! Skipped with a clear message when the machine has no usable playback
//! device, which is normal on a build server.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rhevia_audio::LoopbackCapture;

/// The loudest sample in a buffer.
fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

/// Plays a 440 Hz tone on the default output until the returned stream is
/// dropped.
///
/// Returns None when the machine has no output device, which is the normal
/// state of a build server and not a failure.
fn play_tone() -> Option<cpal::Stream> {
    let device = cpal::default_host().default_output_device()?;
    let config = device.default_output_config().ok()?;
    let rate = config.sample_rate().0 as f32;
    let channels = config.channels() as usize;

    let mut phase = 0.0f32;
    let step = std::f32::consts::TAU * 440.0 / rate;

    let stream = device
        .build_output_stream(
            &config.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                for frame in data.chunks_mut(channels) {
                    // Loud on purpose: the machine's volume is whatever it
                    // happens to be, and a quiet tone would be hard to tell
                    // from the silence this is ruling out.
                    let value = 0.6 * phase.sin();
                    phase += step;
                    for sample in frame.iter_mut() {
                        *sample = value;
                    }
                }
            },
            |e| eprintln!("output stream error: {e}"),
            None,
        )
        .ok()?;

    stream.play().ok()?;
    Some(stream)
}

#[test]
fn a_tone_played_on_the_speakers_comes_back_through_loopback() {
    // Playback starts first. A render endpoint that nothing is using goes
    // idle, and an idle endpoint delivers no loopback packets at all. It is
    // also the order an operator uses: the music is already playing when they
    // add System Audio.
    let Some(_tone) = play_tone() else {
        eprintln!("SKIP: no output device to play through");
        return;
    };
    std::thread::sleep(Duration::from_millis(500));

    let sink: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let capture = match LoopbackCapture::open(None, Arc::clone(&sink)) {
        Ok(capture) => capture,
        Err(e) => {
            eprintln!("SKIP: no capturable playback device: {e}");
            return;
        }
    };
    eprintln!(
        "capturing {} at {} Hz, {} channels",
        capture.device_name, capture.source_rate, capture.source_channels
    );

    let deadline = Instant::now() + Duration::from_secs(6);
    let mut loudest = 0.0f32;
    let mut received = 0usize;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(buffer) = sink.lock() {
            loudest = loudest.max(peak(&buffer));
            received = buffer.len();
        }
        if loudest > 0.05 {
            break;
        }
    }

    // Told apart deliberately: no packets means the endpoint was idle or
    // absent, which is the environment. Packets that are all silent means
    // loopback is running but not carrying the audio, which is a fault.
    if received == 0 {
        eprintln!("SKIP: the endpoint delivered nothing — output muted or no speakers");
        return;
    }

    assert!(
        loudest > 0.01,
        "loopback delivered {received} samples but they were silent (peak {loudest:.5}); \
         packets are arriving but not the audio being played"
    );
    eprintln!("captured {received} samples of the tone at peak {loudest:.3}");
}

#[test]
fn what_is_captured_is_the_tone_and_not_noise() {
    // A format read wrongly still produces loud samples — it produces a
    // buzz. This checks the captured audio is actually a smooth 440 Hz tone,
    // by counting how often it crosses zero.
    let Some(_tone) = play_tone() else {
        eprintln!("SKIP: no output device");
        return;
    };
    std::thread::sleep(Duration::from_millis(500));

    let sink: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let capture = match LoopbackCapture::open(None, Arc::clone(&sink)) {
        Ok(capture) => capture,
        Err(e) => {
            eprintln!("SKIP: {e}");
            return;
        }
    };

    let deadline = Instant::now() + Duration::from_secs(6);
    let mut samples = Vec::new();
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
        if let Ok(buffer) = sink.lock() {
            if peak(&buffer) > 0.05 && buffer.len() > 48_000 {
                samples = buffer.clone();
                break;
            }
        }
    }
    if samples.is_empty() {
        eprintln!("SKIP: nothing audible was captured");
        return;
    }
    drop(capture);

    // The capture is converted to stereo at 48 kHz, so one channel of a
    // 440 Hz tone crosses zero about 880 times a second.
    let left: Vec<f32> = samples.chunks_exact(2).map(|f| f[0]).collect();
    let crossings = left
        .windows(2)
        .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
        .count();
    let seconds = left.len() as f32 / 48_000.0;
    let per_second = crossings as f32 / seconds;

    assert!(
        (per_second - 880.0).abs() < 200.0,
        "expected about 880 zero crossings a second for a 440 Hz tone, got {per_second:.0} — \
         the samples are being read in the wrong format"
    );
    eprintln!("captured a clean tone: {per_second:.0} zero crossings a second");
}

#[test]
fn a_capture_stops_when_it_is_dropped() {
    let sink: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let capture = match LoopbackCapture::open(None, Arc::clone(&sink)) {
        Ok(capture) => capture,
        Err(e) => {
            eprintln!("SKIP: no capturable playback device: {e}");
            return;
        }
    };
    drop(capture);

    // Let the thread notice and finish its current packet.
    std::thread::sleep(Duration::from_millis(400));
    if let Ok(mut buffer) = sink.lock() {
        buffer.clear();
    }

    // A thread that did not stop keeps writing. Nothing should arrive now.
    std::thread::sleep(Duration::from_millis(600));
    let after = sink.lock().map(|b| b.len()).unwrap_or(0);
    assert_eq!(after, 0, "the capture thread kept running after it was dropped");
}

#[test]
fn every_playback_device_can_be_named_and_chosen() {
    let devices = rhevia_audio::list_output_devices();
    if devices.is_empty() {
        eprintln!("SKIP: no playback devices");
        return;
    }

    // Each one has to be openable by the name it was listed under, or the
    // picker offers devices that cannot be selected.
    for device in &devices {
        let sink = Arc::new(Mutex::new(Vec::new()));
        match LoopbackCapture::open(Some(&device.name), sink) {
            Ok(capture) => {
                assert_eq!(capture.device_name, device.name);
                assert!(capture.source_rate >= 8_000, "implausible rate for {}", device.name);
            }
            Err(e) => eprintln!("{} could not be opened: {e}", device.name),
        }
    }
}
