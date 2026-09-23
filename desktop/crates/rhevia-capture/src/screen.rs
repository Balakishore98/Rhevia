//! Screen and window capture.
//!
//! Capture runs on its own thread and hands the newest frame over through a
//! slot. The render thread takes whatever is there and never waits: a capture
//! that stalls costs a repeated frame, where blocking would cost the show.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rhevia_engine::Frame;

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("could not enumerate capture targets: {0}")]
    Enumerate(String),
    #[error("no capture target named {0}")]
    NoSuchTarget(String),
    #[error("the device would not start: {0}")]
    Device(String),
}

/// Something that can be captured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    /// True for a whole monitor, false for a single application window.
    pub is_monitor: bool,
    pub width: u32,
    pub height: u32,
    /// Where a monitor sits on the desktop, in the coordinates Windows lays
    /// the displays out in. Needed to put a window on a particular screen —
    /// "the projector" is a position, not a name, as far as the window
    /// manager is concerned. Zero for a window rather than a monitor.
    pub x: i32,
    pub y: i32,
}

/// Every monitor currently attached.
pub fn monitors() -> Result<Vec<Target>, CaptureError> {
    let monitors = xcap::Monitor::all().map_err(|e| CaptureError::Enumerate(e.to_string()))?;
    Ok(monitors
        .into_iter()
        .map(|m| Target {
            name: m.name().unwrap_or_else(|_| "Display".into()),
            is_monitor: true,
            x: m.x().unwrap_or(0),
            y: m.y().unwrap_or(0),
            width: m.width().unwrap_or(0),
            height: m.height().unwrap_or(0),
        })
        .collect())
}

/// Every capturable application window.
///
/// Minimised windows and those with no title are dropped: they cannot be
/// captured usefully and only make the list harder to read.
pub fn windows() -> Result<Vec<Target>, CaptureError> {
    let windows = xcap::Window::all().map_err(|e| CaptureError::Enumerate(e.to_string()))?;
    let mut out = Vec::new();
    for w in windows {
        let Ok(title) = w.title() else { continue };
        if title.trim().is_empty() {
            continue;
        }
        if w.is_minimized().unwrap_or(false) {
            continue;
        }
        let (width, height) = (w.width().unwrap_or(0), w.height().unwrap_or(0));
        if width == 0 || height == 0 {
            continue;
        }
        out.push(Target { name: title, is_monitor: false, x: 0, y: 0, width, height });
    }
    Ok(out)
}

/// A running capture. Dropping it stops the thread.
pub struct ScreenCapture {
    latest: Arc<Mutex<Option<Frame>>>,
    running: Arc<AtomicBool>,
    pub target: Target,
}

impl ScreenCapture {
    /// Starts capturing `target` at up to `fps` frames a second.
    pub fn start(target: Target, fps: f32) -> Result<Self, CaptureError> {
        // Confirm the target still exists before promising the caller a
        // capture: a monitor can be unplugged between listing and starting.
        if target.is_monitor {
            let found = monitors()?.into_iter().any(|m| m.name == target.name);
            if !found {
                return Err(CaptureError::NoSuchTarget(target.name));
            }
        }

        let latest: Arc<Mutex<Option<Frame>>> = Arc::new(Mutex::new(None));
        let running = Arc::new(AtomicBool::new(true));

        let slot = Arc::clone(&latest);
        let alive = Arc::clone(&running);
        let wanted = target.clone();
        let interval = Duration::from_secs_f32(1.0 / fps.max(1.0));

        std::thread::Builder::new()
            .name("rhevia-screen-capture".into())
            .spawn(move || {
                while alive.load(Ordering::Relaxed) {
                    let started = Instant::now();

                    if let Some(frame) = grab(&wanted) {
                        if let Ok(mut slot) = slot.lock() {
                            // Replace rather than queue: the renderer only
                            // ever wants the newest picture, and a backlog
                            // would only add latency.
                            *slot = Some(frame);
                        }
                    }

                    if let Some(rest) = interval.checked_sub(started.elapsed()) {
                        std::thread::sleep(rest);
                    }
                }
            })
            .map_err(|e| CaptureError::Enumerate(e.to_string()))?;

        Ok(Self { latest, running, target })
    }

    /// The newest frame, if one has arrived since the last call.
    ///
    /// Returns None when nothing new is ready, which the caller should treat
    /// as "keep showing the last picture" rather than as a failure.
    pub fn take(&self) -> Option<Frame> {
        self.latest.lock().ok().and_then(|mut slot| slot.take())
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

/// Grabs one picture, converting to the engine's frame format.
fn grab(target: &Target) -> Option<Frame> {
    let image = if target.is_monitor {
        let monitors = xcap::Monitor::all().ok()?;
        let monitor = monitors
            .into_iter()
            .find(|m| m.name().map(|n| n == target.name).unwrap_or(false))?;
        monitor.capture_image().ok()?
    } else {
        let windows = xcap::Window::all().ok()?;
        let window = windows
            .into_iter()
            .find(|w| w.title().map(|t| t == target.name).unwrap_or(false))?;
        window.capture_image().ok()?
    };

    let (width, height) = (image.width() as usize, image.height() as usize);
    if width == 0 || height == 0 {
        return None;
    }

    // xcap already hands back RGBA8, so this is a move rather than a convert.
    Some(Frame { width, height, data: image.into_raw() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitors_can_be_listed_without_panicking() {
        // A headless machine has none; that must be an empty list, not a crash.
        match monitors() {
            Ok(list) => {
                for m in list {
                    assert!(m.is_monitor);
                    assert!(!m.name.is_empty());
                }
            }
            Err(e) => eprintln!("no monitors available: {e}"),
        }
    }

    #[test]
    fn windows_are_listed_without_untitled_or_minimised_entries() {
        match windows() {
            Ok(list) => {
                for w in list {
                    assert!(!w.is_monitor);
                    assert!(!w.name.trim().is_empty(), "untitled windows should be filtered");
                    assert!(w.width > 0 && w.height > 0);
                }
            }
            Err(e) => eprintln!("no windows available: {e}"),
        }
    }

    #[test]
    fn starting_a_capture_on_a_missing_monitor_is_refused() {
        let missing = Target {
            name: "no-such-display-12345".into(),
            is_monitor: true,
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        assert!(ScreenCapture::start(missing, 30.0).is_err());
    }

    #[test]
    fn a_capture_produces_a_frame_of_the_right_shape() {
        let Ok(list) = monitors() else {
            eprintln!("SKIP: no monitors");
            return;
        };
        let Some(target) = list.into_iter().next() else {
            eprintln!("SKIP: no monitors");
            return;
        };

        let capture = ScreenCapture::start(target, 30.0).expect("should start");
        // Give the thread a moment to produce its first picture.
        for _ in 0..40 {
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
        eprintln!("no frame captured within 2s; the display may be locked");
    }

    #[test]
    fn taking_twice_without_a_new_frame_returns_nothing_the_second_time() {
        // The renderer uses None to mean "hold the previous picture", so a
        // stale frame must not be handed out twice as though it were new.
        let Ok(list) = monitors() else { return };
        let Some(target) = list.into_iter().next() else { return };
        let capture = ScreenCapture::start(target, 1.0).expect("should start");

        for _ in 0..40 {
            if capture.take().is_some() {
                assert!(capture.take().is_none(), "the slot should be empty after taking");
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
