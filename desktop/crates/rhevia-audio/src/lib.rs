//! Audio capture, mixing, metering and DSP.
//!
//! A switcher without audio is not usable for a real show. This is the
//! subsystem that makes Rhevia one.

pub mod capture;
pub mod dsp;
pub mod mixer;

pub use capture::{AudioDevice, CaptureHandle, list_input_devices};
pub use dsp::{
    ChannelDsp, Compressor, CompressorSettings, Delay, EqSettings, Equaliser, GateSettings,
    NoiseGate,
};
pub use mixer::{
    amplitude_to_db, db_to_amplitude, AudioBuffer, AudioMixer, ChannelStrip, Meter, CHANNELS,
    SAMPLE_RATE, SILENCE_DB,
};
