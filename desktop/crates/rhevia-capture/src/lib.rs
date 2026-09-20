//! Live device capture: screen and camera.

pub mod camera;
pub mod screen;

pub use camera::{cameras, CameraCapture, CameraTarget};
pub use screen::{monitors, windows, CaptureError, ScreenCapture, Target};
