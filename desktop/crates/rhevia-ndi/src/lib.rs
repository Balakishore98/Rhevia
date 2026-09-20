//! NDI send and receive.
//!
//! NDI is how a production moves video between machines on a local network:
//! a graphics machine, a replay machine, another switcher. Rhevia both
//! receives sources and publishes its own programme as one.
//!
//! The NDI SDK is not vendored here. This binds to the runtime the user has
//! already installed — the arrangement OBS uses — so there is nothing in this
//! tree that carries the SDK's licence. If the runtime is absent, NDI is
//! reported as unavailable and everything else works unchanged.

pub mod sys;

use std::ffi::{CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;

use rhevia_engine::Frame;

/// Sample rate and channel count everything downstream runs at.
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;

#[derive(Debug, thiserror::Error)]
pub enum NdiError {
    #[error("{0}")]
    Unavailable(String),
    #[error("this machine cannot run NDI")]
    UnsupportedCpu,
    #[error("no NDI source named {0}")]
    NoSuchSource(String),
    #[error("could not start NDI: {0}")]
    Start(String),
}

/// The loaded runtime, or the reason there is none.
///
/// Loaded once and shared: the library keeps global state and initialising it
/// repeatedly is neither necessary nor free.
fn api() -> Result<&'static sys::Api, NdiError> {
    static API: OnceLock<Result<sys::Api, String>> = OnceLock::new();

    let loaded = API.get_or_init(|| {
        let api = sys::Api::load()?;
        // Tells the library to set itself up and reports whether this CPU is
        // one it can use.
        if !unsafe { (api.initialize)() } {
            return Err("this machine does not meet NDI's CPU requirements".into());
        }
        Ok(api)
    });

    loaded.as_ref().map_err(|e| NdiError::Unavailable(e.clone()))
}

/// True when NDI can be used.
pub fn available() -> bool {
    api().is_ok()
}

/// The runtime's version string, for the interface.
pub fn version() -> Option<String> {
    let api = api().ok()?;
    let raw = unsafe { (api.version)() };
    if raw.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(raw) }.to_str().ok().map(|s| s.to_string())
}

/// Why NDI is not available, for the interface to show.
pub fn unavailable_reason() -> Option<String> {
    match api() {
        Ok(_) => None,
        Err(e) => Some(e.to_string()),
    }
}

/// A source seen on the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NdiSource {
    /// As NDI presents it: "MACHINE (Source name)".
    pub name: String,
    pub address: String,
}

/// Every NDI source currently visible.
///
/// `wait` is how long to let discovery run. NDI finds sources by announcement,
/// so a list taken immediately is usually empty — a second is enough for a
/// local network.
pub fn find_sources(wait: Duration) -> Result<Vec<NdiSource>, NdiError> {
    let api = api()?;

    let create = sys::FindCreate {
        // Local sources included: a machine running Rhevia and a graphics
        // application together is an ordinary setup, and hiding them would
        // make that combination impossible.
        show_local_sources: true,
        p_groups: std::ptr::null(),
        p_extra_ips: std::ptr::null(),
    };

    let finder = unsafe { (api.find_create_v2)(&create) };
    if finder.is_null() {
        return Err(NdiError::Start("could not start NDI discovery".into()));
    }

    // Blocks until the list changes or the timeout passes, rather than
    // sleeping and hoping.
    unsafe { (api.find_wait_for_sources)(finder, wait.as_millis() as u32) };

    let mut count: u32 = 0;
    let raw = unsafe { (api.find_get_current_sources)(finder, &mut count) };

    let mut sources = Vec::new();
    if !raw.is_null() {
        for index in 0..count as usize {
            let source = unsafe { &*raw.add(index) };
            let Some(name) = cstr(source.p_ndi_name) else { continue };
            sources.push(NdiSource { name, address: cstr(source.p_url_address).unwrap_or_default() });
        }
    }

    // The list belongs to the finder, so it is copied out before this.
    unsafe { (api.find_destroy)(finder) };
    Ok(sources)
}

fn cstr(raw: *const std::ffi::c_char) -> Option<String> {
    if raw.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(raw) }.to_str().ok().map(|s| s.to_string())
}

/// A receiver pulling one source.
///
/// The same shape as every other input: a thread produces, the renderer takes
/// whatever is newest and never waits.
pub struct NdiReceiver {
    latest: Arc<Mutex<Option<Frame>>>,
    audio: Arc<Mutex<Vec<f32>>>,
    running: Arc<AtomicBool>,
    pub source: NdiSource,
}

impl NdiReceiver {
    /// Connects to `source` and starts receiving.
    pub fn connect(source: NdiSource) -> Result<Self, NdiError> {
        let api = api()?;

        let latest: Arc<Mutex<Option<Frame>>> = Arc::new(Mutex::new(None));
        let audio: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let running = Arc::new(AtomicBool::new(true));

        let slot = Arc::clone(&latest);
        let sink = Arc::clone(&audio);
        let alive = Arc::clone(&running);
        let wanted = source.clone();
        let (opened, opened_rx) = mpsc::channel::<Result<(), String>>();

        std::thread::Builder::new()
            .name("rhevia-ndi-receive".into())
            .spawn(move || {
                receive(api, wanted, slot, sink, alive, &opened);
            })
            .map_err(|e| NdiError::Start(e.to_string()))?;

        match opened_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self { latest, audio, running, source }),
            Ok(Err(e)) => Err(NdiError::Start(e)),
            Err(_) => Err(NdiError::Start("the receiver did not start".into())),
        }
    }

    /// The newest picture, if one has arrived since the last call.
    pub fn take_frame(&self) -> Option<Frame> {
        self.latest.lock().ok().and_then(|mut slot| slot.take())
    }

    /// Takes up to `frames` of interleaved stereo, padded with silence.
    pub fn take_audio(&self, frames: usize) -> Vec<f32> {
        let wanted = frames * CHANNELS;
        let mut out = Vec::with_capacity(wanted);

        if let Ok(mut buffer) = self.audio.lock() {
            let take = wanted.min(buffer.len());
            out.extend(buffer.drain(..take));

            // A sender running ahead is trimmed: held audio is latency that
            // never comes back.
            const MAX: usize = SAMPLE_RATE as usize * CHANNELS / 2;
            if buffer.len() > MAX {
                let excess = buffer.len() - MAX;
                buffer.drain(..excess);
            }
        }
        out.resize(wanted, 0.0);
        out
    }
}

impl Drop for NdiReceiver {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

/// The receive loop.
fn receive(
    api: &'static sys::Api,
    source: NdiSource,
    latest: Arc<Mutex<Option<Frame>>>,
    audio: Arc<Mutex<Vec<f32>>>,
    alive: Arc<AtomicBool>,
    opened: &mpsc::Sender<Result<(), String>>,
) {
    let Ok(name) = CString::new(source.name.clone()) else {
        let _ = opened.send(Err("the source name is not valid".into()));
        return;
    };
    let Ok(receiver_name) = CString::new("Rhevia") else { return };

    let create = sys::RecvCreate {
        source_to_connect_to: sys::Source {
            p_ndi_name: name.as_ptr(),
            p_url_address: std::ptr::null(),
        },
        // RGBA, so the compositor gets what it works in without a conversion
        // pass of our own.
        color_format: sys::COLOR_FORMAT_RGBX_RGBA,
        bandwidth: sys::BANDWIDTH_HIGHEST,
        // Fields off: everything downstream is progressive, and asking the
        // library to deinterlace is better than doing it here.
        allow_video_fields: false,
        p_ndi_recv_name: receiver_name.as_ptr(),
    };

    let receiver = unsafe { (api.recv_create_v3)(&create) };
    if receiver.is_null() {
        let _ = opened.send(Err(format!("could not connect to {}", source.name)));
        return;
    }
    let _ = opened.send(Ok(()));

    while alive.load(Ordering::Relaxed) {
        let mut video = sys::VideoFrame::default();
        let mut sound = sys::AudioFrame::default();

        // A bounded wait, so stopping is noticed promptly when the source has
        // gone quiet.
        let kind = unsafe {
            (api.recv_capture_v2)(receiver, &mut video, &mut sound, std::ptr::null_mut(), 100)
        };

        match kind {
            sys::FRAME_TYPE_VIDEO => {
                if let Some(frame) = to_frame(&video) {
                    if let Ok(mut slot) = latest.lock() {
                        *slot = Some(frame);
                    }
                }
                // Freed whatever happened to the conversion: the library owns
                // the buffer and leaking it would exhaust memory in minutes.
                unsafe { (api.recv_free_video_v2)(receiver, &video) };
            }
            sys::FRAME_TYPE_AUDIO => {
                let samples = to_interleaved(&sound);
                if !samples.is_empty() {
                    if let Ok(mut buffer) = audio.lock() {
                        buffer.extend_from_slice(&samples);
                    }
                }
                unsafe { (api.recv_free_audio_v2)(receiver, &sound) };
            }
            sys::FRAME_TYPE_ERROR => break,
            // None or metadata. Nothing to do.
            _ => {}
        }
    }

    unsafe { (api.recv_destroy)(receiver) };
}

/// Converts a received frame to the engine's RGBA.
///
/// Public so the conversion can be tested without a network: it is the part
/// that silently produces a blue-tinted or skewed picture when it is wrong.
pub fn to_frame(video: &sys::VideoFrame) -> Option<Frame> {
    let (width, height) = (video.xres as usize, video.yres as usize);
    if width == 0 || height == 0 || video.p_data.is_null() {
        return None;
    }

    let stride = video.line_stride_in_bytes as usize;
    if stride < width * 4 {
        return None;
    }

    let mut data = vec![0u8; width * height * 4];
    let source = unsafe { std::slice::from_raw_parts(video.p_data, stride * height) };

    for row in 0..height {
        let from = &source[row * stride..row * stride + width * 4];
        let to = &mut data[row * width * 4..(row + 1) * width * 4];
        to.copy_from_slice(from);
    }

    match video.four_cc {
        sys::FOURCC_RGBA => {}
        // The X variants carry no alpha, and what is in that byte is
        // undefined. Left alone, a frame can arrive fully transparent.
        sys::FOURCC_RGBX => {
            for pixel in data.chunks_exact_mut(4) {
                pixel[3] = 255;
            }
        }
        sys::FOURCC_BGRA | sys::FOURCC_BGRX => {
            let opaque = video.four_cc == sys::FOURCC_BGRX;
            for pixel in data.chunks_exact_mut(4) {
                pixel.swap(0, 2);
                if opaque {
                    pixel[3] = 255;
                }
            }
        }
        // A format that was not asked for. Better no picture than a picture
        // of misread bytes.
        _ => return None,
    }

    Some(Frame { width, height, data })
}

/// Converts NDI's planar audio to interleaved stereo.
///
/// NDI sends each channel as its own block, which is the opposite of what the
/// mixer wants. Reading it as interleaved gives a stuttering, half-speed noise
/// rather than obvious silence, so this is worth its own tests.
pub fn to_interleaved(audio: &sys::AudioFrame) -> Vec<f32> {
    let channels = audio.no_channels as usize;
    let samples = audio.no_samples as usize;
    if channels == 0 || samples == 0 || audio.p_data.is_null() {
        return Vec::new();
    }

    let stride_floats = audio.channel_stride_in_bytes as usize / 4;
    // A sender may pack the planes with no gap, in which case the stride is
    // the sample count.
    let stride = if stride_floats == 0 { samples } else { stride_floats };

    let total = stride * channels;
    let source = unsafe { std::slice::from_raw_parts(audio.p_data, total) };

    let mut out = Vec::with_capacity(samples * CHANNELS);
    for index in 0..samples {
        let left = source[index];
        // Mono is doubled rather than left silent on one side, which is what
        // a single-channel source should sound like.
        let right = if channels > 1 { source[stride + index] } else { left };
        out.push(left);
        out.push(right);
    }
    out
}

/// Publishes the programme as an NDI source.
pub struct NdiSender {
    api: &'static sys::Api,
    instance: sys::Instance,
    pub name: String,
    /// Kept alive: the library holds the pointer given at creation.
    _name: CString,
}

// The instance is used from one thread at a time, behind the engine's own
// ownership.
unsafe impl Send for NdiSender {}

impl NdiSender {
    /// Starts announcing a source called `name`.
    pub fn create(name: &str) -> Result<Self, NdiError> {
        let api = api()?;
        let c_name = CString::new(name).map_err(|_| NdiError::Start("invalid name".into()))?;

        let create = sys::SendCreate {
            p_ndi_name: c_name.as_ptr(),
            p_groups: std::ptr::null(),
            // Clocked to video: the library paces sending to the frame rate
            // of what it is given, which is what a receiver expects. Audio is
            // clocked by the video it arrives with.
            clock_video: true,
            clock_audio: false,
        };

        let instance = unsafe { (api.send_create)(&create) };
        if instance.is_null() {
            return Err(NdiError::Start(format!("could not publish {name}")));
        }

        Ok(Self { api, instance, name: name.to_string(), _name: c_name })
    }

    /// Sends one frame.
    pub fn send_frame(&self, frame: &Frame, fps: f32) {
        if frame.is_empty() {
            return;
        }
        let video = sys::VideoFrame {
            xres: frame.width as i32,
            yres: frame.height as i32,
            four_cc: sys::FOURCC_RGBA,
            frame_rate_n: (fps * 1000.0) as i32,
            frame_rate_d: 1000,
            picture_aspect_ratio: 0.0,
            frame_format_type: 1,
            timecode: i64::MAX,
            p_data: frame.data.as_ptr() as *mut u8,
            line_stride_in_bytes: (frame.width * 4) as i32,
            p_metadata: std::ptr::null(),
            timestamp: 0,
        };
        // Synchronous rather than async: the async form returns before the
        // buffer has been read, and this buffer belongs to the caller.
        unsafe { (self.api.send_send_video_v2)(self.instance, &video) };
    }

    /// Sends a block of interleaved stereo.
    pub fn send_audio(&self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        let frames = samples.len() / CHANNELS;
        if frames == 0 {
            return;
        }

        // NDI wants planes, so the interleaved block is split.
        let mut planar = Vec::with_capacity(frames * CHANNELS);
        for channel in 0..CHANNELS {
            for index in 0..frames {
                planar.push(samples[index * CHANNELS + channel]);
            }
        }

        let audio = sys::AudioFrame {
            sample_rate: SAMPLE_RATE as i32,
            no_channels: CHANNELS as i32,
            no_samples: frames as i32,
            timecode: i64::MAX,
            p_data: planar.as_ptr() as *mut f32,
            channel_stride_in_bytes: (frames * 4) as i32,
            p_metadata: std::ptr::null(),
            timestamp: 0,
        };
        unsafe { (self.api.send_send_audio_v2)(self.instance, &audio) };
    }
}

impl Drop for NdiSender {
    fn drop(&mut self) {
        unsafe { (self.api.send_destroy)(self.instance) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a frame descriptor over a buffer the test owns.
    fn descriptor(
        data: &mut [u8],
        width: i32,
        height: i32,
        four_cc: u32,
        stride: i32,
    ) -> sys::VideoFrame {
        sys::VideoFrame {
            xres: width,
            yres: height,
            four_cc,
            line_stride_in_bytes: stride,
            p_data: data.as_mut_ptr(),
            ..Default::default()
        }
    }

    #[test]
    fn rgba_arrives_unchanged() {
        let mut raw = vec![10u8, 20, 30, 40, 50, 60, 70, 80];
        let video = descriptor(&mut raw, 2, 1, sys::FOURCC_RGBA, 8);
        let frame = to_frame(&video).expect("should convert");

        assert_eq!(frame.width, 2);
        assert_eq!(frame.height, 1);
        assert_eq!(frame.data, vec![10, 20, 30, 40, 50, 60, 70, 80]);
    }

    #[test]
    fn bgra_has_its_channels_swapped() {
        // Read the wrong way round this gives a blue-tinted picture, which is
        // subtle enough to ship by accident.
        let mut raw = vec![255u8, 0, 0, 255]; // blue in BGRA
        let video = descriptor(&mut raw, 1, 1, sys::FOURCC_BGRA, 4);
        let frame = to_frame(&video).expect("should convert");

        assert_eq!(frame.data, vec![0, 0, 255, 255], "expected blue in RGBA");
    }

    #[test]
    fn the_x_formats_are_forced_opaque() {
        // The fourth byte is undefined in RGBX. Trusting it can make a whole
        // frame transparent.
        let mut raw = vec![10u8, 20, 30, 0];
        let video = descriptor(&mut raw, 1, 1, sys::FOURCC_RGBX, 4);
        let frame = to_frame(&video).expect("should convert");
        assert_eq!(frame.data[3], 255);

        let mut raw = vec![10u8, 20, 30, 0];
        let video = descriptor(&mut raw, 1, 1, sys::FOURCC_BGRX, 4);
        let frame = to_frame(&video).expect("should convert");
        assert_eq!(frame.data, vec![30, 20, 10, 255]);
    }

    #[test]
    fn padding_at_the_end_of_each_row_is_dropped() {
        // A sender may pad rows. Copying the stride rather than the width
        // skews the picture diagonally.
        let mut raw = vec![0u8; 3 * 8]; // 2 pixels used, 8 bytes stride, 3 rows
        for row in 0..3 {
            raw[row * 8] = row as u8 + 1; // first pixel of each row
        }
        let video = descriptor(&mut raw, 2, 3, sys::FOURCC_RGBA, 8);
        let frame = to_frame(&video).expect("should convert");

        assert_eq!(frame.data.len(), 2 * 3 * 4);
        assert_eq!(frame.data[0], 1);
        assert_eq!(frame.data[8], 2, "the second row should start where it should");
        assert_eq!(frame.data[16], 3);
    }

    #[test]
    fn a_frame_in_an_unexpected_format_is_refused() {
        let mut raw = vec![0u8; 16];
        let video = descriptor(&mut raw, 2, 2, 0x1234_5678, 8);
        assert!(to_frame(&video).is_none(), "a misread frame is worse than none");
    }

    #[test]
    fn an_impossible_frame_is_refused_rather_than_read_past() {
        let mut raw = vec![0u8; 4];
        // A stride narrower than the row cannot be right.
        let video = descriptor(&mut raw, 4, 1, sys::FOURCC_RGBA, 4);
        assert!(to_frame(&video).is_none());

        let mut raw = vec![0u8; 4];
        let video = descriptor(&mut raw, 0, 0, sys::FOURCC_RGBA, 0);
        assert!(to_frame(&video).is_none());
    }

    #[test]
    fn planar_audio_becomes_interleaved() {
        // Left plane then right plane, which is how NDI sends it. Read as
        // interleaved this becomes half-speed noise rather than silence.
        let mut raw: Vec<f32> = vec![0.1, 0.2, 0.3, /* right */ 0.7, 0.8, 0.9];
        let audio = sys::AudioFrame {
            no_channels: 2,
            no_samples: 3,
            channel_stride_in_bytes: 3 * 4,
            p_data: raw.as_mut_ptr(),
            ..Default::default()
        };

        let out = to_interleaved(&audio);
        assert_eq!(out.len(), 6);
        assert!((out[0] - 0.1).abs() < 1e-6 && (out[1] - 0.7).abs() < 1e-6);
        assert!((out[2] - 0.2).abs() < 1e-6 && (out[3] - 0.8).abs() < 1e-6);
        assert!((out[4] - 0.3).abs() < 1e-6 && (out[5] - 0.9).abs() < 1e-6);
    }

    #[test]
    fn a_mono_source_is_heard_on_both_sides() {
        let mut raw: Vec<f32> = vec![0.5, 0.6];
        let audio = sys::AudioFrame {
            no_channels: 1,
            no_samples: 2,
            channel_stride_in_bytes: 2 * 4,
            p_data: raw.as_mut_ptr(),
            ..Default::default()
        };

        let out = to_interleaved(&audio);
        assert_eq!(out, vec![0.5, 0.5, 0.6, 0.6]);
    }

    #[test]
    fn planes_packed_with_no_gap_are_still_read_correctly() {
        // A stride of zero means the planes are back to back.
        let mut raw: Vec<f32> = vec![0.1, 0.2, 0.7, 0.8];
        let audio = sys::AudioFrame {
            no_channels: 2,
            no_samples: 2,
            channel_stride_in_bytes: 0,
            p_data: raw.as_mut_ptr(),
            ..Default::default()
        };

        assert_eq!(to_interleaved(&audio), vec![0.1, 0.7, 0.2, 0.8]);
    }

    #[test]
    fn an_empty_audio_frame_produces_nothing() {
        let audio = sys::AudioFrame::default();
        assert!(to_interleaved(&audio).is_empty());
    }

    #[test]
    fn the_runtime_reports_itself_or_says_why_not() {
        // Both outcomes are legitimate; what matters is that asking does not
        // panic and that the two answers agree with each other.
        match unavailable_reason() {
            None => {
                assert!(available());
                let version = version().expect("an available runtime has a version");
                assert!(!version.trim().is_empty());
                eprintln!("NDI runtime: {version}");
            }
            Some(reason) => {
                assert!(!available());
                assert!(!reason.trim().is_empty());
                eprintln!("SKIP: {reason}");
            }
        }
    }

    #[test]
    fn discovery_runs_without_panicking() {
        if !available() {
            eprintln!("SKIP: no NDI runtime");
            return;
        }
        match find_sources(Duration::from_millis(600)) {
            Ok(sources) => {
                for source in &sources {
                    assert!(!source.name.trim().is_empty());
                }
                eprintln!("found {} NDI sources", sources.len());
            }
            Err(e) => panic!("discovery failed: {e}"),
        }
    }

    #[test]
    fn connecting_to_a_source_that_is_not_there_fails_rather_than_hanging() {
        if !available() {
            eprintln!("SKIP: no NDI runtime");
            return;
        }
        // NDI will happily create a receiver for a name nobody is announcing;
        // what must not happen is a hang.
        let source = NdiSource {
            name: "NO-SUCH-MACHINE (nothing)".into(),
            address: String::new(),
        };
        let started = std::time::Instant::now();
        let _ = NdiReceiver::connect(source);
        assert!(started.elapsed() < Duration::from_secs(6), "connecting hung");
    }
}
