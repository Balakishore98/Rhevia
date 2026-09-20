//! Hosting one VST3 effect: instantiate, set up, and process audio.
//!
//! The plugin is driven exactly as a live show drives it — realtime mode,
//! fixed block size, 32-bit float, stereo in and out — because a plugin that
//! only works when set up some other way is not one this can use.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use super::abi::{self, Object};
use super::{binary_within, Vst3Error};

/// Channels the host works in. The mixer is stereo throughout.
pub const CHANNELS: usize = 2;

/// A minimal `IHostApplication`.
///
/// Plugins ask the host context for this during `initialize`, and a good
/// number refuse to start without it. It has to outlive the plugin, so it is
/// owned by the instance and never moved.
#[repr(C)]
struct HostApplication {
    vtbl: *const HostApplicationVtbl,
}

#[repr(C)]
struct HostApplicationVtbl {
    query_interface:
        unsafe extern "C" fn(*mut c_void, *const abi::Tuid, *mut *mut c_void) -> abi::TResult,
    add_ref: unsafe extern "C" fn(*mut c_void) -> u32,
    release: unsafe extern "C" fn(*mut c_void) -> u32,
    get_name: unsafe extern "C" fn(*mut c_void, *mut u16) -> abi::TResult,
    create_instance:
        unsafe extern "C" fn(*mut c_void, *mut abi::Tuid, *mut abi::Tuid, *mut *mut c_void)
            -> abi::TResult,
}

unsafe extern "C" fn host_query_interface(
    this: *mut c_void,
    iid: *const abi::Tuid,
    obj: *mut *mut c_void,
) -> abi::TResult {
    if iid.is_null() || obj.is_null() {
        return abi::K_NO_INTERFACE;
    }
    let wanted = *iid;
    if wanted == abi::IHOST_APPLICATION_IID || wanted == abi::FUNKNOWN_IID {
        *obj = this;
        return abi::K_RESULT_OK;
    }
    *obj = std::ptr::null_mut();
    abi::K_NO_INTERFACE
}

// The host object is owned by the instance and outlives every plugin call, so
// the counts are nominal. Returning one keeps well-behaved plugins happy.
unsafe extern "C" fn host_add_ref(_: *mut c_void) -> u32 {
    1
}
unsafe extern "C" fn host_release(_: *mut c_void) -> u32 {
    1
}

unsafe extern "C" fn host_get_name(_: *mut c_void, name: *mut u16) -> abi::TResult {
    if name.is_null() {
        return abi::K_NO_INTERFACE;
    }
    // String128: a plugin will read up to 128 units, so the whole field is
    // written rather than just the text.
    let units: Vec<u16> = "Rhevia".encode_utf16().collect();
    for index in 0..128 {
        *name.add(index) = units.get(index).copied().unwrap_or(0);
    }
    abi::K_RESULT_OK
}

unsafe extern "C" fn host_create_instance(
    _: *mut c_void,
    _cid: *mut abi::Tuid,
    _iid: *mut abi::Tuid,
    obj: *mut *mut c_void,
) -> abi::TResult {
    // The host offers no objects of its own — message and attribute lists are
    // only needed by plugins that send messages, which none of this does.
    if !obj.is_null() {
        *obj = std::ptr::null_mut();
    }
    abi::K_NO_INTERFACE
}

static HOST_VTBL: HostApplicationVtbl = HostApplicationVtbl {
    query_interface: host_query_interface,
    add_ref: host_add_ref,
    release: host_release,
    get_name: host_get_name,
    create_instance: host_create_instance,
};

/// A loaded, running plugin.
pub struct Vst3Instance {
    /// Kept alive: every pointer below lives inside it, and dropping it
    /// shuts the module down before unloading it.
    _module: super::Module,
    factory: *mut c_void,
    component: *mut c_void,
    processor: *mut c_void,

    /// Owned by this instance, and referenced by the plugin. Boxed so its
    /// address does not change.
    _host: Box<HostApplication>,

    /// Planar scratch, one buffer per channel, reused every block.
    input_planes: Vec<Vec<f32>>,
    output_planes: Vec<Vec<f32>>,
    input_pointers: Vec<*mut f32>,
    output_pointers: Vec<*mut f32>,

    pub name: String,
    pub path: PathBuf,
    pub max_block: usize,
    /// True when the plugin declared an input bus. An effect has one; if it
    /// does not, there is nothing to feed it.
    pub has_input: bool,
    /// Channels the plugin's first input and output bus actually carry.
    ///
    /// Not assumed to be two. A plugin may refuse a stereo arrangement and
    /// keep its own, and it then writes one buffer per channel it believes it
    /// has — straight past the end of a two-entry array, which corrupts the
    /// heap rather than failing.
    pub input_channels: usize,
    pub output_channels: usize,
    /// Where the transport is. Advanced every block and handed to the plugin.
    context: abi::ProcessContext,
    /// Samples processed so far, which is what the transport position is.
    played: i64,
    active: bool,
}

// Driven from one thread at a time, behind the engine's own ownership. VST3
// requires exactly that: process is not reentrant.
unsafe impl Send for Vst3Instance {}

impl Vst3Instance {
    /// Loads `plugin` and gets it ready to process.
    pub fn open(
        module: &Path,
        cid: abi::Tuid,
        name: &str,
        sample_rate: f64,
        max_block: usize,
    ) -> Result<Self, Vst3Error> {
        let display = module.display().to_string();
        let binary =
            binary_within(module).ok_or_else(|| Vst3Error::NotAModule(display.clone()))?;

        // Every failure below simply returns: dropping this shuts the
        // module down and unloads it, which is the pairing that used to be
        // missing from each path in turn.
        let module_handle = super::Module::open(&binary, &display)?;

        unsafe {
            let get_factory: unsafe extern "C" fn() -> *mut c_void = module_handle
                .symbol(abi::ENTRY_FACTORY)
                .ok_or_else(|| Vst3Error::NotAModule(display.clone()))?;
            let factory = get_factory();
            if factory.is_null() {
                return Err(Vst3Error::NoFactory(display));
            }

            let factory_vtbl = &*(*(factory as *mut Object<abi::IPluginFactoryVtbl>)).vtbl;

            // The identifiers are passed as byte pointers; they are sixteen
            // raw bytes rather than text, despite the type.
            let mut component: *mut c_void = std::ptr::null_mut();
            let result = (factory_vtbl.create_instance)(
                factory,
                cid.as_ptr() as *const _,
                abi::ICOMPONENT_IID.as_ptr() as *const _,
                &mut component,
            );
            if result != abi::K_RESULT_OK || component.is_null() {
                (factory_vtbl.release)(factory);
                return Err(Vst3Error::Load(
                    name.to_string(),
                    "the plugin would not create its component".into(),
                ));
            }

            let host = Box::new(HostApplication { vtbl: &HOST_VTBL });
            let host_ptr = &*host as *const HostApplication as *mut c_void;

            let component_vtbl = &*(*(component as *mut Object<abi::IComponentVtbl>)).vtbl;

            if (component_vtbl.base.initialize)(component, host_ptr) != abi::K_RESULT_OK {
                (component_vtbl.base.release)(component);
                (factory_vtbl.release)(factory);
                return Err(Vst3Error::Load(
                    name.to_string(),
                    "the plugin refused to initialise".into(),
                ));
            }

            let mut processor: *mut c_void = std::ptr::null_mut();
            if (component_vtbl.base.query_interface)(
                component,
                &abi::IAUDIO_PROCESSOR_IID,
                &mut processor,
            ) != abi::K_RESULT_OK
                || processor.is_null()
            {
                (component_vtbl.base.terminate)(component);
                (component_vtbl.base.release)(component);
                (factory_vtbl.release)(factory);
                return Err(Vst3Error::Load(
                    name.to_string(),
                    "the plugin has no audio processor".into(),
                ));
            }

            let processor_vtbl = &*(*(processor as *mut Object<abi::IAudioProcessorVtbl>)).vtbl;

            // Everything here is 32-bit float. A plugin that cannot do that is
            // of no use, and finding out now is better than a silent failure.
            if (processor_vtbl.can_process_sample_size)(processor, abi::K_SAMPLE32)
                != abi::K_RESULT_OK
            {
                (processor_vtbl.release)(processor);
                (component_vtbl.base.terminate)(component);
                (component_vtbl.base.release)(component);
                (factory_vtbl.release)(factory);
                return Err(Vst3Error::Load(
                    name.to_string(),
                    "the plugin cannot process 32-bit float".into(),
                ));
            }

            let inputs = (component_vtbl.get_bus_count)(component, abi::K_AUDIO, abi::K_INPUT);
            let outputs = (component_vtbl.get_bus_count)(component, abi::K_AUDIO, abi::K_OUTPUT);
            let has_input = inputs > 0;

            // Ask for stereo both ways. A plugin may refuse and keep its own
            // arrangement, which is allowed, so the result is not fatal.
            let mut in_arrangement = abi::K_STEREO;
            let mut out_arrangement = abi::K_STEREO;
            (processor_vtbl.set_bus_arrangements)(
                processor,
                if inputs > 0 { &mut in_arrangement } else { std::ptr::null_mut() },
                inputs.min(1),
                if outputs > 0 { &mut out_arrangement } else { std::ptr::null_mut() },
                outputs.min(1),
            );

            let mut setup = abi::ProcessSetup {
                process_mode: abi::K_REALTIME,
                symbolic_sample_size: abi::K_SAMPLE32,
                max_samples_per_block: max_block as i32,
                sample_rate,
            };
            if (processor_vtbl.setup_processing)(processor, &mut setup) != abi::K_RESULT_OK {
                (processor_vtbl.release)(processor);
                (component_vtbl.base.terminate)(component);
                (component_vtbl.base.release)(component);
                (factory_vtbl.release)(factory);
                return Err(Vst3Error::Load(
                    name.to_string(),
                    "the plugin refused the processing setup".into(),
                ));
            }

            // What the buses actually turned out to be. Asking rather than
            // assuming is the whole point: the arrangement request above is
            // allowed to be refused, and a plugin writes one buffer per
            // channel it believes it has.
            let width = |direction: i32, count: i32| -> usize {
                if count <= 0 {
                    return 0;
                }
                let mut info = abi::BusInfo::default();
                if (component_vtbl.get_bus_info)(component, abi::K_AUDIO, direction, 0, &mut info)
                    == abi::K_RESULT_OK
                {
                    info.channel_count.max(0) as usize
                } else {
                    CHANNELS
                }
            };

            let input_channels = width(abi::K_INPUT, inputs);
            let output_channels = width(abi::K_OUTPUT, outputs);

            // A plugin claiming an implausible width is refused rather than
            // trusted: the number decides how much memory is handed over.
            const MAX_CHANNELS: usize = 64;
            if output_channels == 0 || output_channels > MAX_CHANNELS
                || input_channels > MAX_CHANNELS
            {
                (processor_vtbl.release)(processor);
                (component_vtbl.base.terminate)(component);
                (component_vtbl.base.release)(component);
                (factory_vtbl.release)(factory);
                return Err(Vst3Error::Load(
                    name.to_string(),
                    format!(
                        "the plugin reports {input_channels} in and {output_channels} out, \
                         which this host cannot drive"
                    ),
                ));
            }

            // Buses have to be switched on, or a plugin is handed audio it
            // believes is disconnected and writes nothing.
            for index in 0..inputs {
                (component_vtbl.activate_bus)(component, abi::K_AUDIO, abi::K_INPUT, index, 1);
            }
            for index in 0..outputs {
                (component_vtbl.activate_bus)(component, abi::K_AUDIO, abi::K_OUTPUT, index, 1);
            }

            (component_vtbl.set_active)(component, 1);
            (processor_vtbl.set_processing)(processor, 1);

            let planes = |count: usize| -> Vec<Vec<f32>> {
                (0..count).map(|_| vec![0.0f32; max_block]).collect()
            };

            let mut instance = Self {
                _module: module_handle,
                factory,
                component,
                processor,
                _host: host,
                input_planes: planes(input_channels),
                output_planes: planes(output_channels),
                input_pointers: vec![std::ptr::null_mut(); input_channels],
                output_pointers: vec![std::ptr::null_mut(); output_channels],
                name: name.to_string(),
                path: module.to_path_buf(),
                max_block,
                has_input,
                input_channels,
                output_channels,
                context: abi::ProcessContext {
                    // A live show is always playing, and the flags say which
                    // fields below are worth reading.
                    state: abi::K_PLAYING
                        | abi::K_TEMPO_VALID
                        | abi::K_TIME_SIG_VALID
                        | abi::K_PROJECT_TIME_MUSIC_VALID
                        | abi::K_CONT_TIME_VALID,
                    sample_rate,
                    // A plausible tempo and metre rather than zero: a
                    // tempo-synced delay set to nought beats is a division by
                    // zero inside somebody else's code.
                    tempo: 120.0,
                    time_sig_numerator: 4,
                    time_sig_denominator: 4,
                    ..Default::default()
                },
                played: 0,
                active: true,
            };
            instance.refresh_pointers();
            Ok(instance)
        }
    }

    /// Points the per-channel arrays at the current buffers.
    ///
    /// Recomputed rather than stored once: a `Vec` that reallocates moves its
    /// contents, and a stale pointer here is a plugin writing into freed
    /// memory.
    fn refresh_pointers(&mut self) {
        for channel in 0..self.input_planes.len() {
            self.input_pointers[channel] = self.input_planes[channel].as_mut_ptr();
        }
        for channel in 0..self.output_planes.len() {
            self.output_pointers[channel] = self.output_planes[channel].as_mut_ptr();
        }
    }

    /// Runs one block of interleaved stereo through the plugin, in place.
    ///
    /// A block longer than the setup allowed is processed in pieces rather
    /// than refused: the plugin was told a maximum and exceeding it is
    /// undefined behaviour in its own code.
    pub fn process(&mut self, interleaved: &mut [f32]) {
        if !self.active || interleaved.is_empty() {
            return;
        }
        let frames = interleaved.len() / CHANNELS;
        let mut done = 0;
        while done < frames {
            let take = (frames - done).min(self.max_block);
            self.process_block(&mut interleaved[done * CHANNELS..(done + take) * CHANNELS]);
            done += take;
        }
    }

    fn process_block(&mut self, interleaved: &mut [f32]) {
        let frames = interleaved.len() / CHANNELS;
        if frames == 0 {
            return;
        }

        // Interleaved in, planar across. A plugin wider than stereo gets
        // the signal on its first two channels and silence on the rest; a
        // mono one gets the left. Guessing something cleverer would be a
        // downmix the operator did not ask for.
        for plane in self.input_planes.iter_mut() {
            plane[..frames].fill(0.0);
        }
        for frame in 0..frames {
            for channel in 0..self.input_planes.len().min(CHANNELS) {
                self.input_planes[channel][frame] = interleaved[frame * CHANNELS + channel];
            }
        }
        for plane in self.output_planes.iter_mut() {
            plane[..frames].fill(0.0);
        }
        self.refresh_pointers();

        let mut input_bus = abi::AudioBusBuffers {
            num_channels: self.input_channels as i32,
            silence_flags: 0,
            channel_buffers: self.input_pointers.as_mut_ptr(),
        };
        let mut output_bus = abi::AudioBusBuffers {
            num_channels: self.output_channels as i32,
            silence_flags: 0,
            channel_buffers: self.output_pointers.as_mut_ptr(),
        };

        let mut data = abi::ProcessData {
            process_mode: abi::K_REALTIME,
            symbolic_sample_size: abi::K_SAMPLE32,
            num_samples: frames as i32,
            num_inputs: if self.has_input { 1 } else { 0 },
            num_outputs: 1,
            inputs: if self.has_input { &mut input_bus } else { std::ptr::null_mut() },
            outputs: &mut output_bus,
            // No automation and no notes: a channel strip effect needs
            // neither, and a null list is what the specification expects when
            // there is nothing to send. The context is different — plugins
            // read it without checking, and a null one crashes them.
            process_context: &mut self.context as *mut abi::ProcessContext as *mut c_void,
            ..Default::default()
        };

        unsafe {
            let vtbl = &*(*(self.processor as *mut Object<abi::IAudioProcessorVtbl>)).vtbl;
            if (vtbl.process)(self.processor, &mut data) != abi::K_RESULT_OK {
                // A plugin that fails a block leaves the audio untouched
                // rather than silencing the channel mid-show.
                return;
            }
        }

        // The transport moves on. Left at zero, anything that follows the
        // timeline sits frozen at the start of the show.
        self.played += frames as i64;
        self.context.project_time_samples = self.played;
        self.context.continuous_time_samples = self.played;
        self.context.project_time_music =
            self.played as f64 / self.context.sample_rate * (self.context.tempo / 60.0);

        // Planar out, interleaved back. Only the first two channels are
        // taken; a plugin with more has put its extra outputs somewhere the
        // mixer has no place for.
        let taken = self.output_planes.len().min(CHANNELS);
        for frame in 0..frames {
            for channel in 0..taken {
                interleaved[frame * CHANNELS + channel] = self.output_planes[channel][frame];
            }
            // A mono plugin feeds both sides rather than silencing the right.
            if taken == 1 {
                interleaved[frame * CHANNELS + 1] = self.output_planes[0][frame];
            }
        }
    }

    /// How much delay the plugin adds, in samples.
    pub fn latency_samples(&self) -> u32 {
        unsafe {
            let vtbl = &*(*(self.processor as *mut Object<abi::IAudioProcessorVtbl>)).vtbl;
            (vtbl.get_latency_samples)(self.processor)
        }
    }
}

impl Drop for Vst3Instance {
    fn drop(&mut self) {
        unsafe {
            let processor_vtbl = &*(*(self.processor as *mut Object<abi::IAudioProcessorVtbl>)).vtbl;
            let component_vtbl = &*(*(self.component as *mut Object<abi::IComponentVtbl>)).vtbl;

            // Taken down in the order the specification requires. Releasing a
            // plugin that is still processing is how a host crashes on exit.
            if self.active {
                (processor_vtbl.set_processing)(self.processor, 0);
                (component_vtbl.set_active)(self.component, 0);
                self.active = false;
            }
            (processor_vtbl.release)(self.processor);
            (component_vtbl.base.terminate)(self.component);
            (component_vtbl.base.release)(self.component);

            let factory_vtbl = &*(*(self.factory as *mut Object<abi::IPluginFactoryVtbl>)).vtbl;
            (factory_vtbl.release)(self.factory);
        }
        // The module is shut down and unloaded when its field drops, after
        // this body has released everything living inside it.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;
    const BLOCK: usize = 512;

    /// Opens the installed effects that pass validation, so the ABI is
    /// exercised against real plugins as well as against layout assertions.
    ///
    /// Validated first, in another process, for the same reason the engine
    /// does it: a plugin that faults takes down whatever is hosting it, and
    /// these tests are not exempt from that. Two of the plugins on the
    /// development machine do exactly this.
    fn open_each(mut each: impl FnMut(Vst3Instance)) {
        let usable: Vec<crate::PluginInfo> = crate::validated_effects()
            .into_iter()
            .filter(|p| p.usable == Some(true))
            .collect();

        if usable.is_empty() {
            eprintln!("SKIP: no VST3 effects that survive validation");
            return;
        }

        for plugin in usable {
            let Some(cid) = crate::parse_cid(&plugin.cid) else { continue };
            let path = std::path::PathBuf::from(&plugin.path);
            match Vst3Instance::open(&path, cid, &plugin.name, RATE, BLOCK) {
                Ok(instance) => each(instance),
                // A plugin may legitimately refuse: it may need a window, or
                // a licence, or hardware. That is not a fault in the host.
                Err(e) => eprintln!("  {} would not open: {e}", plugin.name),
            }
        }
    }

    #[test]
    fn an_installed_effect_can_be_opened_and_taken_down() {
        // The whole lifecycle: create, initialise, set up, activate, then
        // stop and release in the right order. Getting the order wrong
        // crashes rather than failing, so reaching the end is the result.
        open_each(|instance| {
            eprintln!("  opened {} (latency {})", instance.name, instance.latency_samples());
            assert!(!instance.name.is_empty());
            assert_eq!(instance.max_block, BLOCK);
        });
    }

    #[test]
    fn a_block_of_audio_can_be_pushed_through() {
        open_each(|mut instance| {
            let mut audio: Vec<f32> = (0..BLOCK * CHANNELS)
                .map(|n| 0.25 * (n as f32 * 0.01).sin())
                .collect();
            let before = audio.clone();

            instance.process(&mut audio);

            assert_eq!(audio.len(), before.len(), "the block changed length");
            assert!(
                audio.iter().all(|s| s.is_finite()),
                "{} produced values that are not finite",
                instance.name
            );
            assert!(
                audio.iter().all(|s| s.abs() <= 8.0),
                "{} produced implausibly loud output",
                instance.name
            );
        });
    }

    #[test]
    fn a_block_longer_than_the_setup_is_split_rather_than_overrunning() {
        // The plugin was told a maximum. Handing it more is undefined
        // behaviour inside its own code, so the host has to divide the work.
        open_each(|mut instance| {
            let mut audio = vec![0.1f32; BLOCK * 3 * CHANNELS];
            instance.process(&mut audio);
            assert_eq!(audio.len(), BLOCK * 3 * CHANNELS);
            assert!(audio.iter().all(|s| s.is_finite()));
        });
    }

    #[test]
    fn an_empty_block_is_harmless() {
        open_each(|mut instance| {
            let mut nothing: Vec<f32> = Vec::new();
            instance.process(&mut nothing);
            assert!(nothing.is_empty());
        });
    }

    #[test]
    fn a_real_module_asked_for_a_class_it_does_not_have_fails_cleanly() {
        // This is the case that used to corrupt the heap. The module loads and
        // starts, the class is refused, and the early return skipped the
        // shutdown that has to pair with the startup — so the crash landed on
        // unload, far from the mistake. It has to be a real module: a missing
        // file never gets far enough to start anything.
        let modules = crate::vst3::find_modules();
        let Some(module) = modules.first() else {
            eprintln!("SKIP: no VST3 modules installed");
            return;
        };

        let result = Vst3Instance::open(module, [0u8; 16], "nothing", RATE, BLOCK);
        assert!(
            result.is_err(),
            "a class the module does not have should be refused"
        );
        // Reaching here without the process dying is the rest of the result.
    }

    #[test]
    fn opening_a_plugin_that_is_not_there_fails_cleanly() {
        let result = Vst3Instance::open(
            Path::new("no-such-plugin-12345.vst3"),
            [0u8; 16],
            "nothing",
            RATE,
            BLOCK,
        );
        assert!(result.is_err());
    }

    #[test]
    fn opening_the_same_plugin_repeatedly_does_not_leak_or_crash() {
        // An operator adding and removing a plugin during a show does exactly
        // this. A reference released twice, or not at all, shows up here.
        let mut rounds = 0;
        for _ in 0..5 {
            open_each(|mut instance| {
                let mut audio = vec![0.2f32; BLOCK * CHANNELS];
                instance.process(&mut audio);
                rounds += 1;
            });
        }
        eprintln!("opened and closed {rounds} times");
    }

    #[test]
    fn the_host_reports_a_name_a_plugin_can_read() {
        // Plugins ask the host what it is called during initialise, and some
        // refuse to start if the answer is empty.
        let mut name = [0u16; 128];
        let result = unsafe { host_get_name(std::ptr::null_mut(), name.as_mut_ptr()) };
        assert_eq!(result, abi::K_RESULT_OK);
        assert_eq!(abi::field16(&name), "Rhevia");
    }

    #[test]
    fn the_host_answers_only_for_the_interfaces_it_has() {
        let host = Box::new(HostApplication { vtbl: &HOST_VTBL });
        let this = &*host as *const HostApplication as *mut c_void;

        let mut out: *mut c_void = std::ptr::null_mut();
        assert_eq!(
            unsafe { host_query_interface(this, &abi::IHOST_APPLICATION_IID, &mut out) },
            abi::K_RESULT_OK
        );
        assert_eq!(out, this);

        // Something it does not implement must be refused, not answered with
        // a pointer to the wrong object.
        let mut other: *mut c_void = std::ptr::null_mut();
        assert_eq!(
            unsafe { host_query_interface(this, &abi::IAUDIO_PROCESSOR_IID, &mut other) },
            abi::K_NO_INTERFACE
        );
        assert!(other.is_null());
    }
}
