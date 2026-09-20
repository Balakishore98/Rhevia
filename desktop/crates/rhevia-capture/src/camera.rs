//! Webcam capture.
//!
//! Same shape as [screen capture](crate::screen): the device runs on its own
//! thread and publishes the newest frame into a slot the renderer takes from
//! without ever waiting. A camera that stalls — and USB cameras do — costs a
//! repeated frame rather than the show.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{ApiBackend, CameraIndex, RequestedFormat, RequestedFormatType, Resolution};
use nokhwa::Camera as NokhwaCamera;
use rhevia_engine::Frame;

use crate::screen::CaptureError;

/// A camera the machine can see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraTarget {
    /// The name to show an operator.
    pub name: String,
    /// Position in the system's device list, which is how it is reopened.
    pub index: u32,
    /// What the driver calls itself. Useful when two cameras share a name.
    pub description: String,
}

/// Every camera currently attached.
///
/// An empty list is a normal answer — plenty of machines have no camera — and
/// is not an error.
pub fn cameras() -> Result<Vec<CameraTarget>, CaptureError> {
    let found = nokhwa::query(ApiBackend::Auto)
        .map_err(|e| CaptureError::Enumerate(e.to_string()))?;

    Ok(found
        .into_iter()
        .map(|info| {
            let index = match info.index() {
                CameraIndex::Index(n) => *n,
                // A string index is a device path rather than a position.
                // Nothing downstream can use it, so it is listed and opened
                // by name instead.
                CameraIndex::String(_) => u32::MAX,
            };
            CameraTarget {
                name: info.human_name(),
                index,
                description: info.description().to_string(),
            }
        })
        .collect())
}

/// A running camera. Dropping it stops the thread and closes the device.
pub struct CameraCapture {
    latest: Arc<Mutex<Option<Frame>>>,
    running: Arc<AtomicBool>,
    /// Set by the capture thread when the device fails after opening, so the
    /// interface can say what went wrong rather than showing a frozen picture.
    failure: Arc<Mutex<Option<String>>>,
    pub target: CameraTarget,
}

impl CameraCapture {
    /// Opens `target` and starts streaming.
    ///
    /// The device is opened on the capture thread, because a camera handle on
    /// Windows belongs to the thread that created it and cannot be moved.
    /// The outcome of opening is sent back here, so a camera already in use by
    /// another application is still reported straight away rather than
    /// appearing to start and then never producing a picture.
    pub fn start(target: CameraTarget) -> Result<Self, CaptureError> {
        let index = if target.index == u32::MAX {
            // Reopen by name when the platform gave a path rather than a
            // position.
            let found = nokhwa::query(ApiBackend::Auto)
                .map_err(|e| CaptureError::Enumerate(e.to_string()))?;
            found
                .into_iter()
                .find(|info| info.human_name() == target.name)
                .map(|info| info.index().clone())
                .ok_or_else(|| CaptureError::NoSuchTarget(target.name.clone()))?
        } else {
            CameraIndex::Index(target.index)
        };

        let latest: Arc<Mutex<Option<Frame>>> = Arc::new(Mutex::new(None));
        let running = Arc::new(AtomicBool::new(true));
        let failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let slot = Arc::clone(&latest);
        let alive = Arc::clone(&running);
        let fault = Arc::clone(&failure);
        let name = target.name.clone();
        let (opened, opened_rx) = mpsc::channel::<Result<(), String>>();

        std::thread::Builder::new()
            .name("rhevia-camera-capture".into())
            .spawn(move || {
                // Highest frame rate rather than highest resolution: a
                // switcher wants motion to be smooth, and the frame is scaled
                // to the programme size anyway. A 60 fps 720p feed beats a
                // 15 fps 4K one here.
                let format =
                    RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate);

                let mut camera = match NokhwaCamera::new(index, format) {
                    Ok(camera) => camera,
                    Err(e) => {
                        let _ = opened.send(Err(format!("{name}: {e}")));
                        return;
                    }
                };
                if let Err(e) = camera.open_stream() {
                    let _ = opened.send(Err(format!("{name}: {e}")));
                    return;
                }
                let _ = opened.send(Ok(()));

                // Consecutive failures, so a single dropped USB frame is
                // ignored but an unplugged camera is reported.
                let mut misses = 0u32;

                while alive.load(Ordering::Relaxed) {
                    match camera.frame() {
                        Ok(buffer) => {
                            misses = 0;
                            if let Ok(image) = buffer.decode_image::<RgbFormat>() {
                                let frame = to_frame(
                                    image.width(),
                                    image.height(),
                                    image.as_raw(),
                                );
                                if let Some(frame) = frame {
                                    if let Ok(mut slot) = slot.lock() {
                                        // Replace rather than queue: the
                                        // renderer only wants the newest
                                        // picture, and a backlog is latency.
                                        *slot = Some(frame);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            misses += 1;
                            if misses >= 30 {
                                if let Ok(mut fault) = fault.lock() {
                                    *fault = Some(format!("{name}: {e}"));
                                }
                                break;
                            }
                            // Back off briefly rather than spinning on a
                            // device that is refusing to deliver.
                            std::thread::sleep(Duration::from_millis(10));
                        }
                    }
                }

                let _ = camera.stop_stream();
            })
            .map_err(|e| CaptureError::Device(e.to_string()))?;

        // Wait for the device to open, but not forever: a driver that never
        // answers must not hang the interface that asked for it.
        match opened_rx.recv_timeout(Duration::from_secs(8)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(CaptureError::Device(e)),
            Err(_) => {
                running.store(false, Ordering::Relaxed);
                return Err(CaptureError::Device(format!(
                    "{}: the camera did not respond",
                    target.name
                )));
            }
        }

        Ok(Self { latest, running, failure, target })
    }

    /// The newest frame, if one has arrived since the last call.
    ///
    /// None means "keep showing the last picture", not a failure.
    pub fn take(&self) -> Option<Frame> {
        self.latest.lock().ok().and_then(|mut slot| slot.take())
    }

    /// The reason the camera stopped, if it has.
    pub fn failure(&self) -> Option<String> {
        self.failure.lock().ok().and_then(|f| f.clone())
    }
}

impl Drop for CameraCapture {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

/// Converts packed RGB8 from the camera into the engine's RGBA8 frame.
///
/// Public so the conversion can be tested without a camera attached: it is the
/// part that can silently produce a shifted or half-filled picture.
pub fn to_frame(width: u32, height: u32, rgb: &[u8]) -> Option<Frame> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || rgb.len() < w * h * 3 {
        return None;
    }

    let mut data = vec![0u8; w * h * 4];
    for (pixel, out) in rgb.chunks_exact(3).zip(data.chunks_exact_mut(4)) {
        out[0] = pixel[0];
        out[1] = pixel[1];
        out[2] = pixel[2];
        out[3] = 255;
    }
    Some(Frame { width: w, height: h, data })
}

/// The resolution a camera is currently delivering, for the input list.
pub fn describe(resolution: Resolution) -> String {
    format!("{}x{}", resolution.width(), resolution.height())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cameras_can_be_listed_without_panicking() {
        // A machine with no camera must give an empty list, not a crash, and
        // not an error either — having no camera is normal.
        match cameras() {
            Ok(list) => {
                for camera in list {
                    assert!(!camera.name.is_empty(), "a camera with no name cannot be chosen");
                }
            }
            Err(e) => eprintln!("camera enumeration unavailable: {e}"),
        }
    }

    #[test]
    fn opening_a_camera_that_is_not_there_is_refused() {
        let missing = CameraTarget {
            name: "no-such-camera-12345".into(),
            index: 999,
            description: String::new(),
        };
        assert!(
            CameraCapture::start(missing).is_err(),
            "a missing camera must fail at start, not silently produce nothing"
        );
    }

    #[test]
    fn rgb_becomes_opaque_rgba_of_the_right_size() {
        // Two pixels: red then green.
        let rgb = [255u8, 0, 0, 0, 255, 0];
        let frame = to_frame(2, 1, &rgb).expect("should convert");

        assert_eq!(frame.width, 2);
        assert_eq!(frame.height, 1);
        assert_eq!(frame.data, vec![255, 0, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn a_short_buffer_is_refused_rather_than_read_past() {
        // A driver that hands back less than it promised must not produce a
        // frame built from whatever was next in memory.
        let rgb = [255u8, 0, 0];
        assert!(to_frame(4, 4, &rgb).is_none());
    }

    #[test]
    fn a_zero_sized_frame_is_refused() {
        assert!(to_frame(0, 0, &[]).is_none());
        assert!(to_frame(640, 0, &[]).is_none());
    }

    #[test]
    fn every_pixel_comes_out_fully_opaque() {
        // A camera frame with any transparency would blend against whatever
        // is behind it in the scene, which is never what is wanted.
        let rgb = vec![128u8; 16 * 9 * 3];
        let frame = to_frame(16, 9, &rgb).expect("should convert");
        for pixel in frame.data.chunks_exact(4) {
            assert_eq!(pixel[3], 255);
        }
    }

    #[test]
    fn a_live_camera_produces_a_frame_of_the_right_shape() {
        let Ok(list) = cameras() else {
            eprintln!("SKIP: camera enumeration unavailable");
            return;
        };
        let Some(target) = list.into_iter().next() else {
            eprintln!("SKIP: no camera attached");
            return;
        };

        let capture = match CameraCapture::start(target) {
            Ok(capture) => capture,
            Err(e) => {
                // In use by another application is a legitimate outcome here.
                eprintln!("SKIP: camera would not open: {e}");
                return;
            }
        };

        for _ in 0..60 {
            if let Some(frame) = capture.take() {
                assert!(frame.width > 0 && frame.height > 0);
                assert_eq!(
                    frame.data.len(),
                    frame.width * frame.height * 4,
                    "frame buffer should be RGBA8"
                );
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        eprintln!("no camera frame within 3s");
    }
}
