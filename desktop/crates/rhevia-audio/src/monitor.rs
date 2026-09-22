//! Hearing the programme.
//!
//! Everything else in this crate takes sound in. This is the one path that
//! sends it back out, to the operator's headphones or speakers, and without
//! it a switcher is unusable: you cannot ride a fader you cannot hear, and
//! you find out a microphone is dead when a viewer tells you.
//!
//! Separate from the master fader on purpose. Turning the monitor down must
//! not turn the stream down, and muting the stream must not leave the
//! operator deaf.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::capture::CaptureError;
use crate::mixer::{db_to_amplitude, CHANNELS, SAMPLE_RATE};

/// How much sound may wait to be played.
///
/// A quarter of a second. Longer and the operator hears the programme behind
/// the picture, which is worse than an occasional gap; shorter and an ordinary
/// scheduling hiccup becomes a click.
const MAX_BUFFERED_FRAMES: usize = SAMPLE_RATE as usize / 4;

/// How much sound is held in hand before playing begins.
///
/// The engine hands over one block per tick and a tick is not perfectly
/// even — the loop sleeps to fill the frame budget and the sleep rounds up.
/// Playing the instant the first samples arrive means the ring is empty
/// again by the next callback, and an empty ring is filled with silence:
/// thirty tiny gaps a second, heard as a steady crackle over everything.
/// Two ticks in hand rides out ordinary jitter and costs under a
/// fifteenth of a second of delay.
const PRIME_FRAMES: usize = SAMPLE_RATE as usize / 15;

/// A playback device the operator can listen on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorDevice {
    pub name: String,
    pub is_default: bool,
}

/// Every device the programme can be played through.
pub fn list_output_devices() -> Vec<MonitorDevice> {
    let host = cpal::default_host();
    let default_name =
        host.default_output_device().and_then(|d| d.name().ok()).unwrap_or_default();

    match host.output_devices() {
        Ok(devices) => devices
            .filter_map(|d| d.name().ok())
            .map(|name| MonitorDevice { is_default: name == default_name, name })
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "could not enumerate playback devices");
            Vec::new()
        }
    }
}

/// Plays the programme.
pub struct AudioMonitor {
    /// Interleaved stereo at the engine rate, waiting to be played.
    pending: Arc<Mutex<VecDeque<f32>>>,
    running: Arc<AtomicBool>,
    /// Linear amplitude, applied in the callback so a change is heard at once.
    gain: Arc<Mutex<f32>>,
    /// False until there is a cushion of sound to play from, and again after
    /// the cushion is exhausted. Held here only to keep it alive for the
    /// callback, which is the one place that reads it.
    #[allow(dead_code)]
    flowing: Arc<AtomicBool>,
    /// How many blocks were thrown away because the cushion had grown past
    /// what it is allowed to hold. Each one jumps the waveform, which is
    /// heard as a pop.
    dropped: Arc<AtomicU64>,
    /// How many times the callback asked for sound and found none.
    ///
    /// Exposed because this is the difference between "the audio is fine" and
    /// "the audio crackles", and it is not otherwise visible from outside.
    starved: Arc<AtomicU64>,
    pub device_name: String,
    pub device_rate: u32,
    pub device_channels: u16,
}

impl AudioMonitor {
    /// Opens `device_name`, or the system default when it is None.
    ///
    /// The stream is built on its own thread and stays there, because a cpal
    /// stream belongs to the thread that made it. The outcome comes back, so
    /// a device that will not open says so rather than playing nothing.
    pub fn open(device_name: Option<&str>) -> Result<Self, CaptureError> {
        let pending: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
        let running = Arc::new(AtomicBool::new(true));
        let gain = Arc::new(Mutex::new(1.0f32));
        let playing = Arc::new(AtomicBool::new(false));
        let gaps = Arc::new(AtomicU64::new(0));
        let spilled = Arc::new(AtomicU64::new(0));

        let source = Arc::clone(&pending);
        let alive = Arc::clone(&running);
        let level = Arc::clone(&gain);
        let flowing = Arc::clone(&playing);
        let starved = Arc::clone(&gaps);
        let wanted = device_name.map(|s| s.to_string());
        let (opened, opened_rx) = mpsc::channel::<Result<(String, u32, u16), String>>();

        std::thread::Builder::new()
            .name("rhevia-audio-monitor".into())
            .spawn(move || {
                let host = cpal::default_host();
                let device = match wanted.as_deref() {
                    Some(name) => host
                        .output_devices()
                        .ok()
                        .and_then(|mut d| d.find(|d| d.name().map(|n| n == name).unwrap_or(false))),
                    None => host.default_output_device(),
                };
                let Some(device) = device else {
                    let _ = opened.send(Err(format!(
                        "no playback device named {}",
                        wanted.unwrap_or_else(|| "default".into())
                    )));
                    return;
                };

                let name = device.name().unwrap_or_else(|_| "unknown".into());
                let config = match device.default_output_config() {
                    Ok(config) => config,
                    Err(e) => {
                        let _ = opened.send(Err(format!("{name}: {e}")));
                        return;
                    }
                };
                let rate = config.sample_rate().0;
                let channels = config.channels();

                // Only float output is built. Every device on a machine that
                // can run this offers it, and converting in the callback for
                // the others would cost more than it is worth.
                if config.sample_format() != cpal::SampleFormat::F32 {
                    let _ = opened.send(Err(format!(
                        "{name}: needs {:?} output, which is not supported",
                        config.sample_format()
                    )));
                    return;
                }

                let built = device.build_output_stream(
                    &config.into(),
                    move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                        let amplitude = level.lock().map(|g| *g).unwrap_or(1.0);
                        fill(out, channels as usize, rate, &source, amplitude, &flowing, &starved);
                    },
                    |e| tracing::warn!(error = %e, "monitor output error"),
                    None,
                );

                let stream = match built {
                    Ok(stream) => stream,
                    Err(e) => {
                        let _ = opened.send(Err(format!("{name}: {e}")));
                        return;
                    }
                };
                if let Err(e) = stream.play() {
                    let _ = opened.send(Err(format!("{name}: {e}")));
                    return;
                }

                let _ = opened.send(Ok((name, rate, channels)));

                // The stream ends when this returns.
                while alive.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
            .map_err(|e| CaptureError::Start(e.to_string()))?;

        match opened_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok((device_name, device_rate, device_channels))) => Ok(Self {
                pending,
                running,
                gain,
                flowing: playing,
                starved: gaps,
                dropped: spilled,
                device_name,
                device_rate,
                device_channels,
            }),
            Ok(Err(e)) => {
                running.store(false, Ordering::Relaxed);
                Err(CaptureError::Open(e))
            }
            Err(_) => {
                running.store(false, Ordering::Relaxed);
                Err(CaptureError::Open("the playback device did not respond".into()))
            }
        }
    }

    /// How many times the device asked for sound and there was none ready.
    ///
    /// Each one is a gap, and gaps arriving at the tick rate are heard as a
    /// crackle rather than as silence. Zero is the only acceptable number
    /// once a show is running.
    pub fn starved(&self) -> u64 {
        self.starved.load(Ordering::Relaxed)
    }

    /// Hands a block of the master bus over to be played.
    ///
    /// Never blocks and never waits: this is called from the engine loop, and
    /// a monitor that stalls must cost sound, not the programme.
    pub fn play(&self, samples: &[f32]) {
        let Ok(mut pending) = self.pending.lock() else { return };
        pending.extend(samples.iter().copied());

        // Anything beyond the budget is dropped from the front. Keeping it
        // would mean the operator hears the show later and later as the
        // evening goes on.
        let limit = MAX_BUFFERED_FRAMES * CHANNELS;
        if pending.len() > limit {
            let excess = pending.len() - limit;
            pending.drain(..excess);
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// How many times sound had to be thrown away to stop the cushion
    /// growing. Each one is a jump in the middle of the waveform.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Sets the listening level, which is nothing to do with the stream.
    pub fn set_gain_db(&self, db: f32) {
        if let Ok(mut gain) = self.gain.lock() {
            *gain = if db <= crate::mixer::SILENCE_DB { 0.0 } else { db_to_amplitude(db) };
        }
    }

    /// How much sound is waiting, in frames. For diagnosing a stutter.
    pub fn buffered_frames(&self) -> usize {
        self.pending.lock().map(|p| p.len() / CHANNELS).unwrap_or(0)
    }
}

impl Drop for AudioMonitor {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

/// Fills one output buffer from what is waiting.
///
/// Separated from the stream so it can be tested: this is where a wrong
/// channel count or rate turns the programme into chipmunks, and an audio
/// callback is not somewhere a mistake is easy to see.
pub(crate) fn fill(
    out: &mut [f32],
    device_channels: usize,
    device_rate: u32,
    source: &Arc<Mutex<VecDeque<f32>>>,
    gain: f32,
    flowing: &AtomicBool,
    starved: &AtomicU64,
) {
    out.fill(0.0);
    if device_channels == 0 {
        return;
    }

    let Ok(mut pending) = source.lock() else { return };
    let frames = out.len() / device_channels;

    // Wait until there is a cushion before starting, and wait again after
    // running dry. Playing whatever has arrived the moment it arrives turns
    // one gap into a gap in every callback, which is a crackle rather than a
    // pause — and a crackle is the harder of the two to listen through.
    if !flowing.load(Ordering::Relaxed) {
        if pending.len() / CHANNELS < PRIME_FRAMES {
            return;
        }
        flowing.store(true, Ordering::Relaxed);
    }

    // How many source frames one output frame is worth. A device running at
    // 44.1 kHz needs 0.919 of a frame each time, and ignoring that plays the
    // whole show slightly sharp.
    let step = SAMPLE_RATE as f64 / device_rate.max(1) as f64;

    let mut position = 0.0f64;
    for frame in 0..frames {
        let index = position as usize;
        let at = index * CHANNELS;
        if at + 1 >= pending.len() {
            // Nothing left. Silence is better than repeating the last block,
            // which sounds like a stutter rather than a gap. Counted, and the
            // cushion rebuilt before playing resumes.
            starved.fetch_add(1, Ordering::Relaxed);
            flowing.store(false, Ordering::Relaxed);
            break;
        }

        // Interpolated between neighbouring frames rather than snapped to
        // the nearest one. On a device at the engine's own rate this costs
        // nothing -- the fraction is zero and it reduces to a copy -- but on
        // a 44.1 kHz device, snapping drops or repeats a sample in an
        // irregular pattern, and that is heard as grit and clicks over
        // everything rather than as a change of pitch.
        let fraction = (position - index as f64) as f32;
        // The frame after, where there is one. On the last frame in hand
        // there is nothing to interpolate towards, so it is held -- which
        // costs at most one frame at the very end of what has arrived, and
        // never happens at all on a device running at the engine's rate.
        let next = at + CHANNELS;
        let (ahead_left, ahead_right) = if next + 1 < pending.len() {
            (pending[next], pending[next + 1])
        } else {
            (pending[at], pending[at + 1])
        };
        let left = (pending[at] + (ahead_left - pending[at]) * fraction) * gain;
        let right = (pending[at + 1] + (ahead_right - pending[at + 1]) * fraction) * gain;

        let base = frame * device_channels;
        for channel in 0..device_channels {
            // Stereo out of a stereo master; a mono device gets the left, and
            // anything wider gets the pair repeated rather than silence in
            // the extra channels.
            out[base + channel] = if device_channels == 1 {
                (left + right) * 0.5
            } else if channel % 2 == 0 {
                left
            } else {
                right
            };
        }
        position += step;
    }

    // Everything consumed is dropped, including the fractional remainder, so
    // the next callback starts where this one stopped.
    let consumed = (position as usize * CHANNELS).min(pending.len());
    pending.drain(..consumed);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A monitor already past its priming, for tests about what it plays
    /// rather than about when it starts.
    fn playing() -> AtomicBool {
        AtomicBool::new(true)
    }

    fn ring(samples: &[f32]) -> Arc<Mutex<VecDeque<f32>>> {
        Arc::new(Mutex::new(samples.iter().copied().collect()))
    }

    #[test]
    fn a_device_at_44_1_khz_is_played_smoothly_rather_than_snapped() {
        // Not every device runs at the engine's rate, and a Bluetooth headset
        // often does not. Snapping to the nearest frame drops or repeats a
        // sample in an irregular pattern, which is not heard as a change of
        // pitch -- it is heard as grit and clicks over everything.
        //
        // A clean tone in must come out clean, measured as the error against
        // the same tone at the device's rate.
        const HZ: f32 = 1000.0;
        const DEVICE: u32 = 44_100;

        let frames = SAMPLE_RATE as usize / 4;
        let mut samples = Vec::with_capacity(frames * CHANNELS);
        for i in 0..frames {
            let v = 0.5
                * (std::f32::consts::TAU * HZ * i as f32 / SAMPLE_RATE as f32).sin();
            samples.push(v);
            samples.push(v);
        }
        let source = ring(&samples);

        let out_frames = DEVICE as usize / 5;
        let mut out = vec![0.0f32; out_frames * 2];
        fill(&mut out, 2, DEVICE, &source, 1.0, &playing(), &AtomicU64::new(0));

        // What the tone should be at the device's rate.
        let mut worst = 0.0f32;
        for frame in 0..out_frames {
            let want = 0.5
                * (std::f32::consts::TAU * HZ * frame as f32 / DEVICE as f32).sin();
            worst = worst.max((out[frame * 2] - want).abs());
        }
        eprintln!("  worst error against a clean 1 kHz tone: {worst:.4}");

        // Snapping to the nearest frame gives an error of roughly the step
        // between samples -- about 0.065 for this tone. Interpolating is an
        // order of magnitude closer.
        assert!(
            worst < 0.02,
            "the resampled tone is {worst:.4} away from the real one, which is audible grit"
        );
    }

    #[test]
    fn a_device_at_the_engine_rate_is_not_resampled_at_all() {
        // The common case, and it must stay exact: interpolating with a zero
        // fraction has to reduce to a copy rather than to almost-a-copy.
        let source = ring(&[0.1, -0.2, 0.3, -0.4, 0.5, -0.6, 0.7, -0.8]);
        let mut out = vec![0.0f32; 6];
        fill(&mut out, 2, SAMPLE_RATE, &source, 1.0, &playing(), &AtomicU64::new(0));
        assert_eq!(out, vec![0.1, -0.2, 0.3, -0.4, 0.5, -0.6]);
    }

    #[test]
    fn a_source_running_slightly_slow_is_not_played_as_a_crackle() {
        // Reported as "pori pori" -- a steady crackle over sound that is
        // clean in the file. The engine was handing over a fixed block per
        // tick while running at 29.7 ticks a second, so the card was asked
        // for 48,000 frames a second and given about 47,600. Every callback
        // ran a little short, and a gap in every callback is a crackle.
        //
        // This feeds the monitor at that same deficit and counts the gaps.
        const CALLBACK: usize = 480; // a hundredth of a second
        let source = ring(&[]);
        let flowing = AtomicBool::new(false);
        let starved = AtomicU64::new(0);
        let mut out = vec![0.0f32; CALLBACK * CHANNELS];

        // Two hundred callbacks, fed 0.8% short each time.
        let short = (CALLBACK as f64 * 0.992) as usize;
        let mut played = 0;
        for tick in 0..200 {
            {
                let mut pending = source.lock().unwrap();
                for i in 0..short {
                    let phase = (tick * short + i) as f32 * 0.01;
                    pending.push_back(phase.sin());
                    pending.push_back(phase.sin());
                }
            }
            fill(&mut out, 2, SAMPLE_RATE, &source, 1.0, &flowing, &starved);
            if out.iter().any(|&s| s != 0.0) {
                played += 1;
            }
        }

        let gaps = starved.load(Ordering::Relaxed);
        eprintln!("  {gaps} gaps over 200 callbacks, {played} of them played");
        // Falling behind by 0.8% has to cost something -- the sound cannot be
        // invented -- but it must be a handful of pauses, not a gap in every
        // callback. Before the cushion this was 199.
        assert!(gaps < 20, "{gaps} gaps in 200 callbacks is a crackle, not a pause");
        assert!(played > 150, "only {played} of 200 callbacks played anything");
    }

    #[test]
    fn nothing_is_played_until_there_is_a_cushion_to_play_from() {
        // Starting on the first samples that arrive empties the ring
        // immediately, and then every callback after it runs dry.
        let source = ring(&[0.5; 64]);
        let flowing = AtomicBool::new(false);
        let starved = AtomicU64::new(0);
        let mut out = vec![0.0f32; 128];

        fill(&mut out, 2, SAMPLE_RATE, &source, 1.0, &flowing, &starved);
        assert!(out.iter().all(|&s| s == 0.0), "played before it had anything in hand");
        assert_eq!(
            source.lock().unwrap().len(),
            64,
            "waiting should not consume what it is waiting for"
        );

        // Once the cushion is there, it plays.
        {
            let mut pending = source.lock().unwrap();
            for _ in 0..PRIME_FRAMES * CHANNELS {
                pending.push_back(0.5);
            }
        }
        fill(&mut out, 2, SAMPLE_RATE, &source, 1.0, &flowing, &starved);
        assert!(out.iter().any(|&s| s != 0.0), "it never started playing");
    }

    #[test]
    fn a_stereo_device_at_the_engine_rate_gets_the_samples_unchanged() {
        let source = ring(&[0.1, 0.2, 0.3, 0.4]);
        let mut out = vec![0.0f32; 4];
        fill(&mut out, 2, SAMPLE_RATE, &source, 1.0, &playing(), &AtomicU64::new(0));

        assert_eq!(out, vec![0.1, 0.2, 0.3, 0.4]);
        assert_eq!(source.lock().unwrap().len(), 0, "everything played should be consumed");
    }

    #[test]
    fn the_listening_level_is_applied() {
        let source = ring(&[1.0, 1.0]);
        let mut out = vec![0.0f32; 2];
        fill(&mut out, 2, SAMPLE_RATE, &source, 0.5, &playing(), &AtomicU64::new(0));
        assert_eq!(out, vec![0.5, 0.5]);
    }

    #[test]
    fn running_out_of_sound_gives_silence_rather_than_a_repeat() {
        // Repeating the last block sounds like a stutter; a gap sounds like a
        // gap, and the second is easier to diagnose.
        let source = ring(&[0.7, 0.7]);
        let mut out = vec![0.0f32; 8];
        fill(&mut out, 2, SAMPLE_RATE, &source, 1.0, &playing(), &AtomicU64::new(0));

        assert_eq!(out[0], 0.7);
        assert_eq!(out[1], 0.7);
        assert!(out[2..].iter().all(|&s| s == 0.0), "should be silent after it runs out");
    }

    #[test]
    fn a_device_at_a_different_rate_is_resampled() {
        // A 96 kHz device needs two output frames per source frame. Without
        // this the programme plays at half speed.
        //
        // The frame in between is the average of its neighbours, not the
        // first one held twice. Holding is what this used to do, and on a
        // device whose rate is not a neat multiple it drops and repeats
        // samples in an irregular pattern -- grit over everything rather
        // than a change of pitch.
        let source = ring(&[0.5, 0.5, 0.9, 0.9]);
        let mut out = vec![0.0f32; 8];
        fill(&mut out, 2, SAMPLE_RATE * 2, &source, 1.0, &playing(), &AtomicU64::new(0));

        assert_eq!(out[0], 0.5);
        assert!(
            (out[2] - 0.7).abs() < 1e-6,
            "the frame between 0.5 and 0.9 should be 0.7, not {}",
            out[2]
        );
        assert_eq!(out[4], 0.9);
    }

    #[test]
    fn a_slower_device_consumes_more_than_it_plays() {
        // 24 kHz takes every second source frame.
        let source = ring(&[0.1, 0.1, 0.2, 0.2, 0.3, 0.3, 0.4, 0.4]);
        let mut out = vec![0.0f32; 4];
        fill(&mut out, 2, SAMPLE_RATE / 2, &source, 1.0, &playing(), &AtomicU64::new(0));

        // Half the rate takes every second source frame, so the second
        // output frame is the third source frame, not the second.
        assert_eq!(out[0], 0.1);
        assert_eq!(out[2], 0.3, "every second frame at half the rate");
    }

    #[test]
    fn a_mono_device_hears_both_sides() {
        // Sending only the left would lose anything panned right, which on a
        // monitor is how a missing guest microphone goes unnoticed.
        let source = ring(&[1.0, 0.0]);
        let mut out = vec![0.0f32; 1];
        fill(&mut out, 1, SAMPLE_RATE, &source, 1.0, &playing(), &AtomicU64::new(0));
        assert_eq!(out[0], 0.5);
    }

    #[test]
    fn a_device_with_more_than_two_channels_is_filled_rather_than_left_half_silent() {
        let source = ring(&[0.3, 0.6]);
        let mut out = vec![0.0f32; 4];
        fill(&mut out, 4, SAMPLE_RATE, &source, 1.0, &playing(), &AtomicU64::new(0));
        assert_eq!(out, vec![0.3, 0.6, 0.3, 0.6]);
    }

    #[test]
    fn an_empty_buffer_produces_silence_and_does_not_panic() {
        let source = ring(&[]);
        let mut out = vec![0.5f32; 6];
        fill(&mut out, 2, SAMPLE_RATE, &source, 1.0, &playing(), &AtomicU64::new(0));
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn a_device_reporting_no_channels_is_survived() {
        let source = ring(&[0.1, 0.2]);
        let mut out = vec![0.0f32; 4];
        fill(&mut out, 0, SAMPLE_RATE, &source, 1.0, &playing(), &AtomicU64::new(0));
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn sound_waiting_is_capped_so_the_monitor_does_not_fall_behind() {
        let Ok(monitor) = AudioMonitor::open(None) else {
            eprintln!("SKIP: no playback device");
            return;
        };

        // Ten seconds of audio pushed at once, as would happen if the device
        // stalled. Held in full it would be ten seconds behind the picture.
        for _ in 0..100 {
            monitor.play(&vec![0.1f32; SAMPLE_RATE as usize / 10 * CHANNELS]);
        }
        assert!(
            monitor.buffered_frames() <= MAX_BUFFERED_FRAMES,
            "{} frames waiting, which is more than the budget",
            monitor.buffered_frames()
        );
    }

    #[test]
    fn the_device_actually_takes_the_sound_rather_than_just_accepting_it() {
        // The difference between sound arriving at a device and sound coming
        // out of it cannot be heard from here, but it can be measured: a
        // device that is really playing drains what it is given. One that is
        // open and silent leaves the buffer exactly where it was.
        let Ok(monitor) = AudioMonitor::open(None) else {
            eprintln!("SKIP: no playback device");
            return;
        };

        // Half a second of tone, which is more than one callback's worth.
        let frames = SAMPLE_RATE as usize / 2;
        let block: Vec<f32> = (0..frames * CHANNELS)
            .map(|n| 0.2 * ((n / CHANNELS) as f32 * 0.06).sin())
            .collect();
        monitor.play(&block);

        let started = monitor.buffered_frames();
        assert!(started > 0, "nothing was queued");

        std::thread::sleep(Duration::from_millis(400));
        let left = monitor.buffered_frames();

        assert!(
            left < started,
            "the device took nothing: {started} frames queued, {left} still waiting"
        );
        eprintln!("  played {} of {started} frames in 400 ms", started - left);
    }

    #[test]
    fn a_playback_device_that_is_not_there_is_refused() {
        assert!(AudioMonitor::open(Some("no-such-playback-device-98765")).is_err());
    }

    #[test]
    fn the_default_device_opens_and_reports_itself() {
        match AudioMonitor::open(None) {
            Ok(monitor) => {
                assert!(!monitor.device_name.is_empty());
                assert!(monitor.device_rate >= 8_000);
                assert!(monitor.device_channels >= 1);
                eprintln!(
                    "monitoring on {} at {} Hz, {} channels",
                    monitor.device_name, monitor.device_rate, monitor.device_channels
                );
            }
            Err(e) => eprintln!("SKIP: {e}"),
        }
    }

    #[test]
    fn every_playback_device_can_be_listed() {
        for device in list_output_devices() {
            assert!(!device.name.trim().is_empty());
        }
    }
}
