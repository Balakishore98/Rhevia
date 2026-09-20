//! System audio capture, through WASAPI loopback.
//!
//! Capturing what the machine is playing is how a show gets music from a
//! browser, sound from a game, or a guest on a call into the mix. Windows does
//! not expose it as an input device, so it cannot come through the same path
//! as a microphone: it is a *render* endpoint opened in loopback mode, which
//! only WASAPI offers.
//!
//! The capture thread owns its COM apartment and its client for its whole
//! life. WASAPI objects belong to the thread that created them, so nothing
//! here can be moved between threads afterwards.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use windows::core::PCWSTR;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    DEVICE_STATE_ACTIVE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED, STGM_READ,
};

use crate::capture::CaptureError;

/// Format tags, which the mix format reports as one of two things.
const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// How long a buffer WASAPI is asked for, in 100-nanosecond units.
///
/// 200 ms. Larger than needed on purpose: this is the amount of audio that can
/// accumulate before the capture thread has to have run, and overrunning it
/// costs a gap in the sound.
const BUFFER_DURATION: i64 = 2_000_000;

/// A playback endpoint whose output can be captured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDevice {
    pub name: String,
    pub is_default: bool,
}

/// Every playback device whose output can be captured.
pub fn list_output_devices() -> Vec<OutputDevice> {
    // Its own apartment: this may be called from any thread, and the
    // enumerator does not outlive the call.
    let _com = ComApartment::enter();

    unsafe {
        let Ok(enumerator) =
            CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
        else {
            return Vec::new();
        };

        let default_id = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .ok()
            .and_then(|d| d.GetId().ok())
            .map(|id| id.to_string().unwrap_or_default())
            .unwrap_or_default();

        let Ok(collection) = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) else {
            return Vec::new();
        };
        let count = collection.GetCount().unwrap_or(0);

        let mut devices = Vec::new();
        for index in 0..count {
            let Ok(device) = collection.Item(index) else { continue };
            let Some(name) = friendly_name(&device) else { continue };
            let id = device.GetId().ok().map(|i| i.to_string().unwrap_or_default());

            devices.push(OutputDevice {
                is_default: id.as_deref() == Some(default_id.as_str()),
                name,
            });
        }
        devices
    }
}

/// Reads a device's display name from its property store.
unsafe fn friendly_name(device: &windows::Win32::Media::Audio::IMMDevice) -> Option<String> {
    let store = device.OpenPropertyStore(STGM_READ).ok()?;
    let value = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
    let wide = PropVariantToStringAlloc(&value).ok()?;
    let name = wide.to_string().ok();
    CoTaskMemFree(Some(wide.0 as *const _));
    name.filter(|n| !n.trim().is_empty())
}

/// A running loopback capture. Dropping it stops the thread.
pub struct LoopbackCapture {
    shared: Arc<Mutex<Vec<f32>>>,
    running: Arc<AtomicBool>,
    pub device_name: String,
    pub source_rate: u32,
    pub source_channels: u16,
}

impl LoopbackCapture {
    /// Starts capturing what `device_name` is playing, or the default
    /// playback device when it is None.
    ///
    /// The client is created on the capture thread and the outcome is sent
    /// back, because a WASAPI client belongs to the thread that made it. That
    /// keeps the usual guarantee: a device that will not open says so now
    /// rather than appearing to work and producing silence.
    pub fn open(
        device_name: Option<&str>,
        sink: Arc<Mutex<Vec<f32>>>,
    ) -> Result<Self, CaptureError> {
        let running = Arc::new(AtomicBool::new(true));
        let alive = Arc::clone(&running);
        let wanted = device_name.map(|s| s.to_string());
        let shared = Arc::clone(&sink);
        let (opened, opened_rx) = mpsc::channel::<Result<(String, u32, u16), String>>();

        std::thread::Builder::new()
            .name("rhevia-system-audio".into())
            .spawn(move || {
                if let Err(e) = run(wanted, shared, alive, &opened) {
                    // Only reported if opening had not already succeeded; a
                    // send to a dropped receiver is harmless.
                    let _ = opened.send(Err(e));
                }
            })
            .map_err(|e| CaptureError::Start(e.to_string()))?;

        match opened_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok((name, rate, channels))) => Ok(Self {
                shared: sink,
                running,
                device_name: name,
                source_rate: rate,
                source_channels: channels,
            }),
            Ok(Err(e)) => {
                running.store(false, Ordering::Relaxed);
                Err(CaptureError::Open(e))
            }
            Err(_) => {
                running.store(false, Ordering::Relaxed);
                Err(CaptureError::Open("the audio endpoint did not respond".into()))
            }
        }
    }

    /// The buffer this capture is filling.
    pub fn sink(&self) -> Arc<Mutex<Vec<f32>>> {
        Arc::clone(&self.shared)
    }
}

impl Drop for LoopbackCapture {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

/// The capture thread: opens the endpoint, then pulls packets until stopped.
fn run(
    wanted: Option<String>,
    sink: Arc<Mutex<Vec<f32>>>,
    alive: Arc<AtomicBool>,
    opened: &mpsc::Sender<Result<(String, u32, u16), String>>,
) -> Result<(), String> {
    let _com = ComApartment::enter();

    unsafe {
        let enumerator =
            CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| format!("no audio endpoints: {e}"))?;

        // Named device, or the default playback endpoint.
        let device = match &wanted {
            Some(name) => {
                let collection = enumerator
                    .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
                    .map_err(|e| e.to_string())?;
                let count = collection.GetCount().unwrap_or(0);
                let mut found = None;
                for index in 0..count {
                    let Ok(candidate) = collection.Item(index) else { continue };
                    if friendly_name(&candidate).as_deref() == Some(name.as_str()) {
                        found = Some(candidate);
                        break;
                    }
                }
                found.ok_or_else(|| format!("no playback device named {name}"))?
            }
            None => enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(|e| format!("no default playback device: {e}"))?,
        };

        let name = friendly_name(&device).unwrap_or_else(|| "System audio".to_string());

        let client: IAudioClient =
            device.Activate(CLSCTX_ALL, None).map_err(|e| format!("{name}: {e}"))?;

        // The shared-mode mix format is the only one loopback accepts; asking
        // for anything else fails rather than converting.
        let format = client.GetMixFormat().map_err(|e| format!("{name}: {e}"))?;
        let rate = (*format).nSamplesPerSec;
        let channels = (*format).nChannels;
        let bits = (*format).wBitsPerSample;
        let is_float = mix_format_is_float(format);

        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                BUFFER_DURATION,
                0,
                format,
                None,
            )
            .map_err(|e| format!("{name}: {e}"))?;

        let capture: IAudioCaptureClient =
            client.GetService().map_err(|e| format!("{name}: {e}"))?;
        client.Start().map_err(|e| format!("{name}: {e}"))?;

        // Opened. Anything after this point is a fault during capture, not a
        // failure to start.
        let _ = opened.send(Ok((name.clone(), rate, channels)));

        let frame_bytes = (channels as usize) * (bits as usize / 8);
        let mut scratch: Vec<f32> = Vec::with_capacity(4096);

        while alive.load(Ordering::Relaxed) {
            let mut available = capture.GetNextPacketSize().unwrap_or(0);
            if available == 0 {
                // Nothing ready. A short sleep rather than a spin: this thread
                // has nothing useful to do and a busy loop would cost a core.
                std::thread::sleep(Duration::from_millis(4));
                continue;
            }

            while available > 0 {
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut frames: u32 = 0;
                let mut flags: u32 = 0;

                if capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                    .is_err()
                {
                    break;
                }

                scratch.clear();
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                    // Windows signals silence by flag rather than by writing
                    // zeros, so the zeros have to be produced here. Skipping
                    // the block instead would make the sound run early.
                    scratch.resize(frames as usize * channels as usize, 0.0);
                } else {
                    let bytes =
                        std::slice::from_raw_parts(data, frames as usize * frame_bytes);
                    decode_into(bytes, bits, is_float, &mut scratch);
                }

                crate::capture::push(&sink, &scratch, channels, rate);
                let _ = capture.ReleaseBuffer(frames);

                available = capture.GetNextPacketSize().unwrap_or(0);
            }
        }

        let _ = client.Stop();
    }
    Ok(())
}

/// Whether the mix format carries floats rather than integers.
///
/// A shared-mode mix format is almost always 32-bit float, but it is reported
/// through an extensible header whose tag says only "extensible" — the real
/// answer is in the sub-format, and reading the tag alone gets it wrong.
unsafe fn mix_format_is_float(format: *const WAVEFORMATEX) -> bool {
    match (*format).wFormatTag {
        WAVE_FORMAT_IEEE_FLOAT => true,
        WAVE_FORMAT_EXTENSIBLE => {
            let extensible = format as *const WAVEFORMATEXTENSIBLE;
            // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT differs from the PCM subtype
            // only in its first field, so comparing that is enough.
            (*extensible).SubFormat.data1 == WAVE_FORMAT_IEEE_FLOAT as u32
        }
        _ => false,
    }
}

/// Converts a raw endpoint buffer into f32 samples.
///
/// Public within the crate so the conversion can be tested without an audio
/// device: it is the part that silently produces noise when it is wrong.
pub(crate) fn decode_into(bytes: &[u8], bits: u16, is_float: bool, out: &mut Vec<f32>) {
    match (is_float, bits) {
        (true, 32) => {
            for chunk in bytes.chunks_exact(4) {
                out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
        }
        (false, 16) => {
            for chunk in bytes.chunks_exact(2) {
                let value = i16::from_le_bytes([chunk[0], chunk[1]]);
                out.push(value as f32 / i16::MAX as f32);
            }
        }
        (false, 32) => {
            for chunk in bytes.chunks_exact(4) {
                let value = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                out.push(value as f32 / i32::MAX as f32);
            }
        }
        (false, 24) => {
            // Three bytes, sign extended into the top of an i32 so the shift
            // does the extension rather than a branch.
            for chunk in bytes.chunks_exact(3) {
                let value = i32::from_le_bytes([0, chunk[0], chunk[1], chunk[2]]);
                out.push(value as f32 / i32::MAX as f32);
            }
        }
        _ => {
            // An unexpected format is silence rather than noise. Feeding
            // misread bytes to the mixer would put a loud buzz on air.
            out.resize(out.len() + bytes.len() / (bits.max(8) as usize / 8), 0.0);
        }
    }
}

/// A COM apartment held for the life of a scope.
struct ComApartment {
    /// False when COM was already initialised on this thread, in which case
    /// uninitialising it would break whoever did.
    owned: bool,
}

impl ComApartment {
    fn enter() -> Self {
        // S_FALSE means this thread was already in an apartment. That is not
        // an error, but it does mean the balance belongs to someone else.
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self { owned: result.is_ok() }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.owned {
            unsafe { CoUninitialize() };
        }
    }
}

/// Unused, but kept so the import is obviously deliberate.
#[allow(dead_code)]
fn _pcwstr(_: PCWSTR) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_playback_devices_does_not_panic() {
        // A machine with no sound card has none. That is an empty list, not a
        // crash.
        for device in list_output_devices() {
            assert!(!device.name.trim().is_empty(), "a device with no name cannot be chosen");
        }
    }

    #[test]
    fn at_most_one_playback_device_is_the_default() {
        let devices = list_output_devices();
        let defaults = devices.iter().filter(|d| d.is_default).count();
        assert!(defaults <= 1, "{defaults} devices claimed to be the default");
    }

    #[test]
    fn float_samples_pass_through_unchanged() {
        let mut out = Vec::new();
        let bytes: Vec<u8> = [0.5f32, -0.25, 1.0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        decode_into(&bytes, 32, true, &mut out);

        assert_eq!(out.len(), 3);
        assert!((out[0] - 0.5).abs() < 1e-6);
        assert!((out[1] + 0.25).abs() < 1e-6);
        assert!((out[2] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn sixteen_bit_integers_are_scaled_to_full_range() {
        let mut out = Vec::new();
        let bytes: Vec<u8> = [i16::MAX, 0, i16::MIN + 1]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        decode_into(&bytes, 16, false, &mut out);

        assert!((out[0] - 1.0).abs() < 1e-4, "full scale should reach 1.0");
        assert!(out[1].abs() < 1e-6);
        assert!((out[2] + 1.0).abs() < 1e-4);
    }

    #[test]
    fn twenty_four_bit_integers_are_scaled_to_full_range() {
        // Packed three bytes at a time, which is the format a lot of audio
        // interfaces report. Read as anything else it becomes loud noise.
        let mut out = Vec::new();
        // 0x7FFFFF is full scale positive; 0x000000 is silence.
        let bytes: Vec<u8> = vec![0xFF, 0xFF, 0x7F, 0x00, 0x00, 0x00];
        decode_into(&bytes, 24, false, &mut out);

        assert_eq!(out.len(), 2);
        assert!((out[0] - 1.0).abs() < 1e-3, "expected full scale, got {}", out[0]);
        assert!(out[1].abs() < 1e-6);
    }

    #[test]
    fn a_format_that_is_not_understood_produces_silence_not_noise() {
        // Misreading the bytes would put a loud buzz on air, which is far
        // worse than a channel that is quiet.
        let mut out = Vec::new();
        decode_into(&[1, 2, 3, 4, 5, 6, 7, 8], 8, false, &mut out);
        assert!(out.iter().all(|&s| s == 0.0), "an unknown format leaked noise");
    }

    #[test]
    fn a_short_buffer_is_not_read_past() {
        // chunks_exact stops before an incomplete sample rather than reading
        // whatever is next in memory.
        let mut out = Vec::new();
        decode_into(&[0, 0, 0], 32, true, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn opening_a_playback_device_that_is_not_there_is_refused() {
        let sink = Arc::new(Mutex::new(Vec::new()));
        let result = LoopbackCapture::open(Some("no-such-playback-device-12345"), sink);
        assert!(result.is_err(), "a missing device must fail rather than produce silence");
    }

    #[test]
    fn the_default_playback_device_can_be_captured() {
        let sink = Arc::new(Mutex::new(Vec::new()));
        match LoopbackCapture::open(None, sink) {
            Ok(capture) => {
                assert!(!capture.device_name.is_empty());
                assert!(capture.source_rate >= 8_000, "implausible rate");
                assert!(capture.source_channels >= 1);
            }
            // A machine with no playback device is a legitimate outcome.
            Err(e) => eprintln!("SKIP: no capturable playback device: {e}"),
        }
    }
}
