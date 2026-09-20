//! The production engine: sources, mixing, transitions and delivery.
//!
//! Runs on its own thread at a fixed frame rate. The UI never touches this
//! state directly — it sends [`Command`]s and reads a snapshot, which is the
//! [command bus](../../../../docs/06-command-bus.md) rule that makes remote
//! control and scripting free later rather than a retrofit.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rhevia_audio::{AudioBuffer, AudioMixer, CaptureHandle, SAMPLE_RATE};
use rhevia_engine::{
    ColourAdjust, EncoderSettings, Frame, Layer, Rect, Scene, TitleStyle, Transition,
};
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
    /// Duration in milliseconds, which is how every switcher labels it.
    SetTransitionMs(f32),
    /// Chooses the effect the AUTO button and the T-bar use.
    SetTransition(Transition),
    SetEq { channel: usize, settings: rhevia_audio::EqSettings },
    SetCompressor { channel: usize, settings: rhevia_audio::CompressorSettings },
    SetGate { channel: usize, settings: rhevia_audio::GateSettings },
    SetAudioDelay { channel: usize, ms: f32 },
    /// Adds a plugin to the end of a channel's chain.
    ///
    /// Only plugins that have survived validation in another process should
    /// reach here: one that faults takes the show with it.
    AddPlugin { channel: usize, path: String, cid: String, name: String },
    RemovePlugin { channel: usize, index: usize },
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
    /// An audio-only input from a capture device.
    AddAudioSource { name: String, device: Option<String> },
    /// A monitor or an application window, captured live.
    AddScreenSource { name: String, target: rhevia_capture::Target },
    /// A webcam or capture card the system exposes as a camera.
    AddCameraSource { name: String, target: rhevia_capture::CameraTarget },
    /// Any media file — video or audio, any container, any codec ffmpeg reads.
    /// This is what a dropped file becomes.
    AddMediaSource { name: String, path: String },
    /// A source another machine is publishing over NDI.
    AddNdiSource { name: String, source: rhevia_ndi::NdiSource },
    /// Publishes the programme as an NDI source other machines can take.
    StartNdiOutput { name: String },
    StopNdiOutput,
    /// Attaches a capture device to an existing input, so a camera carries
    /// its own sound.
    AttachAudio { input: usize, device: Option<String> },
    SetChannelGain { channel: usize, db: f32 },
    ToggleMute(usize),
    ToggleSolo(usize),
    SetPan { channel: usize, pan: f32 },
    ToggleFollowProgram(usize),
    /// Routes a channel to a bus, or stops routing it there.
    SetChannelBus { channel: usize, bus: usize, on: bool },
    SetMasterGain(f32),
    ToggleMasterMute,
    ClearClip(usize),
    AddBarsSource { name: String },
    AddFileSource { name: String, path: String },
    /// A still image: holding slide, sponsor board, stinger graphic.
    AddImageSource { name: String, path: String },
    /// A lower third, rendered here rather than in another application.
    AddTitleSource { name: String, text: String, subtitle: String },
    /// Re-renders an existing title without rebuilding the input.
    SetTitleText { input: usize, text: String, subtitle: String },
    RenameInput { input: usize, name: String },
    /// Zoom and pan within the frame, as vMix offers under Position.
    SetInputTransform { input: usize, zoom: f32, offset_x: f32, offset_y: f32 },
    SetInputColour { input: usize, colour: ColourAdjust },
    ResetInputSettings(usize),
    RemoveSource(usize),
    StartStream { url: String, key: String },
    /// Stops every destination.
    StopStream,
    /// Stops one destination, leaving the others live.
    StopDestination(usize),
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

/// Adjusts an index that referred to a list from which `removed` was taken.
///
/// Returns None when the index referred to the removed item itself. Every
/// stored input index has to go through here: clamping instead means an
/// overlay or the programme quietly starts pointing at a different source.
pub fn remap_after_removal(removed: usize, index: usize) -> Option<usize> {
    match index.cmp(&removed) {
        std::cmp::Ordering::Less => Some(index),
        std::cmp::Ordering::Equal => None,
        std::cmp::Ordering::Greater => Some(index - 1),
    }
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

/// Per-input picture settings.
///
/// Held beside the source rather than inside it, so they survive whatever the
/// source is and apply identically to a camera, a still or a title.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputSettings {
    /// 1.0 fits the frame; above that crops in.
    pub zoom: f32,
    /// Pan, as a fraction of the frame. Only meaningful once zoomed in.
    pub offset_x: f32,
    pub offset_y: f32,
    pub colour: ColourAdjust,
}

impl Default for InputSettings {
    fn default() -> Self {
        Self { zoom: 1.0, offset_x: 0.0, offset_y: 0.0, colour: ColourAdjust::default() }
    }
}

impl InputSettings {
    pub fn is_default(&self) -> bool {
        (self.zoom - 1.0).abs() < 0.001
            && self.offset_x.abs() < 0.001
            && self.offset_y.abs() < 0.001
            && self.colour.is_neutral()
    }

    /// Applies zoom and pan to the rectangle a layer would otherwise occupy.
    ///
    /// Zoom scales about the centre so the shot grows into the frame rather
    /// than out of one corner, which is what an operator expects when they
    /// push in on a wide.
    pub fn apply(&self, rect: Rect) -> Rect {
        let zoom = self.zoom.clamp(0.1, 8.0);
        let width = rect.width * zoom;
        let height = rect.height * zoom;
        Rect::new(
            rect.x - (width - rect.width) / 2.0 + self.offset_x * rect.width,
            rect.y - (height - rect.height) / 2.0 + self.offset_y * rect.height,
            width,
            height,
        )
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
    /// Every destination currently being fed, in the order they were added.
    pub destinations: Vec<DestinationState>,
    /// The name the programme is being published under over NDI, if it is.
    pub ndi_output: Option<String>,
    pub recording: bool,
    pub recorded_bytes: u64,
    pub recording_path: Option<String>,
    pub ftb: bool,
    pub transition_kind: Transition,
    pub audio: Vec<ChannelState>,
    /// Peak level and clip state for each bus, for the matrix header.
    pub bus_levels: Vec<(f32, bool)>,
    pub master: MasterState,
}

/// One live destination, as the streaming panel needs it.
#[derive(Clone, Default)]
pub struct DestinationState {
    /// The address with any passphrase or key removed — this is displayed,
    /// and a stream key in a screenshot is a stream key given away.
    pub address: String,
    /// RTMP or SRT.
    pub protocol: String,
    pub bytes_sent: u64,
    pub uptime_seconds: u64,
}

/// One audio channel strip, as the mixer panel needs it.
#[derive(Clone, Default)]
pub struct ChannelState {
    pub name: String,
    pub gain_db: f32,
    pub muted: bool,
    pub solo: bool,
    pub pan: f32,
    pub follow_program: bool,
    pub peak_db: f32,
    pub rms_db: f32,
    pub clipped: bool,
    /// False when the input has no audio device attached.
    pub has_source: bool,
    pub eq: rhevia_audio::EqSettings,
    pub compressor: rhevia_audio::CompressorSettings,
    pub gate: rhevia_audio::GateSettings,
    pub delay_ms: f32,
    /// Current compressor gain reduction, for the meter.
    pub gain_reduction_db: f32,
    /// Whether the gate is currently passing audio.
    pub gate_open: bool,
    /// Which buses this channel feeds. Index 0 is Master.
    pub buses: [bool; rhevia_audio::BUS_COUNT],
    /// Plugins on this channel, in the order they run.
    pub plugins: Vec<String>,
}

#[derive(Clone, Copy, Default)]
pub struct MasterState {
    pub gain_db: f32,
    pub muted: bool,
    pub peak_db: f32,
    pub rms_db: f32,
    pub clipped: bool,
    /// Loudness to ITU-R BS.1770, which is what a platform measures the
    /// programme against. Peak tells you about clipping; this tells you
    /// whether the show will arrive at the level everything else is at.
    pub momentary_lufs: f32,
    pub short_term_lufs: f32,
    pub integrated_lufs: f32,
}

#[derive(Clone, Default)]
pub struct InputInfo {
    pub name: String,
    pub thumbnail: Option<Frame>,
    /// Current text, when this input is a title. Lets the UI offer an edit
    /// without keeping its own copy of what the engine holds.
    pub title: Option<(String, String)>,
    pub settings: InputSettings,
    /// What kind of source this is, for the settings dialog and the filters.
    pub kind: &'static str,
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
    /// Sound with no picture. Draws its own level so the tile is not a black
    /// rectangle the operator cannot read.
    AudioOnly,
    /// A still. Held as a decoded frame, so it costs nothing per tick.
    Still(Frame),
    /// A title. Re-rendered only when its text changes.
    Title { style: TitleStyle, rendered: Frame },
    /// A monitor or window. The capture runs on its own thread and this
    /// only collects whatever it has most recently produced.
    Screen(rhevia_capture::ScreenCapture),
    /// A camera, collected the same way.
    Camera(rhevia_capture::CameraCapture),
    /// A media file, played on a loop. Carries its own sound, so it feeds the
    /// mixer directly rather than through a capture device.
    Media(Box<rhevia_media::MediaSource>),
    /// A source from another machine on the network, with its sound.
    Ndi(Box<rhevia_ndi::NdiReceiver>),
}

struct SourceSlot {
    source: Source,
    mixer_input: usize,
    /// Audio for this input, when a device is attached.
    audio: Option<CaptureHandle>,
    settings: InputSettings,
    /// Set when a still source needs handing to the mixer again.
    ///
    /// A colour, a still or a title never changes between ticks, so pushing it
    /// every frame meant allocating and filling a full-size frame thirty times
    /// a second for a picture nobody had touched.
    needs_push: bool,
}

const OUTPUT_WIDTH: usize = 1280;
const OUTPUT_HEIGHT: usize = 720;
const TARGET_FPS: f32 = 30.0;
const THUMBNAIL_WIDTH: usize = 320;
const THUMBNAIL_HEIGHT: usize = 180;
/// The program picture sent to the UI. Smaller than the canvas because it is
/// displayed scaled anyway, and every pixel here is paid for on the render
/// thread.
const PREVIEW_WIDTH: usize = 640;
const PREVIEW_HEIGHT: usize = 360;
/// Thumbnails are regenerated every Nth frame. The eye cannot read a
/// multiview faster than this, and doing it every frame cost more than the
/// compositing it was illustrating.
const THUMBNAIL_EVERY: u64 = 3;

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

/// Where the stream is going.
///
/// The encoding is identical either way; only the muxing and the socket
/// differ. RTMP is what the large platforms ingest, SRT is what survives a
/// path that loses packets — so a real production wants both available, not
/// one or the other.
enum Transport {
    Rtmp(RtmpPublisher),
    Srt(rhevia_output::SrtPublisher),
}

impl Transport {
    fn label(&self) -> &'static str {
        match self {
            Transport::Rtmp(_) => "RTMP",
            Transport::Srt(_) => "SRT",
        }
    }
}

struct Delivery {
    transport: Transport,
    /// The destination as it is safe to show: no stream key, no passphrase.
    address: String,
    runtime: tokio::runtime::Runtime,
    sets: ParameterSets,
    sent_config: bool,
    started: Instant,
    bytes: u64,
    /// The stream carries sound as AAC, which is what every platform wants.
    audio: Option<rhevia_output::AacEncoder>,
    sent_audio_config: bool,
    /// Samples sent so far, which is what audio timestamps are derived from.
    /// Counting samples rather than reading a clock keeps audio and video
    /// locked together however the frame rate actually behaves.
    audio_samples: u64,
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
    let mut audio = AudioMixer::new();
    // Fed from the master bus every tick, so the integrated figure covers the
    // whole session rather than only the part that was streamed.
    let mut loudness = rhevia_audio::LoudnessMeter::new();
    // Publishes the programme for other machines on the network. Independent
    // of streaming: a production often sends NDI to a recorder or a second
    // switcher while streaming to a platform.
    let mut ndi_output: Option<rhevia_ndi::NdiSender> = None;
    let mut sources: Vec<SourceSlot> = Vec::new();

    // Two sources up front so the window is never an empty grid.
    sources.push(SourceSlot {
        source: Source::Bars,
        mixer_input: 0,
        audio: None,
        settings: InputSettings::default(),
        needs_push: true,
    });
    mixer.set_input_name(0, "Colour Bars");
    audio.add_channel("Colour Bars");
    sources.push(SourceSlot {
        source: Source::Colour([20, 90, 160]),
        mixer_input: 1,
        audio: None,
        settings: InputSettings::default(),
        needs_push: true,
    });
    mixer.set_input_name(1, "Blue");
    audio.add_channel("Blue");
    for (rgb, name) in [
        ([150, 30, 60], "Magenta"),
        ([30, 130, 90], "Green"),
        ([200, 140, 40], "Amber"),
    ] {
        if let Ok(input) = mixer.add_input(name) {
            sources.push(SourceSlot {
                source: Source::Colour(rgb),
                mixer_input: input,
                audio: None,
                settings: InputSettings::default(),
        needs_push: true,
            });
            audio.add_channel(name);
        }
    }

    let mut program_input = 0usize;
    let mut preview_input = 1usize;
    let mut transition: Option<f32> = None;
    let mut transition_seconds = 1.0f32;
    // A list rather than one: a real production sends to a platform and to
    // an archive or a backup path at the same time. The encoding is done once
    // and fanned out, so a second destination costs a socket, not a second
    // encoder.
    let mut deliveries: Vec<Delivery> = Vec::new();
    let mut stream_error: Option<String> = None;
    let mut layout = Layout::Full;
    let mut overlay_source: [Option<usize>; 4] = [None; 4];
    let mut overlay_on = [false; 4];
    let mut ftb = false;
    let mut transition_kind = Transition::Fade;
    // Loaded once. A title that cannot find a face is reported rather than
    // silently rendering nothing.
    let font = rhevia_engine::system_font().ok();
    // One DSP chain per channel, index-aligned with the audio mixer.
    let mut dsp: Vec<rhevia_audio::ChannelDsp> = Vec::new();
    // One chain of plugins per channel, run after the built-in processing so
    // that a plugin sees audio the way the operator has already shaped it.
    let mut plugins: Vec<Vec<rhevia_plugin::vst3::Vst3Instance>> = Vec::new();
    let mut recorder: Option<(std::io::BufWriter<std::fs::File>, String, u64)> = None;

    let frame_budget = Duration::from_secs_f32(1.0 / TARGET_FPS);
    let start = Instant::now();
    let mut frame_number: u64 = 0;
    let mut fps_window = Instant::now();
    let mut fps_frames = 0u32;
    let mut measured_fps = 0.0f32;
    // Held between regenerations so the UI still has pictures on the frames
    // where none are made.
    let mut cached_inputs: Vec<InputInfo> = Vec::new();
    let mut cached_program: Option<Frame> = None;

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
                        if transition_kind.is_instant() {
                            std::mem::swap(&mut program_input, &mut preview_input);
                            mixer.request_keyframe();
                        } else {
                            transition = Some(0.0);
                        }
                    }
                }
                Command::SetTransitionSeconds(s) => transition_seconds = s.clamp(0.1, 10.0),
                Command::SetTransitionMs(ms) => transition_seconds = (ms / 1000.0).clamp(0.1, 10.0),
                Command::SetTransition(kind) => transition_kind = kind,
                Command::SetEq { channel, settings } => {
                    if let Some(chain) = dsp.get_mut(channel) {
                        chain.eq.set(settings);
                    }
                }
                Command::SetCompressor { channel, settings } => {
                    if let Some(chain) = dsp.get_mut(channel) {
                        chain.compressor.set(settings);
                    }
                }
                Command::SetGate { channel, settings } => {
                    if let Some(chain) = dsp.get_mut(channel) {
                        chain.gate.set(settings);
                    }
                }
                Command::AddPlugin { channel, path, cid, name } => {
                    match rhevia_plugin::parse_cid(&cid) {
                        Some(cid) => {
                            match rhevia_plugin::vst3::Vst3Instance::open(
                                std::path::Path::new(&path),
                                cid,
                                &name,
                                SAMPLE_RATE as f64,
                                PLUGIN_BLOCK,
                            ) {
                                Ok(instance) => {
                                    if let Some(chain) = plugins.get_mut(channel) {
                                        chain.push(instance);
                                    }
                                }
                                Err(e) => stream_error = Some(format!("{name}: {e}")),
                            }
                        }
                        None => stream_error = Some(format!("{name}: bad plugin identifier")),
                    }
                }
                Command::RemovePlugin { channel, index } => {
                    if let Some(chain) = plugins.get_mut(channel) {
                        if index < chain.len() {
                            chain.remove(index);
                        }
                    }
                }
                Command::SetAudioDelay { channel, ms } => {
                    if let Some(chain) = dsp.get_mut(channel) {
                        chain.delay.set_milliseconds(ms);
                    }
                }
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
                    if let Ok(input) = mixer.add_input(name.clone()) {
                        sources.push(SourceSlot {
                            source: Source::Colour(rgb),
                            mixer_input: input,
                            audio: None,
                            settings: InputSettings::default(),
        needs_push: true,
                        });
                        audio.add_channel(name);
                    }
                }
                Command::AddNdiSource { name, source } => {
                    match rhevia_ndi::NdiReceiver::connect(source) {
                        Ok(receiver) => {
                            if let Ok(input) = mixer.add_input(name.clone()) {
                                sources.push(SourceSlot {
                                    source: Source::Ndi(Box::new(receiver)),
                                    mixer_input: input,
                                    audio: None,
                                    settings: InputSettings::default(),
                                    needs_push: true,
                                });
                                audio.add_channel(name);
                            }
                        }
                        Err(e) => stream_error = Some(e.to_string()),
                    }
                }
                Command::StartNdiOutput { name } => {
                    match rhevia_ndi::NdiSender::create(&name) {
                        Ok(sender) => {
                            ndi_output = Some(sender);
                            stream_error = None;
                        }
                        Err(e) => stream_error = Some(e.to_string()),
                    }
                }
                Command::StopNdiOutput => ndi_output = None,
                Command::AddMediaSource { name, path } => {
                    match rhevia_media::MediaSource::open(
                        &path,
                        OUTPUT_WIDTH as u32,
                        OUTPUT_HEIGHT as u32,
                        TARGET_FPS,
                    ) {
                        Ok(media) => {
                            if let Ok(input) = mixer.add_input(name.clone()) {
                                sources.push(SourceSlot {
                                    source: Source::Media(Box::new(media)),
                                    mixer_input: input,
                                    audio: None,
                                    settings: InputSettings::default(),
                                    needs_push: true,
                                });
                                audio.add_channel(name);
                            }
                        }
                        Err(e) => stream_error = Some(e.to_string()),
                    }
                }
                Command::AddCameraSource { name, target } => {
                    match rhevia_capture::CameraCapture::start(target) {
                        Ok(capture) => {
                            if let Ok(input) = mixer.add_input(name.clone()) {
                                sources.push(SourceSlot {
                                    source: Source::Camera(capture),
                                    mixer_input: input,
                                    audio: None,
                                    settings: InputSettings::default(),
                                    needs_push: true,
                                });
                                audio.add_channel(name);
                            }
                        }
                        Err(e) => stream_error = Some(format!("camera: {e}")),
                    }
                }
                Command::AddScreenSource { name, target } => {
                    match rhevia_capture::ScreenCapture::start(target, TARGET_FPS) {
                        Ok(capture) => {
                            if let Ok(input) = mixer.add_input(name.clone()) {
                                sources.push(SourceSlot {
                                    source: Source::Screen(capture),
                                    mixer_input: input,
                                    audio: None,
                                    settings: InputSettings::default(),
                                    needs_push: true,
                                });
                                audio.add_channel(name);
                            }
                        }
                        Err(e) => stream_error = Some(format!("screen capture: {e}")),
                    }
                }
                Command::AddAudioSource { name, device } => {
                    match CaptureHandle::open(device.as_deref()) {
                        Ok(handle) => {
                            if let Ok(input) = mixer.add_input(name.clone()) {
                                sources.push(SourceSlot {
                                    source: Source::AudioOnly,
                                    mixer_input: input,
                                    audio: Some(handle),
                                    settings: InputSettings::default(),
        needs_push: true,
                                });
                                let channel = audio.add_channel(name);
                                // Sound with no picture is almost always a
                                // microphone, and a microphone must stay live
                                // when the camera it sits next to goes off air.
                                if let Some(strip) = audio.channel_mut(channel) {
                                    strip.follow_program = false;
                                }
                            }
                        }
                        Err(e) => stream_error = Some(format!("audio device: {e}")),
                    }
                }
                Command::AttachAudio { input, device } => {
                    match CaptureHandle::open(device.as_deref()) {
                        Ok(handle) => {
                            if let Some(slot) = sources.get_mut(input) {
                                slot.audio = Some(handle);
                                // Sound belonging to a camera should come up
                                // with that camera, so this follows Program.
                                if let Some(strip) = audio.channel_mut(input) {
                                    strip.follow_program = true;
                                }
                            }
                        }
                        Err(e) => stream_error = Some(format!("audio device: {e}")),
                    }
                }
                Command::SetChannelGain { channel, db } => {
                    if let Some(strip) = audio.channel_mut(channel) {
                        strip.gain_db = db.clamp(rhevia_audio::SILENCE_DB, 12.0);
                    }
                }
                Command::ToggleMute(channel) => {
                    if let Some(strip) = audio.channel_mut(channel) {
                        strip.muted = !strip.muted;
                    }
                }
                Command::ToggleSolo(channel) => {
                    if let Some(strip) = audio.channel_mut(channel) {
                        strip.solo = !strip.solo;
                    }
                }
                Command::SetPan { channel, pan } => {
                    if let Some(strip) = audio.channel_mut(channel) {
                        strip.pan = pan.clamp(-1.0, 1.0);
                    }
                }
                Command::ToggleFollowProgram(channel) => {
                    if let Some(strip) = audio.channel_mut(channel) {
                        strip.follow_program = !strip.follow_program;
                    }
                }
                Command::SetChannelBus { channel, bus, on } => {
                    if let Some(strip) = audio.channel_mut(channel) {
                        strip.set_bus(bus, on);
                    }
                }
                Command::SetMasterGain(db) => {
                    audio.master_gain_db = db.clamp(rhevia_audio::SILENCE_DB, 12.0);
                }
                Command::ToggleMasterMute => audio.master_muted = !audio.master_muted,
                Command::ClearClip(channel) => {
                    // usize::MAX addresses the master, so one command serves
                    // every meter rather than needing a separate message.
                    if channel == usize::MAX {
                        audio.master_meter.clear_clip();
                    } else if let Some(strip) = audio.channel_mut(channel) {
                        strip.meter.clear_clip();
                    }
                }
                Command::AddBarsSource { name } => {
                    if let Ok(input) = mixer.add_input(name.clone()) {
                        sources.push(SourceSlot {
                            source: Source::Bars,
                            mixer_input: input,
                            audio: None,
                            settings: InputSettings::default(),
        needs_push: true,
                        });
                        audio.add_channel(name);
                    }
                }
                Command::AddImageSource { name, path } => {
                    match rhevia_engine::load_image(&path) {
                        Ok(frame) => {
                            if let Ok(input) = mixer.add_input(name.clone()) {
                                sources.push(SourceSlot {
                                    source: Source::Still(frame),
                                    mixer_input: input,
                                    audio: None,
                                    settings: InputSettings::default(),
        needs_push: true,
                                });
                                audio.add_channel(name);
                            }
                        }
                        Err(e) => stream_error = Some(e.to_string()),
                    }
                }
                Command::AddTitleSource { name, text, subtitle } => match font.as_ref() {
                    Some(font) => {
                        let style = TitleStyle { text, subtitle, ..Default::default() };
                        let rendered =
                            rhevia_engine::render_title(font, &style, OUTPUT_WIDTH, OUTPUT_HEIGHT);
                        if let Ok(input) = mixer.add_input(name.clone()) {
                            sources.push(SourceSlot {
                                source: Source::Title { style, rendered },
                                mixer_input: input,
                                audio: None,
                                settings: InputSettings::default(),
        needs_push: true,
                            });
                            audio.add_channel(name);
                        }
                    }
                    None => {
                        stream_error = Some("no usable font found on this system".into());
                    }
                },
                Command::RenameInput { input, name } => {
                    if let Some(slot) = sources.get(input) {
                        mixer.set_input_name(slot.mixer_input, name.clone());
                        if let Some(strip) = audio.channel_mut(input) {
                            strip.name = name;
                        }
                    }
                }
                Command::SetInputTransform { input, zoom, offset_x, offset_y } => {
                    if let Some(slot) = sources.get_mut(input) {
                        slot.settings.zoom = zoom.clamp(0.1, 8.0);
                        slot.settings.offset_x = offset_x.clamp(-1.0, 1.0);
                        slot.settings.offset_y = offset_y.clamp(-1.0, 1.0);
                    }
                }
                Command::SetInputColour { input, colour } => {
                    if let Some(slot) = sources.get_mut(input) {
                        slot.settings.colour = colour;
                    }
                }
                Command::ResetInputSettings(input) => {
                    if let Some(slot) = sources.get_mut(input) {
                        slot.settings = InputSettings::default();
                    }
                }
                Command::SetTitleText { input, text, subtitle } => {
                    if let (Some(slot), Some(font)) = (sources.get_mut(input), font.as_ref()) {
                        if let Source::Title { style, rendered } = &mut slot.source {
                            style.text = text;
                            style.subtitle = subtitle;
                            // Re-rendered here rather than every tick: a title
                            // changes when someone types, not thirty times a
                            // second.
                            *rendered = rhevia_engine::render_title(
                                font,
                                style,
                                OUTPUT_WIDTH,
                                OUTPUT_HEIGHT,
                            );
                            slot.needs_push = true;
                        }
                    }
                }
                Command::AddFileSource { name, path } => match load_annexb(&path) {
                    Ok(units) if !units.is_empty() => {
                        if let Ok(input) = mixer.add_input(name.clone()) {
                            sources.push(SourceSlot {
                                source: Source::File { units, next: 0 },
                                mixer_input: input,
                                audio: None,
                                settings: InputSettings::default(),
        needs_push: true,
                            });
                            audio.add_channel(name);
                        }
                    }
                    Ok(_) => stream_error = Some(format!("{path} contained no H.264 frames")),
                    Err(e) => stream_error = Some(format!("could not open {path}: {e}")),
                },
                Command::RemoveSource(i) => {
                    // Never remove the last source, and never leave Program
                    // pointing at nothing.
                    if sources.len() > 1 && i < sources.len() {
                        // Free the mixer input too, then shift every stored
                        // mixer index above it down to match. Without this the
                        // decoder and its held frame stay alive for the rest
                        // of the session.
                        let freed = sources[i].mixer_input;
                        mixer.remove_input(freed);
                        sources.remove(i);
                        for slot in sources.iter_mut() {
                            if slot.mixer_input > freed {
                                slot.mixer_input -= 1;
                            }
                        }
                        audio.remove_channel(i);
                        // The DSP chains are index-aligned with the channels,
                        // so the chain has to go with its channel. Truncating
                        // by length instead shifted every setting onto the
                        // wrong source.
                        if i < dsp.len() {
                            dsp.remove(i);
                            if i < plugins.len() {
                                plugins.remove(i);
                            }
                        }

                        // Everything else that holds an input index has to be
                        // remapped, or it silently addresses a different
                        // source than the one it was pointed at.
                        let last = sources.len() - 1;
                        program_input = remap_after_removal(i, program_input).unwrap_or(0).min(last);
                        preview_input = remap_after_removal(i, preview_input).unwrap_or(0).min(last);
                        for slot in 0..4 {
                            match overlay_source[slot] {
                                Some(source) => {
                                    overlay_source[slot] = remap_after_removal(i, source);
                                    // An overlay whose source is gone must
                                    // come off air rather than start showing
                                    // whatever moved into that index.
                                    if overlay_source[slot].is_none() {
                                        overlay_on[slot] = false;
                                    }
                                }
                                None => overlay_on[slot] = false,
                            }
                        }
                    }
                }
                Command::StartStream { url, key } => {
                    match open_delivery(&url, &key) {
                        Ok(d) => {
                            deliveries.push(d);
                            stream_error = None;
                            // A destination joining mid-show has nothing to
                            // decode until a keyframe arrives, so ask for one
                            // rather than making it wait for the next.
                            mixer.request_keyframe();
                        }
                        Err(e) => stream_error = Some(e.to_string()),
                    }
                }
                Command::StopStream => {
                    for mut d in deliveries.drain(..) {
                        close_delivery(&mut d);
                    }
                }
                Command::StopDestination(index) => {
                    if index < deliveries.len() {
                        let mut d = deliveries.remove(index);
                        close_delivery(&mut d);
                    }
                }
            }
        }
        if !running.load(Ordering::Relaxed) {
            break;
        }

        // ---- audio ---------------------------------------------------------
        // One video frame worth of audio per tick. Driving audio off the video
        // clock keeps them locked together by construction; a separate audio
        // clock would drift apart over a long show.
        let audio_frames = (SAMPLE_RATE as f32 / TARGET_FPS) as usize;
        // One DSP chain per channel. Grown here rather than at every add
        // site, so a chain can never be missing for a channel that exists.
        while dsp.len() < audio.channels.len() {
            dsp.push(rhevia_audio::ChannelDsp::new());
            plugins.push(Vec::new());
        }
        dsp.truncate(audio.channels.len());

        let mut captured: Vec<Option<AudioBuffer>> = sources
            .iter()
            .map(|slot| match &slot.source {
                // A media file brings its own sound. Taking it here, in step
                // with the video, is what keeps the two together.
                Source::Media(media) if media.info.has_audio => {
                    Some(AudioBuffer::from_samples(media.take_audio(audio_frames)))
                }
                // An NDI source carries its own sound, in step with its
                // picture, so it is collected here rather than from a device.
                Source::Ndi(receiver) => {
                    Some(AudioBuffer::from_samples(receiver.take_audio(audio_frames)))
                }
                _ => slot.audio.as_ref().map(|handle| handle.take(audio_frames)),
            })
            .collect();

        // Processed before the mixer, so the fader and meters see the audio
        // after EQ and dynamics -- which is what an operator expects when they
        // set a compressor and the meter stops slamming.
        for (index, buffer) in captured.iter_mut().enumerate() {
            if let (Some(buffer), Some(chain)) = (buffer.as_mut(), dsp.get_mut(index)) {
                chain.process(buffer);
            }
            // Plugins run last on the channel, in the order they were added,
            // so each one sees what the one before it produced.
            if let (Some(buffer), Some(chain)) = (buffer.as_mut(), plugins.get_mut(index)) {
                for instance in chain.iter_mut() {
                    instance.process(&mut buffer.samples);
                }
            }
        }
        let on_air: Vec<bool> = (0..sources.len()).map(|i| i == program_input).collect();
        {
            let refs: Vec<Option<&AudioBuffer>> = captured.iter().map(|c| c.as_ref()).collect();
            audio.mix(&refs, &on_air, audio_frames);
        }
        // Measured post-fader, on exactly what goes out, which is the only
        // point where the figure means anything.
        loudness.measure(audio.output());

        // ---- sources -------------------------------------------------------
        let seconds = start.elapsed().as_secs_f32();
        for slot in &mut sources {
            match &mut slot.source {
                Source::Colour(rgb) => {
                    if slot.needs_push {
                        let _ = mixer.push_frame(
                            slot.mixer_input,
                            Frame::filled(OUTPUT_WIDTH, OUTPUT_HEIGHT, *rgb),
                        );
                        slot.needs_push = false;
                    }
                }
                Source::Bars => {
                    let _ = mixer.push_frame(slot.mixer_input, bars(OUTPUT_WIDTH, OUTPUT_HEIGHT, seconds));
                }
                Source::Still(frame) => {
                    if slot.needs_push {
                        let _ = mixer.push_frame(slot.mixer_input, frame.clone());
                        slot.needs_push = false;
                    }
                }
                Source::Title { rendered, .. } => {
                    if slot.needs_push {
                        let _ = mixer.push_frame(slot.mixer_input, rendered.clone());
                        slot.needs_push = false;
                    }
                }
                Source::Ndi(receiver) => {
                    // None means nothing new has arrived, so the previous
                    // picture stays up rather than flashing black.
                    if let Some(frame) = receiver.take_frame() {
                        let _ = mixer.push_frame(slot.mixer_input, frame);
                    }
                }
                Source::Media(media) => {
                    // None means the decoder has produced nothing new, so the
                    // previous picture stays up rather than flashing black.
                    if let Some(frame) = media.take_frame() {
                        let _ = mixer.push_frame(slot.mixer_input, frame);
                    }
                }
                Source::Camera(capture) => {
                    // None means the capture thread has produced nothing new,
                    // so the previous picture stays up rather than flashing
                    // black between grabs.
                    if let Some(frame) = capture.take() {
                        let _ = mixer.push_frame(slot.mixer_input, frame);
                    }
                    // A camera that has been unplugged mid-show has to say so:
                    // silently holding the last picture would let a dead feed
                    // sit on air unnoticed.
                    if let Some(reason) = capture.failure() {
                        if stream_error.is_none() {
                            stream_error = Some(format!("camera: {reason}"));
                        }
                    }
                }
                Source::Screen(capture) => {
                    // None means the capture thread has produced nothing new,
                    // so the previous picture stays up rather than flashing
                    // black between grabs.
                    if let Some(frame) = capture.take() {
                        let _ = mixer.push_frame(slot.mixer_input, frame);
                    }
                }
                Source::AudioOnly => {
                    let level = audio
                        .channel(slot.mixer_input)
                        .map(|c| c.meter.peak())
                        .unwrap_or(0.0);
                    let _ = mixer.push_frame(
                        slot.mixer_input,
                        audio_tile(OUTPUT_WIDTH, OUTPUT_HEIGHT, level),
                    );
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

        // A scene is built for a given "program" input, so the same code can
        // produce both sides of a transition.
        // Builds a layer for a mixer input with that input's settings applied.
        // Going through here means zoom, pan and colour work in every layout
        // and on overlays, rather than only on the full-screen case.
        let layer_for = |mixer_input: usize, rect: Rect| -> Layer {
            let settings = sources
                .iter()
                .find(|s| s.mixer_input == mixer_input)
                .map(|s| s.settings)
                .unwrap_or_default();
            Layer {
                colour: settings.colour,
                ..Layer::new(mixer_input, settings.apply(rect))
            }
        };

        let build_scene = |program: usize, preview: usize| -> Scene {
            let mut scene = Scene::new();
            let w = OUTPUT_WIDTH as f32;
            let h = OUTPUT_HEIGHT as f32;
            match layout {
                Layout::Full => {
                    scene.push(layer_for(program, Rect::full(OUTPUT_WIDTH, OUTPUT_HEIGHT)));
                }
                Layout::Pip => {
                    scene.push(layer_for(program, Rect::full(OUTPUT_WIDTH, OUTPUT_HEIGHT)));
                    scene.push(layer_for(
                        preview,
                        Rect::new(w * 0.66, h * 0.62, w * 0.30, h * 0.30),
                    ));
                }
                Layout::SideBySide => {
                    scene.push(layer_for(program, Rect::new(0.0, h * 0.25, w * 0.5, h * 0.5)));
                    scene.push(layer_for(preview, Rect::new(w * 0.5, h * 0.25, w * 0.5, h * 0.5)));
                }
                Layout::Quad => {
                    for (i, slot) in sources.iter().take(4).enumerate() {
                        let col = (i % 2) as f32;
                        let row = (i / 2) as f32;
                        scene.push(layer_for(
                            slot.mixer_input,
                            Rect::new(col * w * 0.5, row * h * 0.5, w * 0.5, h * 0.5),
                        ));
                    }
                }
            }

            // Overlays sit above the transition, so a lower third stays put
            // while the shot underneath it changes. Present in both sides of a
            // transition, which makes them hold still through the blend.
            for (slot, on) in overlay_on.iter().enumerate() {
                if !on {
                    continue;
                }
                if let Some(source_index) = overlay_source[slot] {
                    if let Some(source) = sources.get(source_index) {
                        scene.push(layer_for(source.mixer_input, overlay_rect(slot)));
                    }
                }
            }
            scene
        };

        // FTB bypasses layout, overlays and transitions alike. When an
        // operator reaches for it, nothing else may still reach air.
        let program_scene = if ftb {
            let mut black = Scene::new();
            black.background = [0, 0, 0];
            black
        } else {
            build_scene(program_slot, preview_slot)
        };

        let render_start = Instant::now();
        let needs_encoding = !deliveries.is_empty() || recorder.is_some();
        let encoded = match transition {
            // Mid-transition: composite both arrangements in full and blend
            // them with the chosen effect, so a transition works between any
            // two layouts rather than only between two sources.
            Some(progress) if !ftb => {
                let incoming = build_scene(preview_slot, program_slot);
                // A stinger covers the screen with a designated source. The
                // overlay slots already hold "a source chosen for a purpose",
                // so Stinger N uses overlay N rather than inventing a second
                // assignment the operator has to remember.
                let stinger_input = transition_kind
                    .stinger_slot()
                    .and_then(|slot| overlay_source[slot])
                    .and_then(|index| sources.get(index))
                    .map(|slot| slot.mixer_input);
                if needs_encoding {
                    mixer
                        .render_transition_and_encode(
                            &program_scene,
                            &incoming,
                            transition_kind,
                            progress,
                            stinger_input,
                        )
                        .unwrap_or_default()
                } else {
                    mixer.render_transition(
                        &program_scene,
                        &incoming,
                        transition_kind,
                        progress,
                        stinger_input,
                    );
                    Vec::new()
                }
            }
            _ => {
                if needs_encoding {
                    mixer.render_and_encode(&program_scene).unwrap_or_default()
                } else {
                    mixer.render(&program_scene);
                    Vec::new()
                }
            }
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

        // ---- publish over the network --------------------------------------
        // Sent before encoding, from the composited picture, so an NDI
        // receiver gets the full-quality frame rather than one that has been
        // through H.264.
        if let Some(sender) = &ndi_output {
            if let Some(frame) = cached_program.as_ref() {
                sender.send_frame(frame, TARGET_FPS);
                sender.send_audio(&audio.output().samples);
            }
        }

        // ---- deliver -------------------------------------------------------
        // Audio goes out every tick whether or not the encoder emitted a
        // picture, so a frame the video encoder chose to skip does not take a
        // block of sound with it.
        let master = if deliveries.is_empty() { Vec::new() } else { audio.output().samples.clone() };

        // One destination failing takes only itself off air. Dropping the
        // whole stream because a backup path died would be the opposite of
        // what having a backup is for.
        let mut failed: Vec<usize> = Vec::new();
        for (index, d) in deliveries.iter_mut().enumerate() {
            if !encoded.is_empty() {
                if let Err(e) = publish(d, &encoded, frame_number) {
                    stream_error = Some(format!("{}: {e}", d.address));
                    failed.push(index);
                    continue;
                }
            }
            if let Err(e) = publish_audio(d, &master) {
                stream_error = Some(format!("{} audio: {e}", d.address));
                failed.push(index);
            }
        }
        for index in failed.into_iter().rev() {
            deliveries.remove(index);
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

        // Names and title text are cheap and must stay current; only the
        // pictures are throttled.
        let redraw_thumbnails =
            frame_number % THUMBNAIL_EVERY == 0 || cached_inputs.len() != sources.len();
        let infos: Vec<InputInfo> = sources
            .iter()
            .enumerate()
            .map(|(index, slot)| InputInfo {
                name: mixer
                    .input_name(slot.mixer_input)
                    .unwrap_or("Input")
                    .to_string(),
                thumbnail: if redraw_thumbnails {
                    mixer.input_frame(slot.mixer_input).map(thumbnail)
                } else {
                    cached_inputs.get(index).and_then(|i| i.thumbnail.clone())
                },
                title: match &slot.source {
                    Source::Title { style, .. } => {
                        Some((style.text.clone(), style.subtitle.clone()))
                    }
                    _ => None,
                },
                settings: slot.settings,
                kind: match &slot.source {
                    Source::Colour(_) => "Colour",
                    Source::Bars => "Colour Bars",
                    Source::File { .. } => "Video File",
                    Source::AudioOnly => "Audio Input",
                    Source::Still(_) => "Image",
                    Source::Title { .. } => "Title",
                    Source::Screen(c) => {
                        if c.target.is_monitor { "Desktop Capture" } else { "Window Capture" }
                    }
                    Source::Camera(_) => "Camera",
                    Source::Media(m) => {
                        if m.info.has_video { "Media" } else { "Audio File" }
                    }
                    Source::Ndi(_) => "NDI",
                },
            })
            .collect();
        if redraw_thumbnails {
            cached_inputs = infos.clone();
            cached_program = Some(downscale(mixer.program(), PREVIEW_WIDTH, PREVIEW_HEIGHT));
        }

        if let Ok(mut s) = snapshot.lock() {
            s.inputs = infos;
            s.program_input = program_input;
            s.preview_input = preview_input;
            s.transition = transition;
            s.transition_seconds = transition_seconds;
            s.program = cached_program.clone();
            s.streaming = !deliveries.is_empty();
            s.ndi_output = ndi_output.as_ref().map(|s| s.name.clone());
            s.destinations = deliveries
                .iter()
                .map(|d| DestinationState {
                    address: d.address.clone(),
                    protocol: d.transport.label().to_string(),
                    bytes_sent: d.bytes,
                    uptime_seconds: d.started.elapsed().as_secs(),
                })
                .collect();
            s.stream_error = stream_error.clone();
            s.layout = layout;
            s.overlay_source = overlay_source;
            s.overlay_on = overlay_on;
            s.ftb = ftb;
            s.transition_kind = transition_kind;
            s.recording = recorder.is_some();
            s.recorded_bytes = recorder.as_ref().map(|(_, _, b)| *b).unwrap_or(0);
            s.recording_path = recorder.as_ref().map(|(_, p, _)| p.clone());
            s.audio = audio
                .channels
                .iter()
                .enumerate()
                .map(|(i, c)| ChannelState {
                    name: c.name.clone(),
                    gain_db: c.gain_db,
                    muted: c.muted,
                    solo: c.solo,
                    pan: c.pan,
                    follow_program: c.follow_program,
                    peak_db: c.meter.peak_db(),
                    rms_db: c.meter.rms_db(),
                    clipped: c.meter.clipped(),
                    has_source: sources.get(i).map(|s| s.audio.is_some()).unwrap_or(false),
                    eq: dsp.get(i).map(|d| d.eq.settings()).unwrap_or_default(),
                    compressor: dsp.get(i).map(|d| d.compressor.settings()).unwrap_or_default(),
                    gate: dsp.get(i).map(|d| d.gate.settings()).unwrap_or_default(),
                    delay_ms: dsp.get(i).map(|d| d.delay.milliseconds()).unwrap_or(0.0),
                    gain_reduction_db: dsp
                        .get(i)
                        .map(|d| d.compressor.gain_reduction_db())
                        .unwrap_or(0.0),
                    gate_open: dsp.get(i).map(|d| d.gate.is_open()).unwrap_or(true),
                    buses: c.buses,
                    plugins: plugins
                        .get(i)
                        .map(|chain| chain.iter().map(|p| p.name.clone()).collect())
                        .unwrap_or_default(),
                })
                .collect();
            s.bus_levels = audio
                .bus_meters
                .iter()
                .map(|m| (m.peak_db(), m.clipped()))
                .collect();
            s.master = MasterState {
                gain_db: audio.master_gain_db,
                muted: audio.master_muted,
                peak_db: audio.master_meter.peak_db(),
                rms_db: audio.master_meter.rms_db(),
                clipped: audio.master_meter.clipped(),
                momentary_lufs: loudness.momentary_lufs() as f32,
                short_term_lufs: loudness.short_term_lufs() as f32,
                integrated_lufs: loudness.integrated_lufs() as f32,
            };
            s.stats = Stats {
                fps: measured_fps,
                frames_rendered: mixer_stats.frames_rendered,
                frames_encoded: mixer_stats.frames_encoded,
                // Across every destination, which is what the bitrate
                // readout in the header is describing.
                bytes_sent: deliveries.iter().map(|d| d.bytes).sum(),
                uptime_seconds: deliveries
                    .iter()
                    .map(|d| d.started.elapsed().as_secs())
                    .max()
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

    for mut d in deliveries.drain(..) {
        close_delivery(&mut d);
    }
    if let Some((mut w, _, _)) = recorder.take() {
        use std::io::Write;
        let _ = w.flush();
    }
    Ok(())
}

/// The largest block a plugin is told to expect.
///
/// Audio arrives one video frame at a time — 1600 samples at 30 fps — and a
/// plugin told a smaller maximum would have the work split needlessly.
const PLUGIN_BLOCK: usize = 2048;

/// How long to wait for a destination to answer.
///
/// This runs on the engine thread, so it stops the picture while it waits.
/// A bounded wait costs the operator a few seconds; an unbounded one would
/// look exactly like a crash.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// True when the address names an SRT destination rather than an RTMP one.
pub fn is_srt(url: &str) -> bool {
    url.trim().to_ascii_lowercase().starts_with("srt://")
}

/// The RTMP address with any stream key stripped off the end.
///
/// Platforms present the key either as a separate field or as the last path
/// segment, and a key that reached the interface this way would end up in
/// every screenshot of the streaming panel.
pub fn redact_rtmp(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    // Everything up to and including the application name is safe: it is the
    // ingest endpoint, which is public. Anything beyond it might be the key.
    match trimmed.rsplit_once('/') {
        Some((head, tail)) if head.matches('/').count() >= 3 && !tail.is_empty() => {
            format!("{head}/***")
        }
        _ => trimmed.to_string(),
    }
}

fn open_delivery(url: &str, key: &str) -> anyhow::Result<Delivery> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let address;

    let transport = if is_srt(url) {
        // A stream key has no meaning in SRT; the equivalent is the stream id,
        // which rides in the address. Rather than ignore a key the operator
        // typed, it is used as the stream id when the address has none.
        let mut destination = rhevia_output::SrtUrl::parse(url.trim())?;
        if destination.stream_id.is_none() && !key.trim().is_empty() {
            destination.stream_id = Some(key.trim().to_string());
        }

        let publisher = runtime.block_on(async {
            tokio::time::timeout(
                CONNECT_TIMEOUT,
                rhevia_output::SrtPublisher::connect(&destination, true),
            )
            .await
            .map_err(|_| {
                anyhow::anyhow!("{} did not answer within ten seconds", destination.redacted())
            })?
            .map_err(anyhow::Error::from)
        })?;
        address = destination.redacted();
        Transport::Srt(publisher)
    } else {
        let destination = RtmpUrl::parse_with_key(url, Some(key).filter(|k| !k.is_empty()))?;
        address = redact_rtmp(url);
        let publisher = runtime.block_on(RtmpPublisher::connect(&destination))?;
        Transport::Rtmp(publisher)
    };

    // A failed audio encoder must not stop the stream: video only is far
    // better than nothing, and the operator is told in the status line.
    let audio = match rhevia_output::AacEncoder::new(SAMPLE_RATE, 2, 128_000) {
        Ok(encoder) => Some(encoder),
        Err(e) => {
            tracing::warn!(error = %e, "streaming without audio");
            None
        }
    };

    tracing::info!(transport = transport.label(), "streaming");

    Ok(Delivery {
        transport,
        address,
        runtime,
        sets: ParameterSets::default(),
        sent_config: false,
        started: Instant::now(),
        bytes: 0,
        audio,
        sent_audio_config: false,
        audio_samples: 0,
    })
}

/// Ends the stream cleanly.
///
/// Closed rather than dropped: a receiver told the stream has ended finishes
/// its file, where a dropped socket leaves a truncated one that looks exactly
/// like a fault.
fn close_delivery(d: &mut Delivery) {
    d.runtime.block_on(async {
        match &mut d.transport {
            Transport::Rtmp(publisher) => {
                publisher.close().await.ok();
            }
            Transport::Srt(publisher) => {
                publisher.close().await.ok();
            }
        }
    });
}

/// Encodes a block of master audio and publishes it.
fn publish_audio(d: &mut Delivery, samples: &[f32]) -> anyhow::Result<()> {
    let Some(encoder) = d.audio.as_mut() else {
        return Ok(());
    };

    // The AudioSpecificConfig has to precede any audio on RTMP, exactly as
    // the video decoder configuration precedes any frame. A transport stream
    // needs no such thing: each ADTS frame describes itself.
    if !d.sent_audio_config {
        if let Transport::Rtmp(publisher) = &mut d.transport {
            let tag = flv::aac_sequence_header(encoder.config(), Default::default());
            d.runtime.block_on(async { publisher.send_audio(tag, 0).await })?;
        }
        d.sent_audio_config = true;
    }

    let frames = encoder.push(samples)?;
    for frame in frames {
        let samples_before = d.audio_samples;
        d.audio_samples += frame.samples as u64;
        d.bytes += frame.data.len() as u64;

        match &mut d.transport {
            Transport::Rtmp(publisher) => {
                let timestamp = (samples_before * 1000 / SAMPLE_RATE as u64) as u32;
                let tag = flv::aac_frame(&frame.data, Default::default());
                d.runtime.block_on(async { publisher.send_audio(tag, timestamp).await })?;
            }
            Transport::Srt(publisher) => {
                // Transport stream runs at 90 kHz, not milliseconds.
                let timestamp =
                    samples_before * rhevia_output::mpegts::CLOCK_HZ / SAMPLE_RATE as u64;
                let adts = rhevia_output::mpegts::adts_wrap(&frame.data, SAMPLE_RATE, 2);
                d.runtime.block_on(async { publisher.send_audio(&adts, timestamp).await })?;
            }
        }
    }
    Ok(())
}

fn publish(d: &mut Delivery, annexb: &[u8], frame_number: u64) -> anyhow::Result<()> {
    let units = h264::split_annexb(annexb);
    if units.is_empty() {
        return Ok(());
    }
    d.sets.absorb(&units);
    let keyframe = h264::is_keyframe(&units);

    match &mut d.transport {
        Transport::Rtmp(publisher) => {
            // FLV states the parameter sets once, up front, in their own tag.
            if !d.sent_config {
                let Some(config) = d.sets.to_avc_decoder_config() else {
                    return Ok(());
                };
                let tag = flv::avc_sequence_header(&config);
                d.runtime.block_on(async { publisher.send_video(tag, 0, false).await })?;
                d.sent_config = true;
            }

            let avcc = h264::annexb_to_avcc(&units);
            if avcc.is_empty() {
                return Ok(());
            }
            let timestamp = (frame_number as f32 * 1000.0 / TARGET_FPS) as u32;
            d.bytes += avcc.len() as u64;

            let tag = flv::avc_frame(&avcc, keyframe, 0);
            d.runtime
                .block_on(async { publisher.send_video(tag, timestamp, false).await })?;
        }
        Transport::Srt(publisher) => {
            // A transport stream carries the parameter sets inline, repeated
            // before every keyframe, so a player joining part-way through can
            // start. There is no separate configuration step.
            let prepared = rhevia_output::mpegts::prepare_video(&units, &d.sets);
            if prepared.is_empty() {
                return Ok(());
            }
            let timestamp = frame_number * rhevia_output::mpegts::CLOCK_HZ
                / TARGET_FPS.max(1.0) as u64;
            d.bytes += prepared.len() as u64;
            d.sent_config = true;

            d.runtime.block_on(async {
                publisher.send_video(&prepared, timestamp, timestamp, keyframe).await
            })?;
        }
    }
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
    downscale(frame, THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT)
}

/// Downscales for display, by direct row addressing rather than bilinear
/// sampling.
///
/// The bilinear path costs roughly twenty operations per output pixel, and at
/// one program preview plus every input thumbnail that came to more work per
/// frame than compositing the programme itself. Point sampling is visually
/// fine at thumbnail size and about an order of magnitude cheaper.
fn downscale(frame: &Frame, width: usize, height: usize) -> Frame {
    if frame.is_empty() || width == 0 || height == 0 {
        return Frame::new(0, 0);
    }
    let mut out = Frame::new(width, height);

    // Precomputed source columns: the inner loop then does one copy per pixel
    // with no arithmetic at all.
    let columns: Vec<usize> = (0..width)
        .map(|x| (x * frame.width / width).min(frame.width - 1))
        .collect();

    for y in 0..height {
        let source_row = (y * frame.height / height).min(frame.height - 1);
        let source_base = source_row * frame.width * 4;
        let dest_base = y * width * 4;
        for (x, &column) in columns.iter().enumerate() {
            let s = source_base + column * 4;
            let d = dest_base + x * 4;
            out.data[d..d + 4].copy_from_slice(&frame.data[s..s + 4]);
        }
    }
    out
}

/// A picture for an audio-only input: a level bar on a dark field.
///
/// vMix leaves these tiles black, which tells the operator nothing. Showing
/// the level means a dead microphone is visible in the multiview at a glance
/// rather than only in the mixer panel.
fn audio_tile(width: usize, height: usize, level: f32) -> Frame {
    let mut frame = Frame::filled(width, height, [18, 20, 26]);

    let bar_height = height / 6;
    let top = height / 2 - bar_height / 2;
    let filled = (width as f32 * level.clamp(0.0, 1.0)) as usize;

    for y in top..(top + bar_height).min(height) {
        for x in 0..width {
            // Green up to -12 dBFS, amber to -3, red above: the same reading
            // as the meters in the mixer panel.
            let fraction = x as f32 / width.max(1) as f32;
            let colour = if x >= filled {
                [32, 35, 42]
            } else if fraction > 0.85 {
                [226, 62, 62]
            } else if fraction > 0.65 {
                [232, 168, 56]
            } else {
                [64, 196, 118]
            };
            frame.set_pixel(x, y, [colour[0], colour[1], colour[2], 255]);
        }
    }
    frame
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

    // Two rows are built once and then copied, rather than writing every pixel
    // through a bounds-checked setter: the picture only has two distinct row
    // shapes, and the sweep is patched in afterwards.
    let mut bar_row = vec![0u8; width * 4];
    let mut ramp_row = vec![0u8; width * 4];
    for x in 0..width {
        let rgb = COLOURS[(x / bar_width.max(1)).min(COLOURS.len() - 1)];
        let level = (x * 255 / width.max(1)) as u8;
        bar_row[x * 4..x * 4 + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        ramp_row[x * 4..x * 4 + 4].copy_from_slice(&[level, level, level, 255]);
    }

    let ramp_from = height * 3 / 4;
    for y in 0..height {
        let source = if y > ramp_from { &ramp_row } else { &bar_row };
        let base = y * width * 4;
        frame.data[base..base + width * 4].copy_from_slice(source);
    }

    // The sweep proves the picture is live rather than frozen.
    for y in 0..height {
        let base = y * width * 4;
        for x in sweep.saturating_sub(2)..(sweep + 3).min(width) {
            frame.data[base + x * 4..base + x * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
    }
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream key is a password. It must not reach the streaming panel,
    /// because the streaming panel is what ends up in screenshots and in
    /// screen shares.
    mod redaction {
        use super::*;

        #[test]
        fn a_key_on_the_end_of_the_url_is_hidden() {
            let shown = redact_rtmp("rtmp://a.rtmp.youtube.com/live2/abcd-efgh-ijkl-mnop");
            assert!(!shown.contains("abcd"), "the key leaked: {shown}");
            assert_eq!(shown, "rtmp://a.rtmp.youtube.com/live2/***");
        }

        #[test]
        fn a_bare_endpoint_is_left_alone() {
            // There is nothing secret in an ingest endpoint, and blanking part
            // of it would make the panel useless for telling destinations
            // apart.
            for url in [
                "rtmp://a.rtmp.youtube.com/live2",
                "rtmp://live.twitch.tv/app",
                "rtmps://live-api-s.facebook.com:443/rtmp",
            ] {
                assert_eq!(redact_rtmp(url), url, "{url} should not have been redacted");
            }
        }

        #[test]
        fn a_trailing_slash_does_not_hide_the_application_name() {
            assert_eq!(
                redact_rtmp("rtmp://a.rtmp.youtube.com/live2/"),
                "rtmp://a.rtmp.youtube.com/live2"
            );
        }

        #[test]
        fn a_deep_path_keeps_only_the_last_segment_hidden() {
            let shown = redact_rtmp("rtmp://host.example/app/instance/secretkey");
            assert!(shown.contains("app/instance"));
            assert!(!shown.contains("secretkey"));
        }

        #[test]
        fn nonsense_is_returned_rather_than_panicking() {
            // The address comes from a text field, so it can be anything.
            assert_eq!(redact_rtmp(""), "");
            assert_eq!(redact_rtmp("   "), "");
            // A scheme with nothing after it loses its slashes and keeps its
            // shape. It is nonsense either way; what matters is that it comes
            // back rather than panicking the streaming panel.
            assert_eq!(redact_rtmp("rtmp://"), "rtmp:");
        }
    }

    mod destinations {
        use super::*;

        #[test]
        fn an_srt_address_is_told_apart_from_an_rtmp_one() {
            assert!(is_srt("srt://198.51.100.1:9000"));
            assert!(is_srt("  SRT://198.51.100.1:9000  "), "scheme is not case sensitive");
            assert!(!is_srt("rtmp://a.rtmp.youtube.com/live2"));
            assert!(!is_srt("rtmps://live-api-s.facebook.com:443/rtmp"));
            assert!(!is_srt(""));
        }
    }

    #[test]
    fn removing_a_lower_index_shifts_the_ones_above_it_down() {
        // Removing input 1 must move what was input 3 to index 2, not leave a
        // stored index pointing at a different source.
        assert_eq!(remap_after_removal(1, 3), Some(2));
        assert_eq!(remap_after_removal(1, 2), Some(1));
    }

    #[test]
    fn removing_a_higher_index_leaves_lower_ones_alone() {
        assert_eq!(remap_after_removal(3, 0), Some(0));
        assert_eq!(remap_after_removal(3, 2), Some(2));
    }

    #[test]
    fn the_removed_index_itself_resolves_to_nothing() {
        // The caller must decide what to do rather than silently inherit
        // whatever moved into that slot.
        assert_eq!(remap_after_removal(2, 2), None);
    }

    #[test]
    fn input_settings_zoom_scales_about_the_centre() {
        // Zooming from a corner would send the shot off frame instead of
        // pushing in on what the operator is looking at.
        let settings = InputSettings { zoom: 2.0, ..Default::default() };
        let rect = settings.apply(Rect::new(0.0, 0.0, 100.0, 100.0));
        assert_eq!(rect.width, 200.0);
        assert_eq!(rect.height, 200.0);
        assert_eq!(rect.x, -50.0, "should grow equally either side");
        assert_eq!(rect.y, -50.0);
    }

    #[test]
    fn input_settings_pan_moves_by_a_fraction_of_the_frame() {
        let settings = InputSettings { zoom: 1.0, offset_x: 0.25, offset_y: -0.5, ..Default::default() };
        let rect = settings.apply(Rect::new(0.0, 0.0, 200.0, 100.0));
        assert_eq!(rect.x, 50.0);
        assert_eq!(rect.y, -50.0);
    }

    #[test]
    fn default_input_settings_leave_the_rectangle_untouched() {
        let settings = InputSettings::default();
        assert!(settings.is_default());
        let original = Rect::new(10.0, 20.0, 300.0, 200.0);
        let rect = settings.apply(original);
        assert_eq!((rect.x, rect.y, rect.width, rect.height), (10.0, 20.0, 300.0, 200.0));
    }

    #[test]
    fn a_silly_zoom_is_clamped_rather_than_producing_a_degenerate_rect() {
        let settings = InputSettings { zoom: 9999.0, ..Default::default() };
        let rect = settings.apply(Rect::new(0.0, 0.0, 100.0, 100.0));
        assert!(rect.width <= 800.0, "zoom should clamp, got {}", rect.width);
    }
}
