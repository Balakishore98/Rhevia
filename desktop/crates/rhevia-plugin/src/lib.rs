//! Audio plugin hosting.
//!
//! # Licensing, plainly
//!
//! Nothing from Steinberg's VST 3 SDK is vendored into this tree. The code in
//! [`vst3::abi`] declares a published binary interface so that Rhevia can
//! interoperate with plugins the user has already installed — the same
//! relationship it has with the NDI runtime.
//!
//! That is not the whole story, and the rest is not a decision this code can
//! make. Shipping VST3 support in software you sell, and calling it "VST",
//! needs Steinberg's VST 3 Plug-In Licensing Agreement. It costs nothing but
//! it has to be signed by whoever sells the software. Until it is, this is
//! usable for your own productions; it is not something to put in a box with a
//! price on it.
//!
//! The alternative worth knowing about is CLAP, which is MIT licensed and
//! carries no such condition. The [`Host`] trait below is the seam a second
//! backend goes behind, which is why it exists rather than the scanner simply
//! being called directly.

pub mod vst3;

pub use vst3::{Vst3Error, Vst3Plugin};

/// A plugin, whatever format it came in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInfo {
    pub name: String,
    pub vendor: String,
    pub version: String,
    /// "VST3" today. The field exists because it will not always be.
    pub format: &'static str,
    /// What the plugin says it does, as the vendor wrote it.
    pub category: String,
    /// Where it came from, for the interface to show on hover.
    pub path: String,
    /// The class identifier, as hex. What the validator and the engine use to
    /// ask for this particular plugin out of a module that may hold several.
    pub cid: String,
    /// Whether the plugin survived being opened and given a block of audio in
    /// a separate process.
    ///
    /// `None` when it has not been checked. A plugin that crashes during
    /// validation is still listed — an operator looking for it should be told
    /// why it cannot be used, not left wondering where it went.
    pub usable: Option<bool>,
}

impl From<&Vst3Plugin> for PluginInfo {
    fn from(plugin: &Vst3Plugin) -> Self {
        Self {
            name: plugin.name.clone(),
            vendor: plugin.vendor.clone(),
            version: plugin.version.clone(),
            format: "VST3",
            category: plugin.subcategories.clone(),
            path: plugin.path.display().to_string(),
            cid: hex_cid(&plugin.cid),
            usable: None,
        }
    }
}

/// A class identifier as hex, which is how it is passed to another process.
pub fn hex_cid(cid: &vst3::abi::Tuid) -> String {
    cid.iter().map(|b| format!("{b:02x}")).collect()
}

/// Reads a class identifier back from hex.
pub fn parse_cid(text: &str) -> Option<vst3::abi::Tuid> {
    if text.len() != 32 {
        return None;
    }
    let mut cid = [0u8; 16];
    for (index, byte) in cid.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(cid)
}

/// The validator built alongside this crate.
fn validator() -> Option<std::path::PathBuf> {
    let mut directory = std::env::current_exe().ok()?;
    directory.pop();

    let name = if cfg!(windows) { "rhevia-vst3-validate.exe" } else { "rhevia-vst3-validate" };

    // Beside the running program when installed; one level up when running
    // from a test binary, which cargo puts in a deps directory of its own.
    let mut candidates = vec![directory.join(name), directory.join("deps").join(name)];
    if let Some(parent) = directory.parent() {
        candidates.push(parent.join(name));
    }
    candidates.into_iter().find(|c| c.exists())
}

/// Opens a plugin in a separate process and gives it a block of audio.
///
/// This is the whole reason the validator is a separate program. A plugin
/// that faults takes down whichever process is hosting it, and that must not
/// be the one carrying the show. A plugin is only allowed into the live chain
/// once it has survived this.
///
/// Returns `None` when there is no validator to run, which means the question
/// could not be asked rather than that the answer was no.
pub fn validate(plugin: &PluginInfo) -> Option<bool> {
    let validator = validator()?;

    let mut child = std::process::Command::new(validator)
        .arg(&plugin.path)
        .arg(&plugin.cid)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;

    // A plugin that hangs is as unusable as one that crashes, and far harder
    // to notice, so the wait is bounded.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.success()),
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Some(false);
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(_) => return Some(false),
        }
    }
}

/// Every effect installed, each checked in a separate process first.
///
/// Slower than [`installed_effects`] by the cost of one process per plugin,
/// and worth it: this is the list it is safe to put on air.
pub fn validated_effects() -> Vec<PluginInfo> {
    installed_effects()
        .into_iter()
        .map(|mut plugin| {
            plugin.usable = validate(&plugin);
            plugin
        })
        .collect()
}

/// Every audio effect installed on this machine.
///
/// Instruments are left out: a channel strip processes audio it is given, and
/// an instrument has no input, so it would sit silent in the chain.
pub fn installed_effects() -> Vec<PluginInfo> {
    vst3::scan_all().iter().filter(|p| p.is_effect()).map(PluginInfo::from).collect()
}

/// Every plugin installed, effects and instruments alike.
pub fn installed() -> Vec<PluginInfo> {
    vst3::scan_all().iter().map(PluginInfo::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_class_identifier_survives_being_written_as_hex() {
        // It travels to another process as text, and a plugin asked for the
        // wrong class is one that silently fails to validate.
        let cid: vst3::abi::Tuid = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD,
            0xEE, 0xFF,
        ];
        let text = hex_cid(&cid);
        assert_eq!(text.len(), 32);
        assert_eq!(parse_cid(&text), Some(cid));
    }

    #[test]
    fn nonsense_hex_is_refused_rather_than_guessed_at() {
        assert_eq!(parse_cid(""), None);
        assert_eq!(parse_cid("abcd"), None);
        assert_eq!(parse_cid(&"z".repeat(32)), None);
    }

    #[test]
    fn every_installed_plugin_is_checked_in_its_own_process() {
        // The point of the exercise: a plugin that crashes is reported as
        // unusable rather than taking this process with it. Two real NDI
        // plugins on the development machine do exactly that, which is how
        // this was found worth building.
        let checked = validated_effects();
        if checked.is_empty() {
            eprintln!("SKIP: no VST3 effects installed");
            return;
        }
        for plugin in &checked {
            eprintln!(
                "  {} — {}",
                plugin.name,
                match plugin.usable {
                    Some(true) => "usable",
                    Some(false) => "crashed or hung during validation",
                    None => "not checked",
                }
            );
        }
        // Reaching here at all is the result: the crashing plugins did not
        // bring this process down with them.
    }

    #[test]
    fn scanning_reports_what_is_installed_without_panicking() {
        let all = installed();
        for plugin in &all {
            assert!(!plugin.name.trim().is_empty());
            assert_eq!(plugin.format, "VST3");
            eprintln!("{} — {} [{}]", plugin.name, plugin.vendor, plugin.category);
        }
        eprintln!("{} plugins installed", all.len());
    }

    #[test]
    fn effects_are_a_subset_of_everything_installed() {
        let all = installed();
        let effects = installed_effects();
        assert!(effects.len() <= all.len());
        for effect in &effects {
            assert!(
                all.iter().any(|p| p.name == effect.name),
                "{} is an effect that is not in the full list",
                effect.name
            );
        }
    }
}
