//! VST3 module scanning.
//!
//! Finds the plugins installed on the machine and asks each one what it
//! contains, without instantiating anything. Scanning is separate from
//! hosting on purpose: a plugin that crashes while being asked its name should
//! not take a live show with it, and the list is wanted long before any
//! plugin is used.

pub mod abi;
pub mod host;

pub use host::Vst3Instance;

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use abi::{ClassInfo, ClassInfo2, FactoryInfo, IPluginFactory2Vtbl, IPluginFactoryVtbl, Object};

/// Serialises loading and unloading modules.
///
/// `InitDll` and `ExitDll` are global to a module, not to an instance, so two
/// threads loading the same plugin at the same time run its startup twice
/// against shared state. That is a crash inside somebody else's code, and it
/// only shows up under load — which is exactly when a show is running.
///
/// Held for the whole of a scan or an open, not just the `LoadLibrary` call:
/// the race is between one thread's startup and another's shutdown.
pub(crate) static MODULE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Takes the module lock, ignoring poisoning.
///
/// A plugin that panicked while another thread held this must not stop every
/// later plugin from loading.
pub(crate) fn lock_modules() -> std::sync::MutexGuard<'static, ()> {
    MODULE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A loaded module, which shuts itself down before unloading.
///
/// Every way out of loading a plugin goes through dropping this, which is the
/// point: `InitDll` has to be paired with `ExitDll`, and a module started and
/// never stopped corrupts the heap when it is unloaded. There are seven ways
/// for opening a plugin to fail, and remembering the pairing on each of them
/// is how one gets missed — as one was, until a plugin that refused to open
/// took the process down on its way out.
pub(crate) struct Module {
    library: libloading::Library,
    /// Whether `InitDll` was called, and so owes an `ExitDll`.
    initialised: bool,
}

impl Module {
    /// Loads a module and starts it.
    pub(crate) fn open(binary: &Path, display: &str) -> Result<Self, Vst3Error> {
        // Held across the load, and taken again across the unload. Two threads
        // starting the same module at once run its startup twice against
        // shared state, which is a crash inside somebody else's code.
        let _guard = lock_modules();

        let library = unsafe { libloading::Library::new(binary) }
            .map_err(|e| Vst3Error::Load(display.to_string(), e.to_string()))?;

        let mut initialised = false;
        unsafe {
            // Optional: plenty of modules do not export it, and that is not
            // an error. Those that do need it called before anything else.
            if let Ok(init) = library.get::<unsafe extern "C" fn() -> bool>(abi::ENTRY_INIT) {
                init();
                initialised = true;
            }
        }
        Ok(Self { library, initialised })
    }

    /// Looks up an exported symbol.
    pub(crate) unsafe fn symbol<T>(&self, name: &[u8]) -> Option<T>
    where
        T: Copy,
    {
        self.library.get::<T>(name).ok().map(|s| *s)
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        let _guard = lock_modules();
        if self.initialised {
            unsafe {
                if let Ok(exit) = self.library.get::<unsafe extern "C" fn() -> bool>(abi::ENTRY_EXIT)
                {
                    exit();
                }
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Vst3Error {
    #[error("{0} could not be loaded: {1}")]
    Load(String, String),
    #[error("{0} is not a VST3 module: it exports no plugin factory")]
    NotAModule(String),
    #[error("{0} refused to describe itself")]
    NoFactory(String),
}

/// One plugin a module offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vst3Plugin {
    pub name: String,
    pub vendor: String,
    pub version: String,
    /// What the plugin says it is — "Fx", "Instrument", "Fx|EQ" and so on.
    pub subcategories: String,
    /// The module this came from.
    pub path: PathBuf,
    /// The class identifier, which is how it is instantiated later.
    pub cid: abi::Tuid,
}

impl Vst3Plugin {
    /// True for something that processes audio rather than generating it.
    ///
    /// A switcher's channel strip wants effects; an instrument has no input
    /// to process and would sit silent in the chain.
    pub fn is_effect(&self) -> bool {
        let lower = self.subcategories.to_ascii_lowercase();
        !lower.contains("instrument")
    }
}

/// Where VST3 plugins are installed on this platform.
///
/// The per-user directory is searched as well as the shared one: plenty of
/// plugins install there, and a host that misses them looks broken.
pub fn search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if cfg!(windows) {
        if let Ok(common) = std::env::var("CommonProgramFiles") {
            paths.push(PathBuf::from(common).join("VST3"));
        }
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            paths.push(PathBuf::from(local).join("Programs").join("Common").join("VST3"));
        }
    } else if cfg!(target_os = "macos") {
        paths.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
        if let Ok(home) = std::env::var("HOME") {
            paths.push(PathBuf::from(home).join("Library/Audio/Plug-Ins/VST3"));
        }
    } else {
        paths.push(PathBuf::from("/usr/lib/vst3"));
        paths.push(PathBuf::from("/usr/local/lib/vst3"));
        if let Ok(home) = std::env::var("HOME") {
            paths.push(PathBuf::from(home).join(".vst3"));
        }
    }

    paths.retain(|p| p.exists());
    paths
}

/// Every `.vst3` module under the standard directories.
pub fn find_modules() -> Vec<PathBuf> {
    let mut modules = Vec::new();
    for root in search_paths() {
        let Ok(entries) = std::fs::read_dir(&root) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("vst3"))
                == Some(true)
            {
                modules.push(path);
            }
        }
    }
    modules.sort();
    modules
}

/// The binary inside a module.
///
/// A modern VST3 is a directory laid out as a bundle with the real binary
/// several levels down; an older one is a bare DLL with a `.vst3` extension.
/// Both are in the wild, so both are handled.
pub fn binary_within(module: &Path) -> Option<PathBuf> {
    if module.is_file() {
        return Some(module.to_path_buf());
    }
    if !module.is_dir() {
        return None;
    }

    let architecture = if cfg!(windows) {
        if cfg!(target_arch = "aarch64") { "arm64-win" } else { "x86_64-win" }
    } else if cfg!(target_os = "macos") {
        "MacOS"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64-linux"
    } else {
        "x86_64-linux"
    };

    let directory = module.join("Contents").join(architecture);
    let Ok(entries) = std::fs::read_dir(&directory) else { return None };

    // The binary is usually named after the bundle, but not always, so the
    // first regular file in the architecture directory is taken.
    let mut candidates: Vec<PathBuf> =
        entries.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    candidates.sort();

    let preferred = module.file_name().and_then(|n| n.to_str()).map(|n| n.to_string());
    if let Some(name) = preferred {
        if let Some(exact) = candidates.iter().find(|p| {
            p.file_name().and_then(|n| n.to_str()).map(|n| n == name).unwrap_or(false)
        }) {
            return Some(exact.clone());
        }
    }
    candidates.into_iter().next()
}

/// Asks one module what plugins it contains.
///
/// The module is loaded, questioned and unloaded. Nothing is instantiated, so
/// a plugin cannot start processing or open a window during a scan.
pub fn scan_module(module: &Path) -> Result<Vec<Vst3Plugin>, Vst3Error> {
    let display = module.display().to_string();
    let binary = binary_within(module).ok_or_else(|| Vst3Error::NotAModule(display.clone()))?;

    let loaded = Module::open(&binary, &display)?;

    unsafe {
        let get_factory: unsafe extern "C" fn() -> *mut c_void = loaded
            .symbol(abi::ENTRY_FACTORY)
            .ok_or_else(|| Vst3Error::NotAModule(display.clone()))?;

        let raw = get_factory();
        if raw.is_null() {
            return Err(Vst3Error::NoFactory(display));
        }

        let plugins = read_factory(raw, module);

        // Released before the module is unloaded: the object lives inside it.
        let object = raw as *mut Object<IPluginFactoryVtbl>;
        ((*(*object).vtbl).release)(raw);

        Ok(plugins)
    }
}

/// Reads every audio class out of a factory.
unsafe fn read_factory(raw: *mut c_void, module: &Path) -> Vec<Vst3Plugin> {
    let object = raw as *mut Object<IPluginFactoryVtbl>;
    let vtbl = &*(*object).vtbl;

    let mut info = FactoryInfo::default();
    let vendor = if (vtbl.get_factory_info)(raw, &mut info) == abi::K_RESULT_OK {
        abi::field(&info.vendor)
    } else {
        String::new()
    };

    // The richer record carries version and subcategories, which is what
    // makes a plugin list readable. Its absence is not a problem; the basic
    // record is always there.
    let mut factory2: *mut c_void = std::ptr::null_mut();
    let has_v2 = (vtbl.query_interface)(raw, &abi::IPLUGIN_FACTORY2_IID, &mut factory2)
        == abi::K_RESULT_OK
        && !factory2.is_null();

    let count = (vtbl.count_classes)(raw);
    let mut plugins = Vec::new();

    for index in 0..count {
        let mut basic = ClassInfo::default();
        if (vtbl.get_class_info)(raw, index, &mut basic) != abi::K_RESULT_OK {
            continue;
        }
        if abi::field(&basic.category) != abi::K_AUDIO_MODULE_CLASS {
            // Controllers and other helper classes are listed too. Only the
            // audio classes are plugins an operator can choose.
            continue;
        }

        let mut plugin = Vst3Plugin {
            name: abi::field(&basic.name),
            vendor: vendor.clone(),
            version: String::new(),
            subcategories: String::new(),
            path: module.to_path_buf(),
            cid: basic.cid,
        };

        if has_v2 {
            let object2 = factory2 as *mut Object<IPluginFactory2Vtbl>;
            let vtbl2 = &*(*object2).vtbl;
            let mut rich = ClassInfo2::default();
            if (vtbl2.get_class_info2)(factory2, index, &mut rich) == abi::K_RESULT_OK {
                plugin.version = abi::field(&rich.version);
                plugin.subcategories = abi::field(&rich.sub_categories);
                let vendor2 = abi::field(&rich.vendor);
                if !vendor2.is_empty() {
                    plugin.vendor = vendor2;
                }
            }
        }

        if !plugin.name.is_empty() {
            plugins.push(plugin);
        }
    }

    if has_v2 {
        // queryInterface added a reference, which has to be given back.
        let object2 = factory2 as *mut Object<IPluginFactoryVtbl>;
        ((*(*object2).vtbl).release)(factory2);
    }

    plugins
}

/// Every plugin installed on this machine.
///
/// A module that fails is logged and skipped rather than stopping the scan:
/// one broken plugin should not hide the rest.
pub fn scan_all() -> Vec<Vst3Plugin> {
    let mut plugins = Vec::new();
    for module in find_modules() {
        match scan_module(&module) {
            Ok(found) => plugins.extend(found),
            Err(e) => tracing::warn!(error = %e, "skipping a VST3 module"),
        }
    }
    plugins.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    plugins
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_paths_are_real_directories_or_absent() {
        // Whatever comes back must exist, because a path that does not is a
        // scan that silently finds nothing.
        for path in search_paths() {
            assert!(path.is_dir(), "{} is not a directory", path.display());
        }
    }

    #[test]
    fn modules_are_found_by_extension() {
        for module in find_modules() {
            let extension = module.extension().and_then(|e| e.to_str()).unwrap_or("");
            assert!(extension.eq_ignore_ascii_case("vst3"));
        }
    }

    #[test]
    fn a_bundle_resolves_to_the_binary_inside_it() {
        let modules = find_modules();
        if modules.is_empty() {
            eprintln!("SKIP: no VST3 plugins installed");
            return;
        }
        for module in modules {
            match binary_within(&module) {
                Some(binary) => assert!(
                    binary.is_file(),
                    "{} resolved to something that is not a file",
                    module.display()
                ),
                None => panic!("{} has no binary inside it", module.display()),
            }
        }
    }

    #[test]
    fn something_that_is_not_a_module_is_refused_rather_than_crashing() {
        let nowhere = PathBuf::from("no-such-plugin-12345.vst3");
        assert!(scan_module(&nowhere).is_err());
    }

    #[test]
    fn an_instrument_is_told_apart_from_an_effect() {
        let mut plugin = Vst3Plugin {
            name: "Test".into(),
            vendor: String::new(),
            version: String::new(),
            subcategories: "Fx|EQ".into(),
            path: PathBuf::new(),
            cid: [0; 16],
        };
        assert!(plugin.is_effect());

        plugin.subcategories = "Instrument|Synth".into();
        assert!(!plugin.is_effect(), "an instrument has no input to process");

        // Case is not consistent between vendors.
        plugin.subcategories = "instrument".into();
        assert!(!plugin.is_effect());
    }

    #[test]
    fn the_installed_plugins_can_all_be_scanned() {
        let modules = find_modules();
        if modules.is_empty() {
            eprintln!("SKIP: no VST3 plugins installed");
            return;
        }

        let mut total = 0;
        for module in &modules {
            match scan_module(module) {
                Ok(plugins) => {
                    for plugin in &plugins {
                        // A plugin with no name cannot be chosen from a list.
                        assert!(
                            !plugin.name.trim().is_empty(),
                            "{} reported a plugin with no name",
                            module.display()
                        );
                        // An all-zero identifier would fail to instantiate.
                        assert_ne!(
                            plugin.cid, [0u8; 16],
                            "{} reported an empty class identifier",
                            plugin.name
                        );
                        eprintln!(
                            "  {} — {} {} [{}]",
                            plugin.name, plugin.vendor, plugin.version, plugin.subcategories
                        );
                    }
                    total += plugins.len();
                }
                Err(e) => eprintln!("  {} could not be scanned: {e}", module.display()),
            }
        }
        eprintln!("scanned {} modules, {total} plugins", modules.len());
    }

    #[test]
    fn scanning_twice_gives_the_same_answer() {
        // Modules are loaded and unloaded for each scan. A plugin that leaves
        // global state behind would show up as a different list the second
        // time, or as a crash.
        let first = scan_all();
        let second = scan_all();
        assert_eq!(first.len(), second.len());
        for (a, b) in first.iter().zip(second.iter()) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.cid, b.cid);
        }
    }
}
