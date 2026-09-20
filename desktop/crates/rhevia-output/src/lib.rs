//! Stream delivery: FLV muxing and RTMP publishing.
//!
//! This is the output half of the pipeline. It is deliberately independent of
//! how frames were produced, so it can carry passthrough video from a remote
//! camera today and compositor output later without changing.

pub mod aac;
pub mod flv;
pub mod h264;
pub mod mpegts;
pub mod rtmp;
pub mod srt;

pub use aac::{AacEncoder, AacError, AacFrame};
pub use flv::AudioFormat;
pub use h264::ParameterSets;
pub use mpegts::TsMuxer;
pub use rtmp::{RtmpError, RtmpPublisher, RtmpUrl};
pub use srt::{SrtError, SrtMode, SrtPublisher, SrtUrl};
