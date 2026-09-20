//! The middle of the pipeline: decode, composite, encode.
//!
//! Frames arrive encoded, become pictures, get mixed into one picture, and are
//! encoded again for delivery. This is what separates a switcher from a relay.

pub mod codec;
pub mod composite;
pub mod frame;
pub mod source;
pub mod transition;

pub use codec::{CodecError, EncoderSettings, H264Decoder, H264Encoder};
pub use composite::{ColourAdjust, Compositor, Layer, Rect, Scene};
pub use frame::Frame;
pub use source::{load_image, render_title, system_font, SourceError, TitleStyle};
pub use transition::Transition;
