//! Reports what system audio capture is actually receiving.
//!
//! Run it while something is playing to see whether loopback is delivering.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn main() {
    println!("playback devices:");
    for d in rhevia_audio::list_output_devices() {
        println!("  {}{}", d.name, if d.is_default { "   (default)" } else { "" });
    }

    let sink: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let capture = match rhevia_audio::LoopbackCapture::open(None, Arc::clone(&sink)) {
        Ok(c) => c,
        Err(e) => {
            println!("could not open: {e}");
            return;
        }
    };
    println!(
        "\nopened {} at {} Hz, {} ch",
        capture.device_name, capture.source_rate, capture.source_channels
    );

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(8) {
        std::thread::sleep(Duration::from_millis(500));
        let (len, peak) = sink
            .lock()
            .map(|b| (b.len(), b.iter().fold(0.0f32, |m, s| m.max(s.abs()))))
            .unwrap_or((0, 0.0));
        println!(
            "{:.1}s  samples={len}  peak={peak:.5}",
            start.elapsed().as_secs_f32()
        );
    }
}
