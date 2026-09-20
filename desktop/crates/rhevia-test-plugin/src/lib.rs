//! A minimal VST3 effect that halves its input.
//!
//! This exists so the host has a plugin whose behaviour is known exactly. The
//! plugins that happen to be installed on any given machine are a poor test:
//! when one crashes there is no way to tell a host bug from a plugin bug, and
//! that ambiguity cost real time to discover.
//!
//! It implements the same interface declarations the host uses, from the other
//! side. That makes it a two-way check: if the host and this plugin agree on
//! every vtable and struct, audio comes out at exactly half amplitude, and
//! anything else is a mismatch that points at the declarations.
//!
//! Not shipped — it is a test fixture, built into the target directory and
//! loaded from there.

use std::ffi::{c_char, c_void};

use rhevia_plugin::vst3::abi;

/// This plugin's class identifier. Arbitrary, but fixed.
const PLUGIN_CID: abi::Tuid = abi::tuid(0x5268_6576, 0x6961_5465, 0x7374_4678, 0x0000_0001);

/// What the effect does, so a test can assert an exact value rather than
/// "something happened".
pub const GAIN: f32 = 0.5;

/* -------------------------------------------------------------------- */
/*  The component, which is also the audio processor                     */
/* -------------------------------------------------------------------- */

/// One instance.
///
/// The two interfaces are separate COM objects sharing one allocation, which
/// is how the SDK's own plugins do it: `queryInterface` hands out the second
/// vtable, and both keep the whole thing alive.
#[repr(C)]
struct Component {
    /// Must be first: a pointer to this struct is a pointer to this field.
    component_vtbl: *const abi::IComponentVtbl,
    /// The processor interface, offset within the same object.
    processor_vtbl: *const abi::IAudioProcessorVtbl,
    references: std::sync::atomic::AtomicU32,
    active: bool,
    sample_rate: f64,
    max_block: i32,
}

/// The offset from a processor pointer back to the component that owns it.
const PROCESSOR_OFFSET: usize = std::mem::size_of::<*const c_void>();

unsafe fn component_from_processor(this: *mut c_void) -> *mut Component {
    (this as *mut u8).sub(PROCESSOR_OFFSET) as *mut Component
}

unsafe fn shared_query(
    component: *mut Component,
    iid: *const abi::Tuid,
    obj: *mut *mut c_void,
) -> abi::TResult {
    if iid.is_null() || obj.is_null() {
        return abi::K_NO_INTERFACE;
    }
    let wanted = *iid;
    if wanted == abi::ICOMPONENT_IID
        || wanted == abi::IPLUGIN_BASE_IID
        || wanted == abi::FUNKNOWN_IID
    {
        (*component).references.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        *obj = component as *mut c_void;
        return abi::K_RESULT_OK;
    }
    if wanted == abi::IAUDIO_PROCESSOR_IID {
        (*component).references.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        *obj = std::ptr::addr_of_mut!((*component).processor_vtbl) as *mut c_void;
        return abi::K_RESULT_OK;
    }
    *obj = std::ptr::null_mut();
    abi::K_NO_INTERFACE
}

unsafe fn shared_release(component: *mut Component) -> u32 {
    let left = (*component).references.fetch_sub(1, std::sync::atomic::Ordering::Relaxed) - 1;
    if left == 0 {
        drop(Box::from_raw(component));
    }
    left
}

/* ---- IComponent ----------------------------------------------------- */

unsafe extern "C" fn c_query(
    this: *mut c_void,
    iid: *const abi::Tuid,
    obj: *mut *mut c_void,
) -> abi::TResult {
    shared_query(this as *mut Component, iid, obj)
}

unsafe extern "C" fn c_add_ref(this: *mut c_void) -> u32 {
    let component = this as *mut Component;
    (*component).references.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

unsafe extern "C" fn c_release(this: *mut c_void) -> u32 {
    shared_release(this as *mut Component)
}

unsafe extern "C" fn c_initialize(_: *mut c_void, _context: *mut c_void) -> abi::TResult {
    // The host context is deliberately not required: a plugin that insists on
    // one is a plugin that cannot be scanned.
    abi::K_RESULT_OK
}

unsafe extern "C" fn c_terminate(_: *mut c_void) -> abi::TResult {
    abi::K_RESULT_OK
}

unsafe extern "C" fn c_get_controller_class_id(_: *mut c_void, _: *mut abi::Tuid) -> abi::TResult {
    abi::K_NO_INTERFACE
}

unsafe extern "C" fn c_set_io_mode(_: *mut c_void, _: i32) -> abi::TResult {
    abi::K_RESULT_OK
}

unsafe extern "C" fn c_get_bus_count(_: *mut c_void, media: i32, _dir: i32) -> i32 {
    if media == abi::K_AUDIO {
        1
    } else {
        0
    }
}

unsafe extern "C" fn c_get_bus_info(
    _: *mut c_void,
    media: i32,
    direction: i32,
    index: i32,
    info: *mut abi::BusInfo,
) -> abi::TResult {
    if media != abi::K_AUDIO || index != 0 || info.is_null() {
        return abi::K_NO_INTERFACE;
    }
    let target = &mut *info;
    target.media_type = media;
    target.direction = direction;
    target.channel_count = 2;
    target.bus_type = 0;
    target.flags = 1;

    let label = if direction == abi::K_INPUT { "In" } else { "Out" };
    for (index, unit) in label.encode_utf16().enumerate() {
        target.name[index] = unit;
    }
    abi::K_RESULT_OK
}

unsafe extern "C" fn c_get_routing_info(
    _: *mut c_void,
    _: *mut c_void,
    _: *mut c_void,
) -> abi::TResult {
    abi::K_NO_INTERFACE
}

unsafe extern "C" fn c_activate_bus(
    _: *mut c_void,
    _: i32,
    _: i32,
    _: i32,
    _: u8,
) -> abi::TResult {
    abi::K_RESULT_OK
}

unsafe extern "C" fn c_set_active(this: *mut c_void, state: u8) -> abi::TResult {
    (*(this as *mut Component)).active = state != 0;
    abi::K_RESULT_OK
}

unsafe extern "C" fn c_set_state(_: *mut c_void, _: *mut c_void) -> abi::TResult {
    abi::K_RESULT_OK
}

unsafe extern "C" fn c_get_state(_: *mut c_void, _: *mut c_void) -> abi::TResult {
    abi::K_RESULT_OK
}

static COMPONENT_VTBL: abi::IComponentVtbl = abi::IComponentVtbl {
    base: abi::IPluginBaseVtbl {
        query_interface: c_query,
        add_ref: c_add_ref,
        release: c_release,
        initialize: c_initialize,
        terminate: c_terminate,
    },
    get_controller_class_id: c_get_controller_class_id,
    set_io_mode: c_set_io_mode,
    get_bus_count: c_get_bus_count,
    get_bus_info: c_get_bus_info,
    get_routing_info: c_get_routing_info,
    activate_bus: c_activate_bus,
    set_active: c_set_active,
    set_state: c_set_state,
    get_state: c_get_state,
};

/* ---- IAudioProcessor ------------------------------------------------ */

unsafe extern "C" fn p_query(
    this: *mut c_void,
    iid: *const abi::Tuid,
    obj: *mut *mut c_void,
) -> abi::TResult {
    shared_query(component_from_processor(this), iid, obj)
}

unsafe extern "C" fn p_add_ref(this: *mut c_void) -> u32 {
    let component = component_from_processor(this);
    (*component).references.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

unsafe extern "C" fn p_release(this: *mut c_void) -> u32 {
    shared_release(component_from_processor(this))
}

unsafe extern "C" fn p_set_bus_arrangements(
    _: *mut c_void,
    _: *mut u64,
    _: i32,
    _: *mut u64,
    _: i32,
) -> abi::TResult {
    // Stereo either way, whatever is asked for.
    abi::K_RESULT_OK
}

unsafe extern "C" fn p_get_bus_arrangement(
    _: *mut c_void,
    _: i32,
    _: i32,
    arrangement: *mut u64,
) -> abi::TResult {
    if !arrangement.is_null() {
        *arrangement = abi::K_STEREO;
    }
    abi::K_RESULT_OK
}

unsafe extern "C" fn p_can_process_sample_size(_: *mut c_void, size: i32) -> abi::TResult {
    if size == abi::K_SAMPLE32 {
        abi::K_RESULT_OK
    } else {
        abi::K_NO_INTERFACE
    }
}

unsafe extern "C" fn p_get_latency_samples(_: *mut c_void) -> u32 {
    0
}

unsafe extern "C" fn p_setup_processing(
    this: *mut c_void,
    setup: *mut abi::ProcessSetup,
) -> abi::TResult {
    if setup.is_null() {
        return abi::K_NO_INTERFACE;
    }
    let component = component_from_processor(this);
    (*component).sample_rate = (*setup).sample_rate;
    (*component).max_block = (*setup).max_samples_per_block;

    // A host that gets the struct layout wrong hands over nonsense here, so
    // this refuses rather than processing with it.
    if (*setup).sample_rate < 8_000.0 || (*setup).sample_rate > 768_000.0 {
        return abi::K_NO_INTERFACE;
    }
    if (*setup).max_samples_per_block <= 0 || (*setup).max_samples_per_block > 1 << 20 {
        return abi::K_NO_INTERFACE;
    }
    abi::K_RESULT_OK
}

unsafe extern "C" fn p_set_processing(_: *mut c_void, _: u8) -> abi::TResult {
    abi::K_RESULT_OK
}

unsafe extern "C" fn p_process(this: *mut c_void, data: *mut abi::ProcessData) -> abi::TResult {
    if data.is_null() {
        return abi::K_NO_INTERFACE;
    }
    let data = &mut *data;
    let component = component_from_processor(this);

    // The checks a real plugin makes, which is what turns a host mistake into
    // a returned error rather than a crash.
    if data.num_samples <= 0 || data.num_outputs < 1 || data.outputs.is_null() {
        return abi::K_NO_INTERFACE;
    }
    if data.num_samples > (*component).max_block {
        return abi::K_NO_INTERFACE;
    }
    if data.symbolic_sample_size != abi::K_SAMPLE32 {
        return abi::K_NO_INTERFACE;
    }

    let frames = data.num_samples as usize;
    let output = &mut *data.outputs;
    if output.channel_buffers.is_null() || output.num_channels < 1 {
        return abi::K_NO_INTERFACE;
    }

    let silent_input = data.num_inputs < 1 || data.inputs.is_null();

    for channel in 0..output.num_channels as usize {
        let out = *output.channel_buffers.add(channel);
        if out.is_null() {
            return abi::K_NO_INTERFACE;
        }
        let out = std::slice::from_raw_parts_mut(out, frames);

        if silent_input {
            out.fill(0.0);
            continue;
        }
        let input = &*data.inputs;
        if channel >= input.num_channels as usize || input.channel_buffers.is_null() {
            out.fill(0.0);
            continue;
        }
        let source = *input.channel_buffers.add(channel);
        if source.is_null() {
            out.fill(0.0);
            continue;
        }
        let source = std::slice::from_raw_parts(source, frames);
        for (target, sample) in out.iter_mut().zip(source.iter()) {
            *target = sample * GAIN;
        }
    }

    abi::K_RESULT_OK
}

unsafe extern "C" fn p_get_tail_samples(_: *mut c_void) -> u32 {
    0
}

static PROCESSOR_VTBL: abi::IAudioProcessorVtbl = abi::IAudioProcessorVtbl {
    query_interface: p_query,
    add_ref: p_add_ref,
    release: p_release,
    set_bus_arrangements: p_set_bus_arrangements,
    get_bus_arrangement: p_get_bus_arrangement,
    can_process_sample_size: p_can_process_sample_size,
    get_latency_samples: p_get_latency_samples,
    setup_processing: p_setup_processing,
    set_processing: p_set_processing,
    process: p_process,
    get_tail_samples: p_get_tail_samples,
};

/* -------------------------------------------------------------------- */
/*  The factory                                                          */
/* -------------------------------------------------------------------- */

#[repr(C)]
struct Factory {
    vtbl: *const abi::IPluginFactoryVtbl,
}

unsafe extern "C" fn f_query(
    this: *mut c_void,
    iid: *const abi::Tuid,
    obj: *mut *mut c_void,
) -> abi::TResult {
    if iid.is_null() || obj.is_null() {
        return abi::K_NO_INTERFACE;
    }
    let wanted = *iid;
    if wanted == abi::IPLUGIN_FACTORY_IID || wanted == abi::FUNKNOWN_IID {
        *obj = this;
        return abi::K_RESULT_OK;
    }
    // No version 2 factory, so the host has to cope with its absence — which
    // is worth exercising, because plenty of real plugins are the same.
    *obj = std::ptr::null_mut();
    abi::K_NO_INTERFACE
}

unsafe extern "C" fn f_add_ref(_: *mut c_void) -> u32 {
    1
}

unsafe extern "C" fn f_release(_: *mut c_void) -> u32 {
    1
}

/// Writes a string into a fixed C field, null padded.
unsafe fn write_field(field: &mut [c_char], text: &str) {
    for slot in field.iter_mut() {
        *slot = 0;
    }
    for (index, byte) in text.bytes().enumerate() {
        if index + 1 >= field.len() {
            break;
        }
        field[index] = byte as c_char;
    }
}

unsafe extern "C" fn f_get_factory_info(
    _: *mut c_void,
    info: *mut abi::FactoryInfo,
) -> abi::TResult {
    if info.is_null() {
        return abi::K_NO_INTERFACE;
    }
    let info = &mut *info;
    write_field(&mut info.vendor, "Rhevia");
    write_field(&mut info.url, "https://example.invalid");
    write_field(&mut info.email, "nobody@example.invalid");
    info.flags = 0;
    abi::K_RESULT_OK
}

unsafe extern "C" fn f_count_classes(_: *mut c_void) -> i32 {
    1
}

unsafe extern "C" fn f_get_class_info(
    _: *mut c_void,
    index: i32,
    info: *mut abi::ClassInfo,
) -> abi::TResult {
    if index != 0 || info.is_null() {
        return abi::K_NO_INTERFACE;
    }
    let info = &mut *info;
    info.cid = PLUGIN_CID;
    info.cardinality = 0x7FFF_FFFF;
    write_field(&mut info.category, abi::K_AUDIO_MODULE_CLASS);
    write_field(&mut info.name, "Rhevia Test Gain");
    abi::K_RESULT_OK
}

unsafe extern "C" fn f_create_instance(
    _: *mut c_void,
    cid: *const c_char,
    iid: *const c_char,
    obj: *mut *mut c_void,
) -> abi::TResult {
    if cid.is_null() || iid.is_null() || obj.is_null() {
        return abi::K_NO_INTERFACE;
    }
    let wanted_class = *(cid as *const abi::Tuid);
    if wanted_class != PLUGIN_CID {
        return abi::K_NO_INTERFACE;
    }

    let component = Box::into_raw(Box::new(Component {
        component_vtbl: &COMPONENT_VTBL,
        processor_vtbl: &PROCESSOR_VTBL,
        references: std::sync::atomic::AtomicU32::new(1),
        active: false,
        sample_rate: 48_000.0,
        max_block: 512,
    }));

    let wanted_interface = *(iid as *const abi::Tuid);
    let mut out: *mut c_void = std::ptr::null_mut();
    if shared_query(component, &wanted_interface, &mut out) != abi::K_RESULT_OK {
        shared_release(component);
        return abi::K_NO_INTERFACE;
    }
    // The query added a reference on top of the one the object was made with.
    shared_release(component);
    *obj = out;
    abi::K_RESULT_OK
}

static FACTORY_VTBL: abi::IPluginFactoryVtbl = abi::IPluginFactoryVtbl {
    query_interface: f_query,
    add_ref: f_add_ref,
    release: f_release,
    get_factory_info: f_get_factory_info,
    count_classes: f_count_classes,
    get_class_info: f_get_class_info,
    create_instance: f_create_instance,
};

static mut FACTORY: Factory = Factory { vtbl: &FACTORY_VTBL };

/* -------------------------------------------------------------------- */
/*  Module entry points                                                  */
/* -------------------------------------------------------------------- */

#[no_mangle]
pub extern "C" fn InitDll() -> bool {
    true
}

#[no_mangle]
pub extern "C" fn ExitDll() -> bool {
    true
}

#[no_mangle]
pub extern "C" fn GetPluginFactory() -> *mut c_void {
    std::ptr::addr_of_mut!(FACTORY) as *mut c_void
}
