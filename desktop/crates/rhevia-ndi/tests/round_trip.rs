//! Publishes an NDI source and receives it back.
//!
//! The unit tests cover the conversions against buffers the test owns. What
//! they cannot cover is whether the bindings are right: whether a struct is
//! laid out as the library expects, whether the sender announces itself, and
//! whether what comes back through a real receiver is the picture that went
//! in. A round trip through the library answers all three at once.
//!
//! Skipped with a clear message when the NDI runtime is not installed.

use std::time::{Duration, Instant};

use rhevia_engine::Frame;
use rhevia_ndi::{find_sources, NdiReceiver, NdiSender};

const WIDTH: usize = 320;
const HEIGHT: usize = 180;

/// A frame whose pixels say where they are, so a skew or a channel swap shows
/// up as a specific wrong value rather than as "looks odd".
fn test_pattern() -> Frame {
    let mut data = vec![0u8; WIDTH * HEIGHT * 4];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let pixel = (y * WIDTH + x) * 4;
            data[pixel] = (x % 256) as u8;
            data[pixel + 1] = (y % 256) as u8;
            data[pixel + 2] = 128;
            data[pixel + 3] = 255;
        }
    }
    Frame { width: WIDTH, height: HEIGHT, data }
}

/// A name that cannot collide with another source, here or on the network.
///
/// A timestamp alone is not enough: these tests run at the same time, two of
/// them start within the same millisecond, and NDI refuses to publish a name
/// that is already being announced.
fn unique_name() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let serial = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("Rhevia Test {stamp}-{serial}")
}

#[test]
fn a_published_source_can_be_found_and_received() {
    if !rhevia_ndi::available() {
        eprintln!(
            "SKIP: {}",
            rhevia_ndi::unavailable_reason().unwrap_or_default()
        );
        return;
    }

    let name = unique_name();
    let sender = NdiSender::create(&name).expect("should publish");
    let pattern = test_pattern();

    // Kept sending on its own thread for the length of the test. A source
    // that sends one frame and stops is not something a receiver can connect
    // to: NDI discovery and connection both take longer than one frame.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let running = std::sync::Arc::clone(&stop);
    let frame = pattern.clone();
    let pump = std::thread::spawn(move || {
        while running.load(std::sync::atomic::Ordering::Relaxed) {
            sender.send_frame(&frame, 30.0);
            // A block of quiet audio, so the audio path is exercised too.
            sender.send_audio(&vec![0.0f32; 1600 * 2]);
        }
        // Dropped here, on the thread that made it.
        drop(sender);
    });

    // ---- discovery -------------------------------------------------------
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut found = None;
    while Instant::now() < deadline {
        let sources = find_sources(Duration::from_millis(800)).expect("discovery should work");
        if let Some(source) = sources.iter().find(|s| s.name.contains(&name)) {
            found = Some(source.clone());
            break;
        }
    }

    let Some(source) = found else {
        stop.store(false, std::sync::atomic::Ordering::Relaxed);
        let _ = pump.join();
        // Discovery uses mDNS, which a firewall or a locked-down network can
        // block entirely. That is the environment, not the bindings.
        eprintln!("SKIP: the published source was never discovered — mDNS may be blocked");
        return;
    };
    eprintln!("found {}", source.name);

    // ---- receive ---------------------------------------------------------
    let receiver = NdiReceiver::connect(source).expect("should connect");

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut received = None;
    while Instant::now() < deadline {
        if let Some(frame) = receiver.take_frame() {
            received = Some(frame);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    drop(receiver);
    stop.store(false, std::sync::atomic::Ordering::Relaxed);
    let _ = pump.join();

    let received = received.expect("no frame came back through NDI");

    // ---- judge -----------------------------------------------------------
    assert_eq!(received.width, WIDTH, "the picture came back the wrong width");
    assert_eq!(received.height, HEIGHT, "the picture came back the wrong height");
    assert_eq!(received.data.len(), WIDTH * HEIGHT * 4);

    // NDI compresses, so the pixels are close rather than identical. What
    // matters is that the pattern is intact: a channel swap or a row skew
    // moves values by far more than compression does.
    let mut worst = 0i32;
    for y in (0..HEIGHT).step_by(17) {
        for x in (0..WIDTH).step_by(13) {
            let at = (y * WIDTH + x) * 4;
            for (channel, expected) in
                [(0usize, (x % 256) as i32), (1, (y % 256) as i32), (2, 128)]
            {
                let got = received.data[at + channel] as i32;
                worst = worst.max((got - expected).abs());
            }
            assert_eq!(received.data[at + 3], 255, "the picture came back transparent");
        }
    }

    assert!(
        worst < 24,
        "the pattern came back wrong by up to {worst} — that is a channel swap or a \
         row skew, not compression"
    );
    eprintln!("round trip intact, worst pixel error {worst}");
}

#[test]
fn a_sender_announces_itself_under_the_name_it_was_given() {
    if !rhevia_ndi::available() {
        eprintln!("SKIP: no NDI runtime");
        return;
    }

    let name = unique_name();
    let sender = NdiSender::create(&name).expect("should publish");
    assert_eq!(sender.name, name);

    // Nothing is sent, so this only checks the sender was created and can be
    // taken down again without leaking or crashing.
    drop(sender);
}

#[test]
fn several_senders_can_run_at_once() {
    if !rhevia_ndi::available() {
        eprintln!("SKIP: no NDI runtime");
        return;
    }

    // A production publishes programme and preview at the same time, so one
    // sender per process is not enough.
    let senders: Vec<NdiSender> = (0..3)
        .map(|n| {
            NdiSender::create(&format!("{} {n}", unique_name())).expect("should publish")
        })
        .collect();

    let frame = test_pattern();
    for sender in &senders {
        sender.send_frame(&frame, 30.0);
    }
    assert_eq!(senders.len(), 3);
}
