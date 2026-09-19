//! The production engine: sources, mixing, transitions and delivery.
//!
//! Runs on its own thread at a fixed frame rate. The UI never touches this
//! state directly — it sends [`Command`]s and reads a snapshot, which is the
//! [command bus](../../../../docs/06-command-bus.md) rule that makes remote
//! control and scripting free later rather than a retrofit.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rhevia_engine::{EncoderSettings, Frame, Layer, Rect, Scene};
use rhevia_output::h264::{self, ParameterSets};
use rhevia_output::{flv, RtmpPublisher, RtmpUrl};
use rhevia_pipeline::Mixer;

/// Everything the UI can ask for. Data only, so it can also arrive from a
/// network peer, a script or a MIDI key without any other change.
#[derive(Debug, Clone)]
pub enum Command {
    SetPreview(usize),
    /// Swap Preview to Program instantly.
    Cut,
    /// Dissolve Preview into Program over the configured duration.
    Auto,
    SetTransitionSeconds(f32),
    SetLayout(Layout),
    /// Assigns a source to one of the four overlay slots.
    SetOverlaySource { slot: usize, input: usize },
    /// Puts an overlay on or off air. Overlays sit above the transition, so
    /// a lower third survives a cut underneath it — which is the whole point.
    ToggleOverlay(usize),
    StartRecording { path: String },
    StopRecording,
    /// Fade to black. The one control that has to work when everything else
    /// has gone wrong, so it bypasses layouts and overlays entirely.
    ToggleFtb,
    /// Put this input straight to air, skipping Preview.
    CutTo(usize),
    AddColourSource { name: String, rgb: [u8; 3] },
    AddBarsSource { name: String },
    AddFileSource { name: String, path: String },
    RemoveSource(usize),
    StartStream { url: String, key: String },
    StopStream,
    Shutdown,
}

/// How Program is arranged. Multi-source layouts are what make a switcher
/// more than a source selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layout {
    /// Program fills the frame.
    #[default]
    Full,
    /// Program full-frame with Preview inset, bottom right.
    Pip,
    /// Program and Preview side by side, both letterboxed.
    SideBySide,
    /// The first four inputs in quadrants — a director's multiview on air.
    Quad,
}

impl Layout {
    pub fn label(self) -> &'static str {
        match self {
            Layout::Full => "FULL",
            Layout::Pip => "PiP",
            Layout::SideBySide => "SPLIT",
            Layout::Quad => "QUAD",
        }
    }

    pub const ALL: [Layout; 4] = [Layout::Full, Layout::Pip, Layout::SideBySide, Layout::Quad];
}

/// Where each overlay slot draws. Slot 4 is full-frame, as on most switchers,
/// so it can carry a full-screen graphic rather than only a corner box.
fn overlay_rect(slot: usize) -> Rect {
    let w = OUTPUT_WIDTH as f32;
    let h = OUTPUT_HEIGHT as f32;
    match slot {
        0 => Rect::new(w * 0.04, h * 0.62, w * 0.30, h * 0.30),
        1 => Rect::new(w * 0.66, h * 0.06, w * 0.30, h * 0.30),
        2 => Rect::new(w * 0.50, h * 0.08, w * 0.46, h * 0.84),
        _ => Rect::full(OUTPUT_WIDTH, OUTPUT_HEIGHT),
    }
}

/// What the UI reads each repaint. Cloned under a lock, so it stays small —
/// frames are the exception and are only copied when they change.
#[derive(Clone, Default)]
pub struct Snapshot {
    pub inputs: Vec<InputInfo>,
    pub program_input: usize,
    pub preview_input: usize,
    pub transition: Option<f32>,
    pub transition_seconds: f32,
    pub program: Option<Frame>,
    pub streaming: bool,
    pub stream_error: Option<String>,
    pub stats: Stats,
    pub layout: Layout,
    /// Which source each overlay slot carries.
    pub overlay_source: [Option<usize>; 4],
    /// Which overlay slots are currently on air.
    pub overlay_on: [bool; 4],
    pub recording: bool,
    pub recorded_bytes: u64,
    pub recording_path: Option<String>,
    pub ftb: bool,
}

#[derive(Clone, Default)]
pub struct InputInfo {
    pub name: String,
    pub thumbnail: Option<Frame>,
}

#[derive(Clone, Copy, Default)]
pub struct Stats {
    pub fps: f32,
    pub frames_rendered: u64,
    pub frames_encoded: u64,
    pub bytes_sent: u64,
    pub uptime_seconds: u64,
    /// Milliseconds spent compositing the last frame. Above the frame budget
    /// means the mixer is the bottleneck.
    pub render_ms: f32,
}

/// Handle the UI holds.
pub struct EngineHandle {
    commands: std::sync::mpsc::Sender<Command>,
    snapshot: Arc<Mutex<Snapshot>>,
    running: Arc<AtomicBool>,
}

impl EngineHandle {
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}

/// A source of pictures.
enum Source {
    Colour([u8; 3]),
    /// Colour bars with a sweeping bar, so it is obvious at a glance that the
    /// picture is live rather than frozen.
    Bars,
    /// Annex-B access units, played in a loop.
    File {
        units: Vec<Vec<u8>>,
        next: usize,
    },
}

struct SourceSlot {
    source: Source,
    mixer_input: usize,
}

const OUTPUT_WIDTH: usize = 1280;
const OUTPUT_HEIGHT: usize = 720;
const TARGET_FPS: f32 = 30.0;
const THUMBNAIL_WIDTH: usize = 320;
const THUMBNAIL_HEIGHT: usize = 180;

/// Starts the engine thread.
pub fn start() -> EngineHandle {
    let (tx, rx) = std::sync::mpsc::channel::<Command>();
    let snapshot = Arc::new(Mutex::new(Snapshot {
        transition_seconds: 1.0,
        ..Default::default()
    }));
    let running = Arc::new(AtomicBool::new(true));

    let shared = Arc::clone(&snapshot);
    let alive = Arc::clone(&running);
    std::thread::Builder::new()
        .name("rhevia-engine".into())
        .spawn(move || {
            if let Err(e) = run(rx, shared, Arc::clone(&alive)) {
                tracing::error!(error = %e, "engine stopped");
            }
            alive.store(false, Ordering::Relaxed);
        })
        .expect("engine thread should start");

    EngineHandle {
        commands: tx,
        snapshot,
        running,
    }
}

struct Delivery {
    publisher: RtmpPublisher,
    runtime: tokio::runtime::Runtime,
    sets: ParameterSets,
    sent_config: bool,
    started: Instant,
    bytes: u64,
}

fn run(
    commands: std::sync::mpsc::Receiver<Command>,
    snapshot: Arc<Mutex<Snapshot>>,
    running: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let settings = EncoderSettings {
        width: OUTPUT_WIDTH,
        height: OUTPUT_HEIGHT,
        bitrate_bps: 4_500_000,
        fps: TARGET_FPS,
        keyframe_interval: (TARGET_FPS as u32) * 2,
    };
    let mut mixer = Mixer::new(settings)?;
    let mut sources: Vec<SourceSlot> = Vec::new();

    // Two sources up front so the window is never an empty grid.
    sources.push(SourceSlot {
        source: Source::Bars,
        mixer_input: 0,
    });
    mixer.set_input_name(0, "Colour Bars");
    sources.push(SourceSlot {
        source: Source::Colour([20, 90, 160]),
        mixer_input: 1,
    });
    mixer.set_input_name(1, "Blue");
    for (rgb, name) in [
        ([150, 30, 60], "Magenta"),
        ([30, 130, 90], "Green"),
        ([200, 140, 40], "Amber"),
    ] {
        if let Ok(input) = mixer.add_input(name) {
            sources.push(SourceSlot {
                source: Source::Colour(rgb),
                mixer_input: input,
            });
        }
    }

    let mut program_input = 0usize;
    let mut preview_input = 1usize;
    let mut transition: Option<f32> = None;
    let mut transition_seconds = 1.0f32;
    let mut delivery: Option<Delivery> = None;
    let mut stream_error: Option<String> = None;
    let mut layout = Layout::Full;
    let mut overlay_source: [Option<usize>; 4] = [None; 4];
    let mut overlay_on = [false; 4];
    let mut ftb = false;
    let mut recorder: Option<(std::io::BufWriter<std::fs::File>, String, u64)> = None;

    let frame_budget = Duration::from_secs_f32(1.0 / TARGET_FPS);
    let start = Instant::now();
    let mut frame_number: u64 = 0;
    let mut fps_window = Instant::now();
    let mut fps_frames = 0u32;
    let mut measured_fps = 0.0f32;

    while running.load(Ordering::Relaxed) {
        let tick = Instant::now();

        // ---- commands ------------------------------------------------------
        while let Ok(command) = commands.try_recv() {
            match command {
                Command::Shutdown => {
                    running.store(false, Ordering::Relaxed);
                    break;
                }
                Command::SetPreview(i) if i < sources.len() => preview_input = i,
                Command::SetPreview(_) => {}
                Command::Cut => {
                    std::mem::swap(&mut program_input, &mut preview_input);
                    transition = None;
                    // A cut is a discontinuity; without a fresh keyframe a
                    // viewer joining now sees the previous shot's residue.
                    mixer.request_keyframe();
                }
                Command::Auto => {
                    if transition.is_none() && program_input != preview_input {
                        transition = Some(0.0);
                    }
                }
                Command::SetTransitionSeconds(s) => transition_seconds = s.clamp(0.1, 10.0),
                Command::SetLayout(l) => layout = l,
                Command::SetOverlaySource { slot, input } => {
                    if slot < 4 && input < sources.len() {
                        overlay_source[slot] = Some(input);
                    }
                }
                Command::ToggleOverlay(slot) => {
                    if slot < 4 && overlay_source[slot].is_some() {
                        overlay_on[slot] = !overlay_on[slot];
                    }
                }
                Command::ToggleFtb => {
                    ftb = !ftb;
                    mixer.request_keyframe();
                }
                Command::CutTo(i) => {
                    if i < sources.len() {
                        preview_input = program_input;
                        program_input = i;
                        transition = None;
                        mixer.request_keyframe();
                    }
                }
                Command::StartRecording { path } => match std::fs::File::create(&path) {
                    Ok(file) => {
                        recorder = Some((std::io::BufWriter::new(file), path, 0));
                        // Recording must begin on a keyframe or the first
                        // seconds of the file are undecodable.
                        mixer.request_keyframe();
                    }
                    Err(e) => stream_error = Some(format!("could not record: {e}")),
                },
                Command::StopRecording => {
                    if let Some((mut w, _, _)) = recorder.take() {
                        use std::io::Write;
                        let _ = w.flush();
                    }
                }
                Command::AddColourSource { name, rgb } => {
                    if let Ok(input) = mixer.add_input(name) {
                        sources.push(SourceSlot {
                            source: Source::Colour(rgb),
                            mixer_input: input,
                        });
                    }
                }
                Command::AddBarsSource { name } => {
                    if let Ok(input) = mixer.add_input(name) {
                        sources.push(SourceSlot {
                            source: Source::Bars,
                            mixer_input: input,
                        });
                    }
                }
                Command::AddFileSource { name, path } => match load_annexb(&path) {
                    Ok(units) if !units.is_empty() => {
                        if let Ok(input) = mixer.add_input(name) {
                            sources.push(SourceSlot {
                                source: Source::File { units, next: 0 },
                                mixer_input: input,
                            });
                        }
                    }
                    Ok(_) => stream_error = Some(format!("{path} contained no H.264 frames")),
                    Err(e) => stream_error = Some(format!("could not open {path}: {e}")),
                },
                Command::RemoveSource(i) => {
                    // Never remove the last source, and never leave Program
                    // pointing at nothing.
                    if sources.len() > 1 && i < sources.len() {
                        sources.remove(i);
                        program_input = program_input.min(sources.len() - 1);
                        preview_input = preview_input.min(sources.len() - 1);
                    }
                }
                Command::StartStream { url, key } => {
                    match open_delivery(&url, &key) {
                        Ok(d) => {
                            delivery = Some(d);
                            stream_error = None;
                            mixer.request_keyframe();
                        }
                        Err(e) => stream_error = Some(e.to_string()),
                    }
                }
                Command::StopStream => {
                    if let Some(mut d) = delivery.take() {
                        d.runtime.block_on(async { d.publisher.close().await.ok() });
                    }
                }
            }
        }
        if !running.load(Ordering::Relaxed) {
            break;
        }

        // ---- sources -------------------------------------------------------
        let seconds = start.elapsed().as_secs_f32();
        for slot in &mut sources {
            match &mut slot.source {
                Source::Colour(rgb) => {
                    let _ = mixer.push_frame(
                        slot.mixer_input,
                        Frame::filled(OUTPUT_WIDTH, OUTPUT_HEIGHT, *rgb),
                    );
                }
                Source::Bars => {
                    let _ = mixer.push_frame(slot.mixer_input, bars(OUTPUT_WIDTH, OUTPUT_HEIGHT, seconds));
                }
                Source::File { units, next } => {
                    if !units.is_empty() {
                        let unit = &units[*next % units.len()];
                        *next = next.wrapping_add(1);
                        let _ = mixer.push_encoded(slot.mixer_input, unit);
                    }
                }
            }
        }

        // ---- scene ---------------------------------------------------------
        let program_slot = sources.get(program_input).map(|s| s.mixer_input).unwrap_or(0);
        let preview_slot = sources.get(preview_input).map(|s| s.mixer_input).unwrap_or(0);

        let mut scene = Scene::new();
        if ftb {
            // Fade to black bypasses everything. When an operator hits this,
            // whatever is wrong must not still be reaching air.
            scene.background = [0, 0, 0];
        } else {
            let w = OUTPUT_WIDTH as f32;
            let h = OUTPUT_HEIGHT as f32;
            match layout {
                Layout::Full => {
                    scene.push(Layer::new(program_slot, Rect::full(OUTPUT_WIDTH, OUTPUT_HEIGHT)));
                }
                Layout::Pip => {
                    scene.push(Layer::new(program_slot, Rect::full(OUTPUT_WIDTH, OUTPUT_HEIGHT)));
                    scene.push(Layer::new(
                        preview_slot,
                        Rect::new(w * 0.66, h * 0.62, w * 0.30, h * 0.30),
                    ));
                }
                Layout::SideBySide => {
                    scene.push(Layer::new(program_slot, Rect::new(0.0, h * 0.25, w * 0.5, h * 0.5)));
                    scene.push(Layer::new(
                        preview_slot,
                        Rect::new(w * 0.5, h * 0.25, w * 0.5, h * 0.5),
                    ));
                }
                Layout::Quad => {
                    for (i, slot) in sources.iter().take(4).enumerate() {
                        let col = (i % 2) as f32;
                        let row = (i / 2) as f32;
                        scene.push(Layer::new(
                            slot.mixer_input,
                            Rect::new(col * w * 0.5, row * h * 0.5, w * 0.5, h * 0.5),
                        ));
                    }
                }
            }

            // The dissolve rides above the layout so it works in every mode.
            if let Some(progress) = transition {
                scene.push(
                    Layer::new(preview_slot, Rect::full(OUTPUT_WIDTH, OUTPUT_HEIGHT))
                        .with_opacity(progress),
                );
            }

            // Overlays sit above the transition, so a lower third stays put
            // while the shot underneath it changes.
            for (slot, on) in overlay_on.iter().enumerate() {
                if !on {
                    continue;
                }
                if let Some(source_index) = overlay_source[slot] {
                    if let Some(source) = sources.get(source_index) {
                        scene.push(Layer::new(source.mixer_input, overlay_rect(slot)));
                    }
                }
            }
        }

        let render_start = Instant::now();
        let needs_encoding = delivery.is_some() || recorder.is_some();
        let encoded = if needs_encoding {
            mixer.render_and_encode(&scene).unwrap_or_default()
        } else {
            mixer.render(&scene);
            Vec::new()
        };
        let render_ms = render_start.elapsed().as_secs_f32() * 1000.0;

        // ---- record --------------------------------------------------------
        // Annex-B straight to disk. Remuxable to MP4 with a stream copy, and
        // readable while still being written, which is what the shorts
        // pipeline will need.
        if let Some((writer, _, bytes)) = recorder.as_mut() {
            if !encoded.is_empty() {
                use std::io::Write;
                if writer.write_all(&encoded).is_err() {
                    stream_error = Some("recording stopped: write failed".into());
                    recorder = None;
                } else {
                    *bytes += encoded.len() as u64;
                }
            }
        }

        // ---- deliver -------------------------------------------------------
        if let Some(d) = delivery.as_mut() {
            if !encoded.is_empty() {
                if let Err(e) = publish(d, &encoded, frame_number) {
                    stream_error = Some(e.to_string());
                    delivery = None;
                }
            }
        }

        // ---- advance the transition ----------------------------------------
        if let Some(progress) = transition {
            let step = frame_budget.as_secs_f32() / transition_seconds.max(0.001);
            let next = progress + step;
            if next >= 1.0 {
                transition = None;
                std::mem::swap(&mut program_input, &mut preview_input);
                mixer.request_keyframe();
            } else {
                transition = Some(next);
            }
        }

        // ---- publish a snapshot for the UI ---------------------------------
        frame_number += 1;
        fps_frames += 1;
        if fps_window.elapsed() >= Duration::from_secs(1) {
            measured_fps = fps_frames as f32 / fps_window.elapsed().as_secs_f32();
            fps_frames = 0;
            fps_window = Instant::now();
        }

        let mixer_stats = mixer.stats();
        let infos: Vec<InputInfo> = sources
            .iter()
            .enumerate()
            .map(|(_, slot)| InputInfo {
                name: mixer
                    .input_name(slot.mixer_input)
                    .unwrap_or("Input")
                    .to_string(),
                thumbnail: mixer.input_frame(slot.mixer_input).map(thumbnail),
            })
            .collect();

        if let Ok(mut s) = snapshot.lock() {
            s.inputs = infos;
            s.program_input = program_input;
            s.preview_input = preview_input;
            s.transition = transition;
            s.transition_seconds = transition_seconds;
            s.program = Some(thumbnail_sized(mixer.program(), 960, 540));
            s.streaming = delivery.is_some();
            s.stream_error = stream_error.clone();
            s.layout = layout;
            s.overlay_source = overlay_source;
            s.overlay_on = overlay_on;
            s.ftb = ftb;
            s.recording = recorder.is_some();
            s.recorded_bytes = recorder.as_ref().map(|(_, _, b)| *b).unwrap_or(0);
            s.recording_path = recorder.as_ref().map(|(_, p, _)| p.clone());
            s.stats = Stats {
                fps: measured_fps,
                frames_rendered: mixer_stats.frames_rendered,
                frames_encoded: mixer_stats.frames_encoded,
                bytes_sent: delivery.as_ref().map(|d| d.bytes).unwrap_or(0),
                uptime_seconds: delivery
                    .as_ref()
                    .map(|d| d.started.elapsed().as_secs())
                    .unwrap_or(0),
                render_ms,
            };
        }

        // Hold the frame rate. Falling behind is reported through render_ms
        // rather than silently accumulating latency.
        if let Some(rest) = frame_budget.checked_sub(tick.elapsed()) {
            std::thread::sleep(rest);
        }
    }

    if let Some(mut d) = delivery.take() {
        d.runtime.block_on(async { d.publisher.close().await.ok() });
    }
    if let Some((mut w, _, _)) = recorder.take() {
        use std::io::Write;
        let _ = w.flush();
    }
    Ok(())
}

fn open_delivery(url: &str, key: &str) -> anyhow::Result<Delivery> {
    let destination = RtmpUrl::parse_with_key(url, Some(key).filter(|k| !k.is_empty()))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let publisher = runtime.block_on(RtmpPublisher::connect(&destination))?;
    Ok(Delivery {
        publisher,
        runtime,
        sets: ParameterSets::default(),
        sent_config: false,
        started: Instant::now(),
        bytes: 0,
    })
}

fn publish(d: &mut Delivery, annexb: &[u8], frame_number: u64) -> anyhow::Result<()> {
    let units = h264::split_annexb(annexb);
    if units.is_empty() {
        return Ok(());
    }
    d.sets.absorb(&units);

    if !d.sent_config {
        let Some(config) = d.sets.to_avc_decoder_config() else {
            return Ok(());
        };
        let tag = flv::avc_sequence_header(&config);
        d.runtime
            .block_on(async { d.publisher.send_video(tag, 0, false).await })?;
        d.sent_config = true;
    }

    let avcc = h264::annexb_to_avcc(&units);
    if avcc.is_empty() {
        return Ok(());
    }
    let keyframe = h264::is_keyframe(&units);
    let timestamp = (frame_number as f32 * 1000.0 / TARGET_FPS) as u32;
    d.bytes += avcc.len() as u64;

    let tag = flv::avc_frame(&avcc, keyframe, 0);
    d.runtime
        .block_on(async { d.publisher.send_video(tag, timestamp, false).await })?;
    Ok(())
}

/// Reads an Annex-B file and splits it into access units.
fn load_annexb(path: &str) -> std::io::Result<Vec<Vec<u8>>> {
    let data = std::fs::read(path)?;
    let units = h264::split_annexb(&data);
    Ok(h264::split_access_units(&units)
        .into_iter()
        .map(|unit| {
            let mut buf = Vec::new();
            for nal in unit {
                buf.extend_from_slice(&[0, 0, 0, 1]);
                buf.extend_from_slice(nal);
            }
            buf
        })
        .collect())
}

fn thumbnail(frame: &Frame) -> Frame {
    thumbnail_sized(frame, THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT)
}

/// Downscales for display. The UI never needs full resolution, and shipping
/// 1280x720 to it thirty times a second would cost more than compositing does.
fn thumbnail_sized(frame: &Frame, width: usize, height: usize) -> Frame {
    if frame.is_empty() {
        return Frame::new(0, 0);
    }
    let mut out = Frame::new(width, height);
    for y in 0..height {
        let v = (y as f32 + 0.5) / height as f32;
        for x in 0..width {
            let u = (x as f32 + 0.5) / width as f32;
            out.set_pixel(x, y, frame.sample(u, v));
        }
    }
    out
}

/// Colour bars with a sweeping highlight, so a frozen picture is obvious.
fn bars(width: usize, height: usize, seconds: f32) -> Frame {
    const COLOURS: [[u8; 3]; 7] = [
        [192, 192, 192],
        [192, 192, 0],
        [0, 192, 192],
        [0, 192, 0],
        [192, 0, 192],
        [192, 0, 0],
        [0, 0, 192],
    ];

    let mut frame = Frame::new(width, height);
    let bar_width = width / COLOURS.len();
    let sweep = ((seconds * 0.25).fract() * width as f32) as usize;

    for y in 0..height {
        for x in 0..width {
            let mut rgb = COLOURS[(x / bar_width.max(1)).min(COLOURS.len() - 1)];
            // Lower quarter is a greyscale ramp, as on a real test card.
            if y > height * 3 / 4 {
                let level = (x * 255 / width.max(1)) as u8;
                rgb = [level, level, level];
            }
            if x.abs_diff(sweep) < 3 {
                rgb = [255, 255, 255];
            }
            frame.set_pixel(x, y, [rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    frame
}
