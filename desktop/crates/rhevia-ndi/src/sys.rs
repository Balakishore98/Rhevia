//! The NDI C ABI, declared here and bound at runtime.
//!
//! Nothing from the NDI SDK is vendored into this tree. The library is the one
//! the user already installed — the same arrangement OBS uses — and it is
//! found through the environment variables the NDI installer sets, loaded with
//! `LoadLibrary`, and bound symbol by symbol by name.
//!
//! Binding by name rather than through the `NDIlib_v5` function table is
//! deliberate. The table is a hundred-odd pointers whose order is part of the
//! SDK; getting one wrong calls a different function with the wrong arguments,
//! which is a crash at best. A name either resolves or it does not.
//!
//! The struct layouts below are the part that has to be exactly right, so
//! there are tests asserting their sizes and field offsets. A silently wrong
//! layout produces garbage pixels rather than an error.

use std::ffi::{c_char, c_int, c_void};

/// Opaque handles. Their contents belong to the library.
pub type Instance = *mut c_void;

/// What `recv_capture_v2` returned.
pub const FRAME_TYPE_NONE: c_int = 0;
pub const FRAME_TYPE_VIDEO: c_int = 1;
pub const FRAME_TYPE_AUDIO: c_int = 2;
pub const FRAME_TYPE_ERROR: c_int = 4;

/// Ask for RGBA rather than the native UYVY: the library converts with
/// optimised code, and the compositor works in RGBA. Converting here would be
/// the same work done worse.
pub const COLOR_FORMAT_RGBX_RGBA: c_int = 2;

/// Full quality. The low-bandwidth stream is a proxy, which is not what a
/// programme feed wants.
pub const BANDWIDTH_HIGHEST: c_int = 100;

/// FourCC codes, little-endian packed as the library reports them.
pub const FOURCC_RGBA: u32 = fourcc(b"RGBA");
pub const FOURCC_RGBX: u32 = fourcc(b"RGBX");
pub const FOURCC_BGRA: u32 = fourcc(b"BGRA");
pub const FOURCC_BGRX: u32 = fourcc(b"BGRX");

const fn fourcc(code: &[u8; 4]) -> u32 {
    (code[0] as u32) | ((code[1] as u32) << 8) | ((code[2] as u32) << 16) | ((code[3] as u32) << 24)
}

/// A source on the network.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Source {
    pub p_ndi_name: *const c_char,
    /// A union of url and ip in the header; both are a pointer, so one field
    /// describes it.
    pub p_url_address: *const c_char,
}

impl Default for Source {
    fn default() -> Self {
        Self { p_ndi_name: std::ptr::null(), p_url_address: std::ptr::null() }
    }
}

#[repr(C)]
pub struct FindCreate {
    pub show_local_sources: bool,
    pub p_groups: *const c_char,
    pub p_extra_ips: *const c_char,
}

#[repr(C)]
pub struct RecvCreate {
    pub source_to_connect_to: Source,
    pub color_format: c_int,
    pub bandwidth: c_int,
    pub allow_video_fields: bool,
    pub p_ndi_recv_name: *const c_char,
}

#[repr(C)]
pub struct SendCreate {
    pub p_ndi_name: *const c_char,
    pub p_groups: *const c_char,
    pub clock_video: bool,
    pub clock_audio: bool,
}

#[repr(C)]
pub struct VideoFrame {
    pub xres: c_int,
    pub yres: c_int,
    pub four_cc: u32,
    pub frame_rate_n: c_int,
    pub frame_rate_d: c_int,
    pub picture_aspect_ratio: f32,
    pub frame_format_type: c_int,
    pub timecode: i64,
    pub p_data: *mut u8,
    /// A union of stride and size; both are an int.
    pub line_stride_in_bytes: c_int,
    pub p_metadata: *const c_char,
    pub timestamp: i64,
}

impl Default for VideoFrame {
    fn default() -> Self {
        // Zeroed rather than field by field: the library fills this in, and
        // any field left uninitialised is one it may read.
        unsafe { std::mem::zeroed() }
    }
}

#[repr(C)]
pub struct AudioFrame {
    pub sample_rate: c_int,
    pub no_channels: c_int,
    pub no_samples: c_int,
    pub timecode: i64,
    pub p_data: *mut f32,
    /// A union of channel stride and size; both are an int.
    pub channel_stride_in_bytes: c_int,
    pub p_metadata: *const c_char,
    pub timestamp: i64,
}

impl Default for AudioFrame {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

/// The functions this needs, bound by name.
pub struct Api {
    /// Kept alive: unloading the library invalidates every pointer below.
    _library: libloading::Library,

    pub initialize: unsafe extern "C" fn() -> bool,
    pub destroy: unsafe extern "C" fn(),
    pub version: unsafe extern "C" fn() -> *const c_char,

    pub find_create_v2: unsafe extern "C" fn(*const FindCreate) -> Instance,
    pub find_destroy: unsafe extern "C" fn(Instance),
    pub find_wait_for_sources: unsafe extern "C" fn(Instance, u32) -> bool,
    pub find_get_current_sources: unsafe extern "C" fn(Instance, *mut u32) -> *const Source,

    pub recv_create_v3: unsafe extern "C" fn(*const RecvCreate) -> Instance,
    pub recv_destroy: unsafe extern "C" fn(Instance),
    pub recv_capture_v2:
        unsafe extern "C" fn(Instance, *mut VideoFrame, *mut AudioFrame, *mut c_void, u32) -> c_int,
    pub recv_free_video_v2: unsafe extern "C" fn(Instance, *const VideoFrame),
    pub recv_free_audio_v2: unsafe extern "C" fn(Instance, *const AudioFrame),

    pub send_create: unsafe extern "C" fn(*const SendCreate) -> Instance,
    pub send_destroy: unsafe extern "C" fn(Instance),
    pub send_send_video_v2: unsafe extern "C" fn(Instance, *const VideoFrame),
    pub send_send_audio_v2: unsafe extern "C" fn(Instance, *const AudioFrame),
}

// The library is thread safe by its own documentation, and the pointers are
// immutable once bound.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

/// Where the NDI installer says its runtime lives.
///
/// The variables are set by the installer for exactly this purpose, newest
/// first. Falling back to a bare library name lets the system search path
/// find it when a user has put it somewhere else.
fn candidates() -> Vec<std::path::PathBuf> {
    const NAME: &str = if cfg!(target_pointer_width = "64") {
        "Processing.NDI.Lib.x64.dll"
    } else {
        "Processing.NDI.Lib.x86.dll"
    };

    let mut paths = Vec::new();
    for version in ["V6", "V5", "V4", "V3"] {
        if let Ok(dir) = std::env::var(format!("NDI_RUNTIME_DIR_{version}")) {
            paths.push(std::path::PathBuf::from(dir).join(NAME));
        }
    }
    paths.push(std::path::PathBuf::from(NAME));
    paths
}

impl Api {
    /// Loads the installed runtime, or explains why it could not.
    pub fn load() -> Result<Self, String> {
        let mut tried = Vec::new();
        for path in candidates() {
            match unsafe { libloading::Library::new(&path) } {
                Ok(library) => return Self::bind(library),
                Err(e) => tried.push(format!("{}: {e}", path.display())),
            }
        }
        Err(format!("the NDI runtime is not installed. Tried:\n  {}", tried.join("\n  ")))
    }

    fn bind(library: libloading::Library) -> Result<Self, String> {
        // A macro only to keep the error message attached to the name that
        // failed; each binding is otherwise identical.
        macro_rules! sym {
            ($name:literal) => {
                unsafe {
                    *library
                        .get($name)
                        .map_err(|e| format!("{} is missing from the NDI runtime: {e}",
                            String::from_utf8_lossy(&$name[..$name.len() - 1])))?
                }
            };
        }

        Ok(Self {
            initialize: sym!(b"NDIlib_initialize\0"),
            destroy: sym!(b"NDIlib_destroy\0"),
            version: sym!(b"NDIlib_version\0"),

            find_create_v2: sym!(b"NDIlib_find_create_v2\0"),
            find_destroy: sym!(b"NDIlib_find_destroy\0"),
            find_wait_for_sources: sym!(b"NDIlib_find_wait_for_sources\0"),
            find_get_current_sources: sym!(b"NDIlib_find_get_current_sources\0"),

            recv_create_v3: sym!(b"NDIlib_recv_create_v3\0"),
            recv_destroy: sym!(b"NDIlib_recv_destroy\0"),
            recv_capture_v2: sym!(b"NDIlib_recv_capture_v2\0"),
            recv_free_video_v2: sym!(b"NDIlib_recv_free_video_v2\0"),
            recv_free_audio_v2: sym!(b"NDIlib_recv_free_audio_v2\0"),

            send_create: sym!(b"NDIlib_send_create\0"),
            send_destroy: sym!(b"NDIlib_send_destroy\0"),
            send_send_video_v2: sym!(b"NDIlib_send_send_video_v2\0"),
            send_send_audio_v2: sym!(b"NDIlib_send_send_audio_v2\0"),

            _library: library,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    /// The layouts are the part that cannot be checked by the compiler against
    /// the real header. A wrong offset does not fail to build — it reads the
    /// wrong bytes and produces garbage pixels or a crash, so the sizes and
    /// offsets are pinned here against the published ABI.
    #[test]
    fn a_source_is_two_pointers() {
        assert_eq!(size_of::<Source>(), 16);
        assert_eq!(offset_of!(Source, p_ndi_name), 0);
        assert_eq!(offset_of!(Source, p_url_address), 8);
    }

    #[test]
    fn the_find_descriptor_pads_after_its_flag() {
        assert_eq!(size_of::<FindCreate>(), 24);
        assert_eq!(offset_of!(FindCreate, show_local_sources), 0);
        assert_eq!(offset_of!(FindCreate, p_groups), 8);
        assert_eq!(offset_of!(FindCreate, p_extra_ips), 16);
    }

    #[test]
    fn the_receiver_descriptor_matches_the_header() {
        assert_eq!(size_of::<RecvCreate>(), 40);
        assert_eq!(offset_of!(RecvCreate, source_to_connect_to), 0);
        assert_eq!(offset_of!(RecvCreate, color_format), 16);
        assert_eq!(offset_of!(RecvCreate, bandwidth), 20);
        assert_eq!(offset_of!(RecvCreate, allow_video_fields), 24);
        assert_eq!(offset_of!(RecvCreate, p_ndi_recv_name), 32);
    }

    #[test]
    fn the_sender_descriptor_matches_the_header() {
        assert_eq!(size_of::<SendCreate>(), 24);
        assert_eq!(offset_of!(SendCreate, p_ndi_name), 0);
        assert_eq!(offset_of!(SendCreate, p_groups), 8);
        assert_eq!(offset_of!(SendCreate, clock_video), 16);
        assert_eq!(offset_of!(SendCreate, clock_audio), 17);
    }

    #[test]
    fn a_video_frame_matches_the_header() {
        assert_eq!(size_of::<VideoFrame>(), 72);
        assert_eq!(offset_of!(VideoFrame, xres), 0);
        assert_eq!(offset_of!(VideoFrame, yres), 4);
        assert_eq!(offset_of!(VideoFrame, four_cc), 8);
        assert_eq!(offset_of!(VideoFrame, frame_rate_n), 12);
        assert_eq!(offset_of!(VideoFrame, frame_rate_d), 16);
        assert_eq!(offset_of!(VideoFrame, picture_aspect_ratio), 20);
        assert_eq!(offset_of!(VideoFrame, frame_format_type), 24);
        // The timecode forces eight-byte alignment, so there is padding here.
        assert_eq!(offset_of!(VideoFrame, timecode), 32);
        assert_eq!(offset_of!(VideoFrame, p_data), 40);
        assert_eq!(offset_of!(VideoFrame, line_stride_in_bytes), 48);
        assert_eq!(offset_of!(VideoFrame, p_metadata), 56);
        assert_eq!(offset_of!(VideoFrame, timestamp), 64);
    }

    #[test]
    fn an_audio_frame_matches_the_header() {
        assert_eq!(size_of::<AudioFrame>(), 56);
        assert_eq!(offset_of!(AudioFrame, sample_rate), 0);
        assert_eq!(offset_of!(AudioFrame, no_channels), 4);
        assert_eq!(offset_of!(AudioFrame, no_samples), 8);
        assert_eq!(offset_of!(AudioFrame, timecode), 16);
        assert_eq!(offset_of!(AudioFrame, p_data), 24);
        assert_eq!(offset_of!(AudioFrame, channel_stride_in_bytes), 32);
        assert_eq!(offset_of!(AudioFrame, p_metadata), 40);
        assert_eq!(offset_of!(AudioFrame, timestamp), 48);
    }

    #[test]
    fn fourcc_codes_are_packed_little_endian() {
        // The first character is the low byte, so 'RGBA' reads back as
        // 0x41424752 and 'BGRA' as 0x41524742 — the middle two bytes swap,
        // which is exactly the difference the converter has to act on.
        assert_eq!(FOURCC_RGBA, 0x4142_4752);
        assert_eq!(FOURCC_BGRA, 0x4152_4742);
        assert_ne!(FOURCC_RGBA, FOURCC_BGRA);
        assert_ne!(FOURCC_RGBX, FOURCC_RGBA);
    }

    #[test]
    fn the_runtime_is_looked_for_where_the_installer_puts_it() {
        let paths = candidates();
        assert!(!paths.is_empty());
        // The last resort is a bare name, so the system search path is used
        // when the variables are absent.
        let last = paths.last().unwrap();
        assert_eq!(last.parent(), Some(std::path::Path::new("")));
    }
}
