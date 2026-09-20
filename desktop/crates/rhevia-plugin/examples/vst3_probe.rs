//! Walks the VST3 host sequence one step at a time, printing as it goes.
//!
//! When a plugin takes the process down, the last line printed says which
//! call did it.

use std::ffi::c_void;

use rhevia_plugin::vst3::{self, abi, abi::Object};

fn main() {
    let plugins = vst3::scan_all();
    println!("{} plugins", plugins.len());

    for plugin in plugins {
        println!("\n=== {} ({}) ===", plugin.name, plugin.path.display());
        unsafe { walk(&plugin) };
    }
    println!("\nall steps completed");
}

unsafe fn walk(plugin: &vst3::Vst3Plugin) {
    let Some(binary) = vst3::binary_within(&plugin.path) else {
        println!("  no binary");
        return;
    };

    println!("  loading {}", binary.display());
    let Ok(library) = libloading::Library::new(&binary) else {
        println!("  load failed");
        return;
    };

    if let Ok(init) = library.get::<unsafe extern "C" fn() -> bool>(abi::ENTRY_INIT) {
        println!("  InitDll -> {}", init());
    } else {
        println!("  no InitDll");
    }

    let Ok(get_factory) = library.get::<unsafe extern "C" fn() -> *mut c_void>(abi::ENTRY_FACTORY)
    else {
        println!("  no GetPluginFactory");
        return;
    };
    let factory = get_factory();
    println!("  factory {factory:?}");
    if factory.is_null() {
        return;
    }
    let fv = &*(*(factory as *mut Object<abi::IPluginFactoryVtbl>)).vtbl;

    let mut component: *mut c_void = std::ptr::null_mut();
    let result = (fv.create_instance)(
        factory,
        plugin.cid.as_ptr() as *const _,
        abi::ICOMPONENT_IID.as_ptr() as *const _,
        &mut component,
    );
    println!("  createInstance -> {result} component {component:?}");
    if result != abi::K_RESULT_OK || component.is_null() {
        (fv.release)(factory);
        return;
    }

    let cv = &*(*(component as *mut Object<abi::IComponentVtbl>)).vtbl;

    println!("  initialize(null context)…");
    let init = (cv.base.initialize)(component, std::ptr::null_mut());
    println!("  initialize -> {init}");

    println!("  getBusCount…");
    let ins = (cv.get_bus_count)(component, abi::K_AUDIO, abi::K_INPUT);
    let outs = (cv.get_bus_count)(component, abi::K_AUDIO, abi::K_OUTPUT);
    let event_ins = (cv.get_bus_count)(component, abi::K_EVENT, abi::K_INPUT);
    println!("  audio buses: {ins} in, {outs} out;  event in: {event_ins}");

    for direction in [abi::K_INPUT, abi::K_OUTPUT] {
        let count = if direction == abi::K_INPUT { ins } else { outs };
        for index in 0..count {
            let mut info = abi::BusInfo::default();
            let r = (cv.get_bus_info)(component, abi::K_AUDIO, direction, index, &mut info);
            println!(
                "    {} bus {index}: r={r} channels={} name={:?} type={} flags={}",
                if direction == abi::K_INPUT { "in " } else { "out" },
                info.channel_count,
                abi::field16(&info.name),
                info.bus_type,
                info.flags
            );
        }
    }

    println!("  queryInterface(IAudioProcessor)…");
    let mut processor: *mut c_void = std::ptr::null_mut();
    let qr = (cv.base.query_interface)(component, &abi::IAUDIO_PROCESSOR_IID, &mut processor);
    println!("  -> {qr}, processor {processor:?}");

    if qr == abi::K_RESULT_OK && !processor.is_null() {
        let pv = &*(*(processor as *mut Object<abi::IAudioProcessorVtbl>)).vtbl;

        let can = (pv.can_process_sample_size)(processor, abi::K_SAMPLE32);
        println!("  canProcessSampleSize(32) -> {can}");

        println!("  getLatencySamples…");
        let latency = (pv.get_latency_samples)(processor);
        println!("  latency {latency}");

        let mut setup = abi::ProcessSetup {
            process_mode: abi::K_REALTIME,
            symbolic_sample_size: abi::K_SAMPLE32,
            max_samples_per_block: 512,
            sample_rate: 48_000.0,
        };
        println!("  setupProcessing…");
        let sr = (pv.setup_processing)(processor, &mut setup);
        println!("  setupProcessing -> {sr}");

        // ---- arrangements, then what the buses actually became --------
        let mut arr_in = abi::K_STEREO;
        let mut arr_out = abi::K_STEREO;
        let ar = (pv.set_bus_arrangements)(processor, &mut arr_in, 1, &mut arr_out, 1);
        println!("  setBusArrangements(stereo) -> {ar}");

        for direction in [abi::K_INPUT, abi::K_OUTPUT] {
            let mut info = abi::BusInfo::default();
            let r = (cv.get_bus_info)(component, abi::K_AUDIO, direction, 0, &mut info);
            println!(
                "    after arrangement, {} bus: r={r} channels={}",
                if direction == abi::K_INPUT { "in " } else { "out" },
                info.channel_count
            );
        }

        println!("  activateBus…");
        (cv.activate_bus)(component, abi::K_AUDIO, abi::K_INPUT, 0, 1);
        (cv.activate_bus)(component, abi::K_AUDIO, abi::K_OUTPUT, 0, 1);
        println!("  setActive(true)…");
        (cv.set_active)(component, 1);
        println!("  setProcessing(true)…");
        let spr = (pv.set_processing)(processor, 1);
        println!("  setProcessing -> {spr}");

        // ---- one block, with the widths the plugin reported -------------
        let mut info = abi::BusInfo::default();
        (cv.get_bus_info)(component, abi::K_AUDIO, abi::K_INPUT, 0, &mut info);
        let in_ch = info.channel_count.max(1) as usize;
        let mut info = abi::BusInfo::default();
        (cv.get_bus_info)(component, abi::K_AUDIO, abi::K_OUTPUT, 0, &mut info);
        let out_ch = info.channel_count.max(1) as usize;
        println!("  processing with {in_ch} in, {out_ch} out");

        let frames = 512usize;
        let mut in_planes: Vec<Vec<f32>> = (0..in_ch).map(|_| vec![0.1f32; frames]).collect();
        let mut out_planes: Vec<Vec<f32>> = (0..out_ch).map(|_| vec![0.0f32; frames]).collect();
        let mut in_ptrs: Vec<*mut f32> = in_planes.iter_mut().map(|p| p.as_mut_ptr()).collect();
        let mut out_ptrs: Vec<*mut f32> = out_planes.iter_mut().map(|p| p.as_mut_ptr()).collect();

        let mut ib = abi::AudioBusBuffers {
            num_channels: in_ch as i32,
            silence_flags: 0,
            channel_buffers: in_ptrs.as_mut_ptr(),
        };
        let mut ob = abi::AudioBusBuffers {
            num_channels: out_ch as i32,
            silence_flags: 0,
            channel_buffers: out_ptrs.as_mut_ptr(),
        };

        let mut context = abi::ProcessContext {
            state: abi::K_PLAYING | abi::K_TEMPO_VALID | abi::K_TIME_SIG_VALID,
            sample_rate: 48_000.0,
            tempo: 120.0,
            time_sig_numerator: 4,
            time_sig_denominator: 4,
            ..Default::default()
        };

        println!("  process (with context)…");
        let mut data = abi::ProcessData {
            process_mode: abi::K_REALTIME,
            symbolic_sample_size: abi::K_SAMPLE32,
            num_samples: frames as i32,
            num_inputs: 1,
            num_outputs: 1,
            inputs: &mut ib,
            outputs: &mut ob,
            process_context: &mut context as *mut abi::ProcessContext as *mut c_void,
            ..Default::default()
        };
        let pr = (pv.process)(processor, &mut data);
        println!("  process -> {pr}");

        println!("  setProcessing(false)…");
        (pv.set_processing)(processor, 0);
        println!("  setActive(false)…");
        (cv.set_active)(component, 0);

        println!("  releasing processor…");
        (pv.release)(processor);
    }

    println!("  terminate…");
    (cv.base.terminate)(component);
    println!("  releasing component…");
    (cv.base.release)(component);
    println!("  releasing factory…");
    (fv.release)(factory);
    println!("  done");
}
