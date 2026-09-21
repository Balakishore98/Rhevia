//! Opens a screen capture the way the engine now does, and reports each step.
//!
//! The engine used to open devices on its own thread. It now does it on a
//! worker, and adding a desktop capture took the program down — so this walks
//! the same sequence from a worker thread and prints where it gets to.

use std::time::{Duration, Instant};

fn main() {
    println!("listing monitors on the main thread…");
    match rhevia_capture::monitors() {
        Ok(list) => {
            for m in &list {
                println!("  {} {}x{}", m.name, m.width, m.height);
            }
            if list.is_empty() {
                println!("  none");
                return;
            }
        }
        Err(e) => {
            println!("  failed: {e}");
            return;
        }
    }

    let target = rhevia_capture::monitors().unwrap().remove(0);
    println!("\nopening {:?} from a worker thread, as the engine does…", target.name);

    let handle = std::thread::spawn(move || {
        println!("  worker: listing monitors again (start does this to validate)…");
        match rhevia_capture::monitors() {
            Ok(list) => println!("  worker: sees {} monitors", list.len()),
            Err(e) => println!("  worker: listing failed: {e}"),
        }

        println!("  worker: ScreenCapture::start…");
        match rhevia_capture::ScreenCapture::start(target, 30.0) {
            Ok(capture) => {
                println!("  worker: started");
                capture
            }
            Err(e) => {
                println!("  worker: failed: {e}");
                panic!("could not start");
            }
        }
    });

    let capture = match handle.join() {
        Ok(capture) => capture,
        Err(_) => {
            println!("the worker thread died");
            return;
        }
    };
    println!("moved the capture back to the main thread");

    println!("\ncollecting frames for four seconds…");
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut frames = 0;
    while Instant::now() < deadline {
        if let Some(frame) = capture.take() {
            frames += 1;
            if frames == 1 {
                println!("  first frame {}x{}, {} bytes", frame.width, frame.height, frame.data.len());
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    println!("  {frames} frames");

    println!("\ndropping the capture…");
    drop(capture);
    std::thread::sleep(Duration::from_millis(500));
    println!("still here — nothing crashed");
}
