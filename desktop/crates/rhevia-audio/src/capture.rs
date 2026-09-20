//! Capturing audio from a device.
//!
//! cpal's callback runs on the operating system's audio thread, which must
//! never block. It therefore does the minimum — convert, push, return — and
//! everything else happens when the engine drains the buffer.

use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::mixer::{AudioBuffer, CHANNELS, SAMPLE_RATE};

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("no audio device named {0}")]
    NoSuchDevice(String),
    #[error("the device has no usable input configuration: {0}")]
    NoConfig(String),
    #[error("could not open the audio device: {0}")]
    Open(String),
    #[error("could not start capture: {0}")]
    Start(String),
}

/// Where a channel gets its sound from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeviceKind {
    /// A microphone, line input or USB interface.
    #[default]
    Input,
    /// What the machine is playing, captured from a playback endpoint.
    ///
    /// Windows does not present this as an input device, so it cannot come
    /// through the same path — see [`crate::loopback`].
    SystemAudio,
}

/// An input device the user can choose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    pub name: String,
    pub is_default: bool,
    pub kind: DeviceKind,
}

/// The prefix that marks a device name as a playback endpoint to capture.
///
/// Names are the only handle the engine has on a device, and a microphone can
/// share a name with a playback endpoint, so the kind has to travel with the
/// name rather than beside it.
const SYSTEM_PREFIX: &str = "System audio: ";

/// The name to use when opening a system-audio device.
pub fn system_audio_name(endpoint: &str) -> String {
    format!("{SYSTEM_PREFIX}{endpoint}")
}

/// The playback endpoint a system-audio name refers to, if it is one.
pub fn system_audio_endpoint(name: &str) -> Option<&str> {
    name.strip_prefix(SYSTEM_PREFIX)
}

/// Every capture device currently present.
///
/// Devices appear and disappear as things are plugged in, so this is called
/// again rather than cached.
pub fn list_input_devices() -> Vec<AudioDevice> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_default();

    let mut devices: Vec<AudioDevice> = match host.input_devices() {
        Ok(devices) => devices
            .filter_map(|d| d.name().ok())
            .map(|name| AudioDevice {
                is_default: name == default_name,
                name,
                kind: DeviceKind::Input,
            })
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "could not enumerate audio inputs");
            Vec::new()
        }
    };

    // Playback endpoints, listed alongside the microphones. This is how a show
    // gets music from a browser, sound from a game, or a guest on a call into
    // the mix, and an operator should not have to know it works differently.
    for endpoint in crate::loopback::list_output_devices() {
        devices.push(AudioDevice {
            name: system_audio_name(&endpoint.name),
            is_default: false,
            kind: DeviceKind::SystemAudio,
        });
    }

    devices
}

/// Whichever kind of capture is running. Dropping it stops the capture.
///
/// Held as one type so everything downstream — the mixer, the DSP chain, the
/// meters — sees a channel rather than a kind of device.
// Neither field is ever read: they are held so that dropping the handle drops
// the stream, which is what stops the capture.
#[allow(dead_code)]
enum Backend {
    Device(cpal::Stream),
    System(crate::loopback::LoopbackCapture),
}

/// A running capture. Dropping it stops the stream.
pub struct CaptureHandle {
    /// Kept alive: dropping it ends capture.
    _backend: Backend,
    shared: Arc<Mutex<Vec<f32>>>,
    pub device_name: String,
    pub source_rate: u32,
    pub source_channels: u16,
    pub kind: DeviceKind,
}

impl CaptureHandle {
    /// Opens `device_name`, or the system default when it is None.
    pub fn open(device_name: Option<&str>) -> Result<Self, CaptureError> {
        // A system-audio name is a playback endpoint, which takes a different
        // path entirely.
        if let Some(endpoint) = device_name.and_then(system_audio_endpoint) {
            return Self::open_system_audio(endpoint);
        }

        let host = cpal::default_host();

        let device = match device_name {
            Some(wanted) => host
                .input_devices()
                .map_err(|e| CaptureError::Open(e.to_string()))?
                .find(|d| d.name().map(|n| n == wanted).unwrap_or(false))
                .ok_or_else(|| CaptureError::NoSuchDevice(wanted.to_string()))?,
            None => host
                .default_input_device()
                .ok_or_else(|| CaptureError::NoSuchDevice("default".into()))?,
        };

        let name = device.name().unwrap_or_else(|_| "unknown".into());
        let config = device
            .default_input_config()
            .map_err(|e| CaptureError::NoConfig(e.to_string()))?;
        let source_rate = config.sample_rate().0;
        let source_channels = config.channels();

        let shared: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(
            SAMPLE_RATE as usize * CHANNELS,
        )));
        let sink = Arc::clone(&shared);

        let on_error = |e| tracing::warn!(error = %e, "audio capture error");

        // Convert to interleaved stereo at the engine's rate inside the
        // callback, so the engine only ever sees one format.
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config.into(),
                move |data: &[f32], _| push(&sink, data, source_channels, source_rate),
                on_error,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config.into(),
                move |data: &[i16], _| {
                    let floats: Vec<f32> =
                        data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                    push(&sink, &floats, source_channels, source_rate);
                },
                on_error,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_input_stream(
                &config.into(),
                move |data: &[u16], _| {
                    let floats: Vec<f32> = data
                        .iter()
                        .map(|&s| (s as f32 / u16::MAX as f32) * 2.0 - 1.0)
                        .collect();
                    push(&sink, &floats, source_channels, source_rate);
                },
                on_error,
                None,
            ),
            other => {
                return Err(CaptureError::NoConfig(format!(
                    "unsupported sample format {other:?}"
                )))
            }
        }
        .map_err(|e| CaptureError::Open(e.to_string()))?;

        stream.play().map_err(|e| CaptureError::Start(e.to_string()))?;

        Ok(Self {
            _backend: Backend::Device(stream),
            shared,
            device_name: name,
            source_rate,
            source_channels,
            kind: DeviceKind::Input,
        })
    }

    /// Opens a playback endpoint in loopback mode.
    fn open_system_audio(endpoint: &str) -> Result<Self, CaptureError> {
        let shared: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(
            SAMPLE_RATE as usize * CHANNELS,
        )));
        let capture =
            crate::loopback::LoopbackCapture::open(Some(endpoint), Arc::clone(&shared))?;

        Ok(Self {
            device_name: system_audio_name(&capture.device_name),
            source_rate: capture.source_rate,
            source_channels: capture.source_channels,
            kind: DeviceKind::SystemAudio,
            _backend: Backend::System(capture),
            shared,
        })
    }

    /// Takes up to `frames` stereo frames, padding with silence if the device
    /// has not produced enough yet.
    ///
    /// Padding rather than blocking: a video frame must go out on time, and a
    /// momentary audio underrun is a click, while a stalled render is a
    /// dropped frame on air.
    pub fn take(&self, frames: usize) -> AudioBuffer {
        let wanted = frames * CHANNELS;
        let mut buffer = AudioBuffer::silent(frames);

        if let Ok(mut pending) = self.shared.lock() {
            let available = pending.len().min(wanted);
            buffer.samples[..available].copy_from_slice(&pending[..available]);
            pending.drain(..available);

            // Cap the backlog. If the consumer falls behind, old audio is
            // worthless — holding it only grows latency until the buffer is
            // seconds behind the picture.
            let max_backlog = SAMPLE_RATE as usize * CHANNELS / 2;
            if pending.len() > max_backlog {
                let excess = pending.len() - max_backlog;
                pending.drain(..excess);
                tracing::debug!(dropped = excess, "audio backlog trimmed");
            }
        }

        buffer
    }
}

/// Converts a device block to interleaved stereo at the engine rate.
pub(crate) fn push(sink: &Arc<Mutex<Vec<f32>>>, data: &[f32], channels: u16, rate: u32) {
    let channels = channels.max(1) as usize;
    let frames = data.len() / channels;
    if frames == 0 {
        return;
    }

    // Nearest-neighbour rate conversion. Audible on a large ratio shift, and
    // good enough while every practical device runs at 44.1 or 48 kHz; a
    // proper resampler belongs here before this ships.
    let ratio = SAMPLE_RATE as f64 / rate as f64;
    let out_frames = ((frames as f64) * ratio).round() as usize;

    let mut converted = Vec::with_capacity(out_frames * CHANNELS);
    for out_frame in 0..out_frames {
        let src_frame = ((out_frame as f64) / ratio).floor() as usize;
        let src_frame = src_frame.min(frames - 1);
        let base = src_frame * channels;

        let (left, right) = if channels == 1 {
            // Mono is duplicated rather than panned, so a mono microphone
            // arrives centred instead of only in the left ear.
            (data[base], data[base])
        } else {
            (data[base], data[base + 1])
        };
        converted.push(left);
        converted.push(right);
    }

    if let Ok(mut pending) = sink.lock() {
        pending.extend_from_slice(&converted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerating_devices_does_not_panic_without_hardware() {
        // CI machines have no audio device; this must return empty, not crash.
        let _ = list_input_devices();
    }

    #[test]
    fn mono_is_duplicated_to_both_channels() {
        let sink = Arc::new(Mutex::new(Vec::new()));
        push(&sink, &[0.5, -0.25], 1, SAMPLE_RATE);
        let out = sink.lock().unwrap().clone();
        assert_eq!(out, vec![0.5, 0.5, -0.25, -0.25], "mono must arrive centred");
    }

    #[test]
    fn stereo_passes_through_unchanged_at_the_engine_rate() {
        let sink = Arc::new(Mutex::new(Vec::new()));
        push(&sink, &[0.1, 0.2, 0.3, 0.4], 2, SAMPLE_RATE);
        let out = sink.lock().unwrap().clone();
        assert_eq!(out, vec![0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn a_lower_device_rate_produces_more_frames() {
        // 24 kHz in, 48 kHz out: twice as many frames.
        let sink = Arc::new(Mutex::new(Vec::new()));
        push(&sink, &[0.5, 0.5, 0.6, 0.6], 2, SAMPLE_RATE / 2);
        let out = sink.lock().unwrap().clone();
        assert_eq!(out.len(), 8, "expected 4 stereo frames, got {}", out.len() / 2);
    }

    #[test]
    fn extra_device_channels_are_reduced_to_the_first_two() {
        // A 4-channel interface must not shift the picture's audio sideways.
        let sink = Arc::new(Mutex::new(Vec::new()));
        push(&sink, &[0.1, 0.2, 0.9, 0.9], 4, SAMPLE_RATE);
        let out = sink.lock().unwrap().clone();
        assert_eq!(out, vec![0.1, 0.2]);
    }

    #[test]
    fn an_empty_block_is_ignored() {
        let sink = Arc::new(Mutex::new(Vec::new()));
        push(&sink, &[], 2, SAMPLE_RATE);
        assert!(sink.lock().unwrap().is_empty());
    }
}
