//! RheviaLink: pairing and ingest for remote cameras.
//!
//! A phone anywhere on the internet becomes a camera input on a desktop
//! anywhere else. See `docs/01-remote-camera.md` for why the transport is
//! WebRTC rather than NDI.

pub mod protocol;
pub mod signaling;

pub use protocol::{ClientInfo, IceServer, PeerInfo, Role, SignalPayload};
pub use signaling::{LinkError, LinkEvent, SignalingClient};
