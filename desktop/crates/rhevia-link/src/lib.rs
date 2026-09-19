//! RheviaLink: pairing and ingest for remote cameras.
//!
//! A phone anywhere on the internet becomes a camera input on a desktop
//! anywhere else. See `docs/01-remote-camera.md` for why the transport is
//! WebRTC rather than NDI.

pub mod depacketize;
pub mod protocol;
pub mod receiver;
pub mod signaling;

pub use depacketize::{AccessUnit, H264Depacketizer};
pub use protocol::{ClientInfo, IceServer, PeerInfo, Role, SignalPayload};
pub use receiver::{CameraReceiver, MediaError, MediaEvent};
pub use signaling::{LinkError, LinkEvent, SignalingClient};
