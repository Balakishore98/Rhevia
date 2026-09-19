//! The middle of the pipeline: decode, composite, encode.
//!
//! Frames arrive encoded, become pictures, get mixed into one picture, and are
//! encoded again for delivery. This is what separates a switcher from a relay.

pub mod codec;
pub mod composite;
pub mod frame;

pub use codec::{CodecError, EncoderSettings, H264Decoder, H264Encoder};
pub use composite::{Compositor, Layer, Rect, Scene};
pub use frame::Frame;
