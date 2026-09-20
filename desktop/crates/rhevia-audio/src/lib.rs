//! Audio capture, mixing, metering and DSP.
//!
//! A switcher without audio is not usable for a real show. This is the
//! subsystem that makes Rhevia one.

pub mod capture;
pub mod dsp;
pub mod loopback;
pub mod loudness;
pub mod mixer;

pub use capture::{
    list_input_devices, system_audio_endpoint, system_audio_name, AudioDevice, CaptureHandle,
    DeviceKind,
};
pub use dsp::{
    ChannelDsp, Compressor, CompressorSettings, Delay, EqSettings, Equaliser, GateSettings,
    NoiseGate,
};
pub use loopback::{list_output_devices, LoopbackCapture, OutputDevice};
pub use loudness::LoudnessMeter;
pub use mixer::{
    amplitude_to_db, db_to_amplitude, AudioBuffer, AudioMixer, ChannelStrip, Meter, BUS_COUNT,
    BUS_NAMES, CHANNELS, SAMPLE_RATE, SILENCE_DB,
};
