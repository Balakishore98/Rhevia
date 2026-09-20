//! The VST3 binary interface, declared here rather than vendored.
//!
//! **Licensing.** Nothing from Steinberg's VST 3 SDK is in this tree. These
//! are declarations of a published binary interface, written to interoperate
//! with plugins the user has installed. Shipping VST3 support in a product
//! you sell is a separate question and needs Steinberg's developer agreement,
//! which is free but has to be signed by the person selling the software. See
//! the note in [`super`].
//!
//! VST3 is COM-shaped: every interface begins with the three `FUnknown`
//! methods and is reached through a vtable pointer. On x86-64 Windows there is
//! only one calling convention, so `extern "C"` is correct despite the
//! `__stdcall` in the headers; on 32-bit it would not be, which is why this
//! refuses to build there.

#![cfg(target_pointer_width = "64")]

use std::ffi::{c_char, c_void};

/// A VST3 result code. Zero is success, which is the opposite of most of this
/// codebase and easy to get backwards.
pub type TResult = i32;

pub const K_RESULT_OK: TResult = 0;
pub const K_RESULT_TRUE: TResult = 0;
pub const K_NO_INTERFACE: TResult = 1;

/// An interface identifier: sixteen bytes, byte order as the SDK writes them.
pub type Tuid = [u8; 16];

/// Builds an identifier from the four 32-bit words the SDK declares them in.
///
/// On Windows, VST3 identifiers are laid out the way COM lays out a GUID: the
/// first word little-endian, the second as two little-endian halves in the
/// other order, and the last two big-endian. Everywhere else the bytes are
/// simply big-endian throughout.
///
/// This is not a detail that can be skipped. With the wrong order every
/// `queryInterface` fails and `createInstance` answers `E_NOINTERFACE`, which
/// reads as "this plugin has no audio processor" rather than as a host bug —
/// exactly the wrong conclusion.
pub const fn tuid(a: u32, b: u32, c: u32, d: u32) -> Tuid {
    if cfg!(windows) {
        [
            (a & 0xff) as u8,
            ((a >> 8) & 0xff) as u8,
            ((a >> 16) & 0xff) as u8,
            ((a >> 24) & 0xff) as u8,
            ((b >> 16) & 0xff) as u8,
            ((b >> 24) & 0xff) as u8,
            (b & 0xff) as u8,
            ((b >> 8) & 0xff) as u8,
            ((c >> 24) & 0xff) as u8,
            ((c >> 16) & 0xff) as u8,
            ((c >> 8) & 0xff) as u8,
            (c & 0xff) as u8,
            ((d >> 24) & 0xff) as u8,
            ((d >> 16) & 0xff) as u8,
            ((d >> 8) & 0xff) as u8,
            (d & 0xff) as u8,
        ]
    } else {
        [
            ((a >> 24) & 0xff) as u8,
            ((a >> 16) & 0xff) as u8,
            ((a >> 8) & 0xff) as u8,
            (a & 0xff) as u8,
            ((b >> 24) & 0xff) as u8,
            ((b >> 16) & 0xff) as u8,
            ((b >> 8) & 0xff) as u8,
            (b & 0xff) as u8,
            ((c >> 24) & 0xff) as u8,
            ((c >> 16) & 0xff) as u8,
            ((c >> 8) & 0xff) as u8,
            (c & 0xff) as u8,
            ((d >> 24) & 0xff) as u8,
            ((d >> 16) & 0xff) as u8,
            ((d >> 8) & 0xff) as u8,
            (d & 0xff) as u8,
        ]
    }
}

/// The class category an audio effect or instrument declares itself under.
pub const K_AUDIO_MODULE_CLASS: &str = "Audio Module Class";

/// What the factory reports about itself.
#[repr(C)]
pub struct FactoryInfo {
    pub vendor: [c_char; 64],
    pub url: [c_char; 256],
    pub email: [c_char; 128],
    pub flags: i32,
}

impl Default for FactoryInfo {
    fn default() -> Self {
        // Zeroed: the plugin fills this in, and it may read nothing.
        unsafe { std::mem::zeroed() }
    }
}

/// What the factory reports about one class it can make.
#[repr(C)]
pub struct ClassInfo {
    pub cid: Tuid,
    pub cardinality: i32,
    pub category: [c_char; 32],
    pub name: [c_char; 64],
}

impl Default for ClassInfo {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

/// The richer class record a version 2 factory offers.
#[repr(C)]
pub struct ClassInfo2 {
    pub cid: Tuid,
    pub cardinality: i32,
    pub category: [c_char; 32],
    pub name: [c_char; 64],
    pub class_flags: u32,
    pub sub_categories: [c_char; 128],
    pub vendor: [c_char; 64],
    pub version: [c_char; 64],
    pub sdk_version: [c_char; 64],
}

impl Default for ClassInfo2 {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

/// `IPluginFactory`, the only interface needed to find out what a module
/// contains.
#[repr(C)]
pub struct IPluginFactoryVtbl {
    pub query_interface:
        unsafe extern "C" fn(*mut c_void, *const Tuid, *mut *mut c_void) -> TResult,
    pub add_ref: unsafe extern "C" fn(*mut c_void) -> u32,
    pub release: unsafe extern "C" fn(*mut c_void) -> u32,

    pub get_factory_info: unsafe extern "C" fn(*mut c_void, *mut FactoryInfo) -> TResult,
    pub count_classes: unsafe extern "C" fn(*mut c_void) -> i32,
    pub get_class_info: unsafe extern "C" fn(*mut c_void, i32, *mut ClassInfo) -> TResult,
    pub create_instance:
        unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, *mut *mut c_void) -> TResult,
}

/// `IPluginFactory2`, which adds the richer class record.
#[repr(C)]
pub struct IPluginFactory2Vtbl {
    pub base: IPluginFactoryVtbl,
    pub get_class_info2: unsafe extern "C" fn(*mut c_void, i32, *mut ClassInfo2) -> TResult,
}

/// Any COM-shaped object: a pointer to its vtable.
#[repr(C)]
pub struct Object<V> {
    pub vtbl: *const V,
}

/// `IPluginFactory`: 7A4D8A0E-7C3B-4A9E-BF... — the published identifiers.
pub const IPLUGIN_FACTORY_IID: Tuid = tuid(0x7A4D_811C, 0x5211_4A1F, 0xAED9_D2EE, 0x0B43_BF9F);
pub const IPLUGIN_FACTORY2_IID: Tuid = tuid(0x0007_B650, 0xF24B_4C0B, 0xA464_EDB9, 0xF00B_2ABB);
pub const IPLUGIN_FACTORY3_IID: Tuid = tuid(0x4555_A2AB, 0xC123_4569, 0x98A0_5433, 0x1E2F_A0DE);

/// The module entry points a VST3 binary exports on Windows.
pub const ENTRY_INIT: &[u8] = b"InitDll\0";
pub const ENTRY_EXIT: &[u8] = b"ExitDll\0";
pub const ENTRY_FACTORY: &[u8] = b"GetPluginFactory\0";

/// Reads a fixed-size C string field into a Rust string.
///
/// The fields are padded with nulls and are not guaranteed to be terminated
/// when full, so the length is bounded by the array rather than by a null.
pub fn field(bytes: &[c_char]) -> String {
    let raw: Vec<u8> = bytes
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&raw).trim().to_string()
}


/* ------------------------------------------------------------------ */
/*  Processing                                                         */
/* ------------------------------------------------------------------ */

/// Media types a bus can carry.
pub const K_AUDIO: i32 = 0;
pub const K_EVENT: i32 = 1;

/// Bus directions.
pub const K_INPUT: i32 = 0;
pub const K_OUTPUT: i32 = 1;

/// Sample sizes a processor can be asked for. Everything here is 32-bit
/// float, which is what the mixer works in.
pub const K_SAMPLE32: i32 = 0;

/// Processing modes. Realtime is the only one a live show uses.
pub const K_REALTIME: i32 = 0;

/// Stereo, as a speaker arrangement bitmask: left and right.
pub const K_STEREO: u64 = 0x3;

/// What a plugin says about one of its buses.
#[repr(C)]
pub struct BusInfo {
    pub media_type: i32,
    pub direction: i32,
    pub channel_count: i32,
    /// UTF-16, fixed length, null padded.
    pub name: [u16; 128],
    pub bus_type: i32,
    pub flags: u32,
}

impl Default for BusInfo {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

/// How the plugin should expect to be driven.
#[repr(C)]
pub struct ProcessSetup {
    pub process_mode: i32,
    pub symbolic_sample_size: i32,
    pub max_samples_per_block: i32,
    pub sample_rate: f64,
}

/// One bus worth of audio, as an array of per-channel pointers.
///
/// VST3 is planar: each channel is its own buffer. The mixer is interleaved,
/// so something has to convert, and doing it here keeps the plugin boundary
/// the only place that knows.
#[repr(C)]
pub struct AudioBusBuffers {
    pub num_channels: i32,
    pub silence_flags: u64,
    /// `Sample32**` — an array of pointers, one per channel.
    pub channel_buffers: *mut *mut f32,
}

impl Default for AudioBusBuffers {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

/// Everything one `process` call is given.
#[repr(C)]
pub struct ProcessData {
    pub process_mode: i32,
    pub symbolic_sample_size: i32,
    pub num_samples: i32,
    pub num_inputs: i32,
    pub num_outputs: i32,
    pub inputs: *mut AudioBusBuffers,
    pub outputs: *mut AudioBusBuffers,
    pub input_parameter_changes: *mut c_void,
    pub output_parameter_changes: *mut c_void,
    pub input_events: *mut c_void,
    pub output_events: *mut c_void,
    pub process_context: *mut c_void,
}

impl Default for ProcessData {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}


/// State flags for [`ProcessContext`], saying which of its fields mean
/// anything.
pub const K_PLAYING: u32 = 1 << 1;
pub const K_SYSTEM_TIME_VALID: u32 = 1 << 8;
pub const K_PROJECT_TIME_MUSIC_VALID: u32 = 1 << 9;
pub const K_TEMPO_VALID: u32 = 1 << 10;
pub const K_TIME_SIG_VALID: u32 = 1 << 13;
pub const K_CONT_TIME_VALID: u32 = 1 << 17;

/// Where the transport is, in every unit a plugin might ask for.
///
/// A live show has no timeline, but this is not optional: plugins read it
/// without checking, and handing over a null context crashes them. Real
/// values are supplied so that anything tempo-synced behaves predictably
/// rather than at whatever the uninitialised memory said.
#[repr(C)]
pub struct ProcessContext {
    pub state: u32,
    pub sample_rate: f64,
    pub project_time_samples: i64,
    pub system_time: i64,
    pub continuous_time_samples: i64,
    pub project_time_music: f64,
    pub bar_position_music: f64,
    pub cycle_start_music: f64,
    pub cycle_end_music: f64,
    pub tempo: f64,
    pub time_sig_numerator: i32,
    pub time_sig_denominator: i32,
    /// `{ uint8 keyNote; uint8 rootNote; int16 chordMask; }`
    pub chord: [u8; 4],
    pub smpte_offset_subframes: i32,
    /// `{ uint32 framesPerSecond; uint32 flags; }`
    pub frame_rate: [u32; 2],
    pub samples_to_next_clock: i32,
}

impl Default for ProcessContext {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

/// `IPluginBase`, which `IComponent` extends.
#[repr(C)]
pub struct IPluginBaseVtbl {
    pub query_interface:
        unsafe extern "C" fn(*mut c_void, *const Tuid, *mut *mut c_void) -> TResult,
    pub add_ref: unsafe extern "C" fn(*mut c_void) -> u32,
    pub release: unsafe extern "C" fn(*mut c_void) -> u32,

    pub initialize: unsafe extern "C" fn(*mut c_void, *mut c_void) -> TResult,
    pub terminate: unsafe extern "C" fn(*mut c_void) -> TResult,
}

/// `IComponent`: what the plugin is, and its buses.
#[repr(C)]
pub struct IComponentVtbl {
    pub base: IPluginBaseVtbl,

    pub get_controller_class_id: unsafe extern "C" fn(*mut c_void, *mut Tuid) -> TResult,
    pub set_io_mode: unsafe extern "C" fn(*mut c_void, i32) -> TResult,
    pub get_bus_count: unsafe extern "C" fn(*mut c_void, i32, i32) -> i32,
    pub get_bus_info:
        unsafe extern "C" fn(*mut c_void, i32, i32, i32, *mut BusInfo) -> TResult,
    pub get_routing_info:
        unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> TResult,
    pub activate_bus: unsafe extern "C" fn(*mut c_void, i32, i32, i32, u8) -> TResult,
    pub set_active: unsafe extern "C" fn(*mut c_void, u8) -> TResult,
    pub set_state: unsafe extern "C" fn(*mut c_void, *mut c_void) -> TResult,
    pub get_state: unsafe extern "C" fn(*mut c_void, *mut c_void) -> TResult,
}

/// `IAudioProcessor`: the audio path itself.
#[repr(C)]
pub struct IAudioProcessorVtbl {
    pub query_interface:
        unsafe extern "C" fn(*mut c_void, *const Tuid, *mut *mut c_void) -> TResult,
    pub add_ref: unsafe extern "C" fn(*mut c_void) -> u32,
    pub release: unsafe extern "C" fn(*mut c_void) -> u32,

    pub set_bus_arrangements:
        unsafe extern "C" fn(*mut c_void, *mut u64, i32, *mut u64, i32) -> TResult,
    pub get_bus_arrangement: unsafe extern "C" fn(*mut c_void, i32, i32, *mut u64) -> TResult,
    pub can_process_sample_size: unsafe extern "C" fn(*mut c_void, i32) -> TResult,
    pub get_latency_samples: unsafe extern "C" fn(*mut c_void) -> u32,
    pub setup_processing: unsafe extern "C" fn(*mut c_void, *mut ProcessSetup) -> TResult,
    pub set_processing: unsafe extern "C" fn(*mut c_void, u8) -> TResult,
    pub process: unsafe extern "C" fn(*mut c_void, *mut ProcessData) -> TResult,
    pub get_tail_samples: unsafe extern "C" fn(*mut c_void) -> u32,
}

/// The published identifiers for the interfaces the audio path needs.
pub const IPLUGIN_BASE_IID: Tuid = tuid(0x2288_8DDB, 0x156E_45AE, 0x8358_B348, 0x0819_0625);
pub const ICOMPONENT_IID: Tuid = tuid(0xE831_FF31, 0xF2D5_4301, 0x928E_BBEE, 0x2569_7802);
pub const IAUDIO_PROCESSOR_IID: Tuid = tuid(0x4204_3F99, 0xB7DA_453C, 0xA569_E79D, 0x9AAE_C33D);
pub const IHOST_APPLICATION_IID: Tuid = tuid(0x58E5_95CC, 0xDB2D_4969, 0x8B6A_AF8C, 0x36A6_64E5);
pub const FUNKNOWN_IID: Tuid = tuid(0x0000_0000, 0x0000_0000, 0xC000_0000, 0x0000_0046);

/// Reads a fixed-size UTF-16 field, as bus names are stored.
pub fn field16(units: &[u16]) -> String {
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end]).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    /// The layouts are what cannot be checked against the real header here, so
    /// they are pinned. A wrong size makes the plugin write past the end of
    /// the struct, which corrupts the stack rather than failing cleanly.
    #[test]
    fn the_factory_record_matches_the_published_layout() {
        assert_eq!(size_of::<FactoryInfo>(), 64 + 256 + 128 + 4);
        assert_eq!(offset_of!(FactoryInfo, vendor), 0);
        assert_eq!(offset_of!(FactoryInfo, url), 64);
        assert_eq!(offset_of!(FactoryInfo, email), 320);
        assert_eq!(offset_of!(FactoryInfo, flags), 448);
    }

    #[test]
    fn the_class_record_matches_the_published_layout() {
        assert_eq!(size_of::<ClassInfo>(), 16 + 4 + 32 + 64);
        assert_eq!(offset_of!(ClassInfo, cid), 0);
        assert_eq!(offset_of!(ClassInfo, cardinality), 16);
        assert_eq!(offset_of!(ClassInfo, category), 20);
        assert_eq!(offset_of!(ClassInfo, name), 52);
    }

    #[test]
    fn the_richer_class_record_matches_the_published_layout() {
        assert_eq!(offset_of!(ClassInfo2, cid), 0);
        assert_eq!(offset_of!(ClassInfo2, cardinality), 16);
        assert_eq!(offset_of!(ClassInfo2, category), 20);
        assert_eq!(offset_of!(ClassInfo2, name), 52);
        assert_eq!(offset_of!(ClassInfo2, class_flags), 116);
        assert_eq!(offset_of!(ClassInfo2, sub_categories), 120);
        assert_eq!(offset_of!(ClassInfo2, vendor), 248);
        assert_eq!(offset_of!(ClassInfo2, version), 312);
        assert_eq!(offset_of!(ClassInfo2, sdk_version), 376);
        assert_eq!(size_of::<ClassInfo2>(), 440);
    }

    #[test]
    fn the_factory_vtable_is_seven_pointers_in_order() {
        // Every slot after the first three is offset by them. A vtable one
        // slot out calls a different method with the wrong arguments.
        assert_eq!(size_of::<IPluginFactoryVtbl>(), 7 * 8);
        assert_eq!(offset_of!(IPluginFactoryVtbl, query_interface), 0);
        assert_eq!(offset_of!(IPluginFactoryVtbl, add_ref), 8);
        assert_eq!(offset_of!(IPluginFactoryVtbl, release), 16);
        assert_eq!(offset_of!(IPluginFactoryVtbl, get_factory_info), 24);
        assert_eq!(offset_of!(IPluginFactoryVtbl, count_classes), 32);
        assert_eq!(offset_of!(IPluginFactoryVtbl, get_class_info), 40);
        assert_eq!(offset_of!(IPluginFactoryVtbl, create_instance), 48);
    }

    #[test]
    fn the_second_factory_extends_the_first_rather_than_replacing_it() {
        assert_eq!(size_of::<IPluginFactory2Vtbl>(), 8 * 8);
        assert_eq!(offset_of!(IPluginFactory2Vtbl, base), 0);
        assert_eq!(offset_of!(IPluginFactory2Vtbl, get_class_info2), 56);
    }

    #[test]
    fn identifiers_are_built_in_com_byte_order() {
        // A GUID on Windows: the first word little-endian, the second as two
        // little-endian halves swapped, the last two big-endian. This is the
        // difference between every interface resolving and none of them.
        let id = tuid(0x0102_0304, 0x0506_0708, 0x090A_0B0C, 0x0D0E_0F10);
        if cfg!(windows) {
            assert_eq!(
                id,
                [0x04, 0x03, 0x02, 0x01, 0x06, 0x05, 0x08, 0x07, 9, 10, 11, 12, 13, 14, 15, 16]
            );
        } else {
            assert_eq!(id, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
        }
    }

    #[test]
    fn the_audio_processor_identifier_is_the_published_guid() {
        // 42043F99-B7DA-453C-A569-E79D9AAEC33D, as a Windows GUID: the first
        // three groups little-endian, the rest big-endian. Written out in
        // full because it is the value that had to be right for anything to
        // work at all.
        assert_eq!(
            IAUDIO_PROCESSOR_IID,
            if cfg!(windows) {
                [
                    0x99, 0x3F, 0x04, 0x42, 0xDA, 0xB7, 0x3C, 0x45, 0xA5, 0x69, 0xE7, 0x9D,
                    0x9A, 0xAE, 0xC3, 0x3D,
                ]
            } else {
                [
                    0x42, 0x04, 0x3F, 0x99, 0xB7, 0xDA, 0x45, 0x3C, 0xA5, 0x69, 0xE7, 0x9D,
                    0x9A, 0xAE, 0xC3, 0x3D,
                ]
            }
        );
    }

    #[test]
    fn the_factory_identifiers_are_sixteen_bytes_and_distinct() {
        assert_eq!(IPLUGIN_FACTORY_IID.len(), 16);
        assert_ne!(IPLUGIN_FACTORY_IID, IPLUGIN_FACTORY2_IID);
        assert_ne!(IPLUGIN_FACTORY2_IID, IPLUGIN_FACTORY3_IID);
    }


    #[test]
    fn the_process_setup_matches_the_published_layout() {
        // The sample rate is a double, so there is padding before it. Missing
        // that padding hands the plugin a rate read from the wrong bytes.
        assert_eq!(size_of::<ProcessSetup>(), 24);
        assert_eq!(offset_of!(ProcessSetup, process_mode), 0);
        assert_eq!(offset_of!(ProcessSetup, symbolic_sample_size), 4);
        assert_eq!(offset_of!(ProcessSetup, max_samples_per_block), 8);
        assert_eq!(offset_of!(ProcessSetup, sample_rate), 16);
    }

    #[test]
    fn the_bus_buffers_match_the_published_layout() {
        assert_eq!(size_of::<AudioBusBuffers>(), 24);
        assert_eq!(offset_of!(AudioBusBuffers, num_channels), 0);
        assert_eq!(offset_of!(AudioBusBuffers, silence_flags), 8);
        assert_eq!(offset_of!(AudioBusBuffers, channel_buffers), 16);
    }

    #[test]
    fn the_process_data_matches_the_published_layout() {
        assert_eq!(size_of::<ProcessData>(), 80);
        assert_eq!(offset_of!(ProcessData, process_mode), 0);
        assert_eq!(offset_of!(ProcessData, symbolic_sample_size), 4);
        assert_eq!(offset_of!(ProcessData, num_samples), 8);
        assert_eq!(offset_of!(ProcessData, num_inputs), 12);
        assert_eq!(offset_of!(ProcessData, num_outputs), 16);
        // Five ints then pointers, so there is padding at twenty.
        assert_eq!(offset_of!(ProcessData, inputs), 24);
        assert_eq!(offset_of!(ProcessData, outputs), 32);
        assert_eq!(offset_of!(ProcessData, input_parameter_changes), 40);
        assert_eq!(offset_of!(ProcessData, output_parameter_changes), 48);
        assert_eq!(offset_of!(ProcessData, input_events), 56);
        assert_eq!(offset_of!(ProcessData, output_events), 64);
        assert_eq!(offset_of!(ProcessData, process_context), 72);
    }

    #[test]
    fn the_bus_record_matches_the_published_layout() {
        assert_eq!(size_of::<BusInfo>(), 276);
        assert_eq!(offset_of!(BusInfo, media_type), 0);
        assert_eq!(offset_of!(BusInfo, direction), 4);
        assert_eq!(offset_of!(BusInfo, channel_count), 8);
        assert_eq!(offset_of!(BusInfo, name), 12);
        assert_eq!(offset_of!(BusInfo, bus_type), 268);
        assert_eq!(offset_of!(BusInfo, flags), 272);
    }

    #[test]
    fn the_component_vtable_is_fourteen_slots() {
        // Three from FUnknown, two from IPluginBase, nine of its own. One
        // slot out of place calls a different method entirely.
        assert_eq!(size_of::<IComponentVtbl>(), 14 * 8);
        assert_eq!(offset_of!(IComponentVtbl, base), 0);
        assert_eq!(offset_of!(IComponentVtbl, get_controller_class_id), 40);
        assert_eq!(offset_of!(IComponentVtbl, set_io_mode), 48);
        assert_eq!(offset_of!(IComponentVtbl, get_bus_count), 56);
        assert_eq!(offset_of!(IComponentVtbl, get_bus_info), 64);
        assert_eq!(offset_of!(IComponentVtbl, get_routing_info), 72);
        assert_eq!(offset_of!(IComponentVtbl, activate_bus), 80);
        assert_eq!(offset_of!(IComponentVtbl, set_active), 88);
        assert_eq!(offset_of!(IComponentVtbl, set_state), 96);
        assert_eq!(offset_of!(IComponentVtbl, get_state), 104);
    }

    #[test]
    fn the_plugin_base_vtable_is_five_slots() {
        assert_eq!(size_of::<IPluginBaseVtbl>(), 5 * 8);
        assert_eq!(offset_of!(IPluginBaseVtbl, initialize), 24);
        assert_eq!(offset_of!(IPluginBaseVtbl, terminate), 32);
    }

    #[test]
    fn the_processor_vtable_is_eleven_slots() {
        assert_eq!(size_of::<IAudioProcessorVtbl>(), 11 * 8);
        assert_eq!(offset_of!(IAudioProcessorVtbl, set_bus_arrangements), 24);
        assert_eq!(offset_of!(IAudioProcessorVtbl, get_bus_arrangement), 32);
        assert_eq!(offset_of!(IAudioProcessorVtbl, can_process_sample_size), 40);
        assert_eq!(offset_of!(IAudioProcessorVtbl, get_latency_samples), 48);
        assert_eq!(offset_of!(IAudioProcessorVtbl, setup_processing), 56);
        assert_eq!(offset_of!(IAudioProcessorVtbl, set_processing), 64);
        assert_eq!(offset_of!(IAudioProcessorVtbl, process), 72);
        assert_eq!(offset_of!(IAudioProcessorVtbl, get_tail_samples), 80);
    }

    #[test]
    fn the_base_interface_identifier_is_the_com_one() {
        // FUnknown shares IUnknown's identifier, which is a useful check that
        // the word order is right: it is a very well known value.
        assert_eq!(
            FUNKNOWN_IID,
            [0, 0, 0, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46]
        );
    }

    #[test]
    fn a_utf16_field_reads_up_to_its_first_null() {
        let mut raw = [0u16; 128];
        for (index, unit) in "Stereo In".encode_utf16().enumerate() {
            raw[index] = unit;
        }
        assert_eq!(field16(&raw), "Stereo In");
        assert_eq!(field16(&[0u16; 128]), "");
    }


    #[test]
    fn the_process_context_matches_the_published_layout() {
        // Handed to the plugin on every block. Too small and the plugin
        // writes past it; fields at the wrong offset give it nonsense for
        // the tempo and the transport position.
        assert_eq!(size_of::<ProcessContext>(), 112);
        assert_eq!(offset_of!(ProcessContext, state), 0);
        // The sample rate is a double, so there is padding after the state.
        assert_eq!(offset_of!(ProcessContext, sample_rate), 8);
        assert_eq!(offset_of!(ProcessContext, project_time_samples), 16);
        assert_eq!(offset_of!(ProcessContext, system_time), 24);
        assert_eq!(offset_of!(ProcessContext, continuous_time_samples), 32);
        assert_eq!(offset_of!(ProcessContext, project_time_music), 40);
        assert_eq!(offset_of!(ProcessContext, bar_position_music), 48);
        assert_eq!(offset_of!(ProcessContext, cycle_start_music), 56);
        assert_eq!(offset_of!(ProcessContext, cycle_end_music), 64);
        assert_eq!(offset_of!(ProcessContext, tempo), 72);
        assert_eq!(offset_of!(ProcessContext, time_sig_numerator), 80);
        assert_eq!(offset_of!(ProcessContext, time_sig_denominator), 84);
        assert_eq!(offset_of!(ProcessContext, chord), 88);
        assert_eq!(offset_of!(ProcessContext, smpte_offset_subframes), 92);
        assert_eq!(offset_of!(ProcessContext, frame_rate), 96);
        assert_eq!(offset_of!(ProcessContext, samples_to_next_clock), 104);
    }

    #[test]
    fn a_padded_field_reads_up_to_its_first_null() {
        let mut raw = [0 as c_char; 64];
        for (index, byte) in b"Steinberg".iter().enumerate() {
            raw[index] = *byte as c_char;
        }
        assert_eq!(field(&raw), "Steinberg");
    }

    #[test]
    fn a_field_that_fills_its_array_is_not_read_past() {
        // A name exactly as long as its field has no terminator.
        let raw = [b'A' as c_char; 8];
        assert_eq!(field(&raw), "AAAAAAAA");
    }

    #[test]
    fn an_empty_field_reads_as_nothing() {
        let raw = [0 as c_char; 32];
        assert_eq!(field(&raw), "");
    }
}
