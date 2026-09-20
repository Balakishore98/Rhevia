//! Opens one VST3 plugin and pushes a block of audio through it.
//!
//! Run as a separate process by the scanner, so that a plugin which crashes
//! takes only this process with it. A plugin that survives here is one the
//! show can use; one that does not is listed as unusable rather than being
//! allowed to bring down a live production.
//!
//! Usage: `rhevia-vst3-validate <module path> <32 hex characters of class id>`
//! Exits 0 when the plugin worked, non-zero otherwise.

use std::path::PathBuf;

use rhevia_plugin::vst3::{abi, Vst3Instance};

fn main() -> std::process::ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.len() != 2 {
        eprintln!("usage: rhevia-vst3-validate <module> <class id in hex>");
        return std::process::ExitCode::from(2);
    }

    let module = PathBuf::from(&arguments[0]);
    let Some(cid) = parse_cid(&arguments[1]) else {
        eprintln!("the class id must be 32 hex characters");
        return std::process::ExitCode::from(2);
    };

    match Vst3Instance::open(&module, cid, "validation", 48_000.0, 512) {
        Ok(mut instance) => {
            // A block of real signal rather than silence: a plugin that
            // divides by its input, or indexes by it, only faults on
            // something non-zero.
            let mut audio: Vec<f32> = (0..512 * 2)
                .map(|n| 0.25 * ((n as f32) * 0.05).sin())
                .collect();
            instance.process(&mut audio);

            if audio.iter().any(|s| !s.is_finite()) {
                eprintln!("the plugin produced values that are not finite");
                return std::process::ExitCode::from(3);
            }
            drop(instance);
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::ExitCode::from(1)
        }
    }
}

/// Reads a class id written as hex.
fn parse_cid(text: &str) -> Option<abi::Tuid> {
    if text.len() != 32 {
        return None;
    }
    let mut cid = [0u8; 16];
    for (index, byte) in cid.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(cid)
}
