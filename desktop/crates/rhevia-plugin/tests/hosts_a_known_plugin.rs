//! Drives the host against a plugin whose behaviour is known exactly.
//!
//! `rhevia-test-plugin` halves whatever it is given. That makes this a
//! two-way check on the interface declarations: if the host and the plugin
//! agree on every vtable slot and struct offset, the audio comes back at
//! exactly half amplitude. Anything else — silence, garbage, a crash —
//! points straight at a mismatch.
//!
//! The plugins that happen to be installed on a machine cannot do this job.
//! When one of those crashes there is no way to tell a host bug from a plugin
//! bug, which is exactly the ambiguity this removes.

use std::path::PathBuf;

use rhevia_plugin::vst3::{abi, Vst3Instance};

const RATE: f64 = 48_000.0;
const BLOCK: usize = 512;

/// The identifier the fixture plugin declares.
fn fixture_cid() -> abi::Tuid {
    abi::tuid(0x5268_6576, 0x6961_5465, 0x7374_4678, 0x0000_0001)
}

/// Where cargo put the fixture.
///
/// Found relative to this test binary rather than assumed, so it works under
/// both debug and release.
fn fixture_path() -> Option<PathBuf> {
    let mut directory = std::env::current_exe().ok()?;
    // .../target/<profile>/deps/<test binary>
    directory.pop();
    directory.pop();

    let name = if cfg!(windows) {
        "rhevia_test_plugin.dll"
    } else if cfg!(target_os = "macos") {
        "librhevia_test_plugin.dylib"
    } else {
        "librhevia_test_plugin.so"
    };

    let path = directory.join(name);
    path.exists().then_some(path)
}

fn open() -> Option<Vst3Instance> {
    let path = fixture_path()?;
    match Vst3Instance::open(&path, fixture_cid(), "Rhevia Test Gain", RATE, BLOCK) {
        Ok(instance) => Some(instance),
        Err(e) => panic!("the fixture plugin would not open: {e}"),
    }
}

#[test]
fn the_fixture_is_built_before_these_tests_run() {
    // Without it every test below silently skips, which would look like
    // passing.
    assert!(
        fixture_path().is_some(),
        "rhevia_test_plugin was not built — run `cargo build -p rhevia-test-plugin` first, \
         or `cargo test --workspace`, which builds it"
    );
}

#[test]
fn a_known_plugin_can_be_opened_and_reports_its_buses() {
    let Some(instance) = open() else { return };

    assert_eq!(instance.name, "Rhevia Test Gain");
    assert_eq!(instance.max_block, BLOCK);
    assert!(instance.has_input, "the fixture declares an input bus");
    assert_eq!(instance.input_channels, 2, "the host read the wrong input width");
    assert_eq!(instance.output_channels, 2, "the host read the wrong output width");
    assert_eq!(instance.latency_samples(), 0);
}

#[test]
fn audio_comes_back_exactly_halved() {
    // The whole point. Half is a value neither side could produce by
    // accident: silence would mean the buffers never connected, and the
    // original would mean the output was never read back.
    let Some(mut instance) = open() else { return };

    let original: Vec<f32> = (0..BLOCK * 2)
        .map(|n| 0.5 * ((n as f32) * 0.05).sin())
        .collect();
    let mut audio = original.clone();
    instance.process(&mut audio);

    for (index, (got, was)) in audio.iter().zip(original.iter()).enumerate() {
        let expected = was * 0.5;
        assert!(
            (got - expected).abs() < 1e-6,
            "sample {index}: expected {expected}, got {got}"
        );
    }
}

#[test]
fn the_two_channels_do_not_get_crossed() {
    // Interleaving is done twice — in on the way to the plugin, out on the
    // way back — and getting either wrong swaps the channels, which is
    // inaudible on a mono source and obvious on a stereo one.
    let Some(mut instance) = open() else { return };

    let mut audio = vec![0.0f32; BLOCK * 2];
    for frame in 0..BLOCK {
        audio[frame * 2] = 1.0; // left
        audio[frame * 2 + 1] = -1.0; // right
    }
    instance.process(&mut audio);

    for frame in 0..BLOCK {
        assert!(
            (audio[frame * 2] - 0.5).abs() < 1e-6,
            "the left channel came back as {}",
            audio[frame * 2]
        );
        assert!(
            (audio[frame * 2 + 1] + 0.5).abs() < 1e-6,
            "the right channel came back as {}",
            audio[frame * 2 + 1]
        );
    }
}

#[test]
fn a_block_longer_than_the_setup_is_split_into_pieces() {
    // The plugin was told a maximum and refuses anything larger, so if the
    // host failed to divide the work the audio would come back untouched.
    let Some(mut instance) = open() else { return };

    let frames = BLOCK * 3 + 17;
    let mut audio = vec![0.8f32; frames * 2];
    instance.process(&mut audio);

    assert_eq!(audio.len(), frames * 2);
    for (index, sample) in audio.iter().enumerate() {
        assert!(
            (sample - 0.4).abs() < 1e-6,
            "sample {index} came back as {sample}, so a piece was not processed"
        );
    }
}

#[test]
fn a_block_that_is_not_a_whole_number_of_frames_is_handled() {
    let Some(mut instance) = open() else { return };
    // An odd length cannot be a whole number of stereo frames.
    let mut audio = vec![0.4f32; 101];
    instance.process(&mut audio);
    assert_eq!(audio.len(), 101);
    assert!(audio.iter().all(|s| s.is_finite()));
}

#[test]
fn an_empty_block_is_harmless() {
    let Some(mut instance) = open() else { return };
    let mut nothing: Vec<f32> = Vec::new();
    instance.process(&mut nothing);
    assert!(nothing.is_empty());
}

#[test]
fn processing_many_blocks_stays_stable() {
    // A show is hours of this. A reference miscounted once per block, or a
    // buffer pointer refreshed wrongly, shows up over a few thousand.
    let Some(mut instance) = open() else { return };

    for round in 0..2_000 {
        let mut audio = vec![0.25f32; BLOCK * 2];
        instance.process(&mut audio);
        assert!(
            (audio[0] - 0.125).abs() < 1e-6,
            "block {round} came back as {}",
            audio[0]
        );
    }
}

#[test]
fn opening_and_closing_repeatedly_does_not_leak_or_crash() {
    // An operator adding and removing a plugin during a show. A reference
    // released twice takes the process down here.
    for _ in 0..25 {
        let Some(mut instance) = open() else { return };
        let mut audio = vec![0.6f32; BLOCK * 2];
        instance.process(&mut audio);
        assert!((audio[0] - 0.3).abs() < 1e-6);
    }
}

#[test]
fn several_instances_can_run_side_by_side() {
    // Two channel strips with the same plugin on both. Shared state between
    // instances shows up as one of them producing the wrong answer.
    let Some(mut first) = open() else { return };
    let Some(mut second) = open() else { return };

    let mut quiet = vec![0.2f32; BLOCK * 2];
    let mut loud = vec![0.8f32; BLOCK * 2];
    first.process(&mut quiet);
    second.process(&mut loud);

    assert!((quiet[0] - 0.1).abs() < 1e-6);
    assert!((loud[0] - 0.4).abs() < 1e-6);
}

#[test]
fn a_wrong_class_identifier_is_refused() {
    let Some(path) = fixture_path() else { return };
    let result = Vst3Instance::open(&path, [0xAB; 16], "nothing", RATE, BLOCK);
    assert!(result.is_err(), "the fixture made something for an unknown class");
}
