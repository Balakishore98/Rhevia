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
    /// Drives the transition by hand, from the T-bar.
    ///
    /// A T-bar is not a progress indicator. It is how a director takes a
    /// transition at the speed the moment needs — slow under a speech, quick
    /// out of a song — and it has to be the same control that shows where the
    /// transition has got to.
    SetTransitionProgress(f32),
    /// Lets go of the T-bar. A transition taken all the way completes; one
    /// left short is abandoned and Program stays where it was.
    ReleaseTransition,
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
    /// Adds an empty input that other inputs are stacked into.
    ///
    /// A video and a logo taken to air as one thing rather than as two that
    /// have to be switched together and can get out of step.
    AddLayeredSource { name: String },
    /// Puts `layer` on top of the stack in `input`.
    AddLayer { input: usize, layer: usize },
    RemoveLayer { input: usize, at: usize },
    SetLayer { input: usize, at: usize, layer: StackLayer },
    /// Moves a layer up or down the stack. True raises it.
    MoveLayer { input: usize, at: usize, up: bool },
    SetOverlaySource { slot: usize, input: usize },
    /// Chooses where an overlay slot draws.
    SetOverlayMode { slot: usize, mode: OverlayMode },
    /// Chooses how an overlay arrives and leaves.
    SetOverlayAnimation { slot: usize, animation: OverlayAnimation },
    /// Puts a still image over the programme, always, wherever it is chosen
    /// to sit. A church logo in the corner of every shot.
    SetWatermark { path: String },
    ClearWatermark,
    SetWatermarkLook { corner: usize, scale: f32, opacity: f32 },
    /// The strap of text that crawls along the foot of the frame.
    SetTicker { text: String, on: bool },
    SetTickerLook { speed: f32, background: [u8; 3], colour: [u8; 3] },
    /// Changes what the stream leaves at.
    ///
    /// Takes effect on the next frame rather than the next show: an operator
    /// who finds the upload cannot carry 1080p halfway through a service
    /// needs to drop to 720p without going off air.
    SetStreamQuality { size: crate::settings::StreamSize, kbps: u32, audio_kbps: u32 },
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
    /// Input gain, before everything else on the channel.
    ///
    /// The fader balances one source against another and lives around unity.
    /// A quiet source -- a phone on a stand, a mixing desk's headphone out,
    /// a camera's built-in microphone -- needs to be brought up to where a
    /// fader can work with it at all, and that is a different control with a
    /// different range. It starts at nothing and is raised as needed.
    SetChannelTrim { channel: usize, db: f32 },
    ToggleMute(usize),
    ToggleSolo(usize),
    SetPan { channel: usize, pan: f32 },
    ToggleFollowProgram(usize),
    /// Routes a channel to a bus, or stops routing it there.
    SetChannelBus { channel: usize, bus: usize, on: bool },
    SetMasterGain(f32),
    /// Starts or stops listening to the programme, on a chosen device.
    ///
    /// Nothing to do with the master fader: turning the monitor down must not
    /// turn the stream down, and muting the stream must not leave the
    /// operator deaf.
    SetMonitor { device: Option<String>, on: bool },
    SetMonitorGain(f32),
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
    /// Works the transport of a playable input.
    ///
    /// A clip an operator cannot hold or cue is something they are watching
    /// rather than a source they are using: a walk-in video has to be stopped
    /// at the top and taken at the moment the service starts, not whenever it
    /// happens to have looped round to.
    MediaTransport { input: usize, action: MediaAction },
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

/// One picture inside a layered input.
///
/// Position is held as fractions of the frame rather than pixels, so a stack
/// built at 1080p still lines up if the production is later run at 720p or
/// 2160p.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StackLayer {
    /// Which input supplies the picture, by its place in the matrix.
    pub input: usize,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// 0 invisible, 1 solid. A logo at 0.6 sits over a shot without hiding it.
    pub opacity: f32,
    /// Off rather than removed, so a layer can be tried and put back.
    pub visible: bool,
    /// Letterbox inside the rectangle rather than stretching to fill it.
    pub preserve_aspect: bool,
}

impl Default for StackLayer {
    fn default() -> Self {
        // Full frame: a layer added on top of nothing should be visible
        // immediately, not a speck in the corner to be hunted for.
        Self {
            input: 0,
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
            opacity: 1.0,
            visible: true,
            preserve_aspect: true,
        }
    }
}

impl StackLayer {
    /// The named places a layer is usually put.
    pub const PLACES: [(&'static str, f32, f32, f32, f32); 6] = [
        ("FULL", 0.0, 0.0, 1.0, 1.0),
        ("LOWER L", 0.04, 0.62, 0.30, 0.30),
        ("LOWER R", 0.66, 0.62, 0.30, 0.30),
        ("UPPER L", 0.04, 0.06, 0.30, 0.30),
        ("UPPER R", 0.66, 0.06, 0.30, 0.30),
        ("CENTRE", 0.15, 0.15, 0.70, 0.70),
    ];

    fn rect(&self, width: usize, height: usize) -> Rect {
        let (w, h) = (width as f32, height as f32);
        Rect::new(self.x * w, self.y * h, self.width * w, self.height * h)
    }

    /// Keeps a layer somewhere it can be seen and dragged back from.
    pub fn sane(mut self) -> Self {
        self.width = self.width.clamp(0.02, 4.0);
        self.height = self.height.clamp(0.02, 4.0);
        self.x = self.x.clamp(-2.0, 2.0);
        self.y = self.y.clamp(-2.0, 2.0);
        self.opacity = self.opacity.clamp(0.0, 1.0);
        self
    }
}

/// How many layers one input may stack.
///
/// Each one is a full composite pass over the frame, and the whole stack has
/// to be built every tick inside the same budget as everything else.
pub const MAX_LAYERS: usize = 8;

/// Where an overlay slot draws.
///
/// Each slot chooses, rather than being fixed by its number. An operator
/// running a service wants slot 1 to be the verse full-screen and slot 2 to
/// be a camera full-screen, and being told that slot 1 can only ever be a
/// corner box is an arbitrary rule to work around.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OverlayMode {
    /// Covers the programme completely. The one an operator reaches for when
    /// the graphic *is* the shot: a verse, a notice, a second camera taken
    /// whole without disturbing what is in Preview.
    #[default]
    Full,
    /// Small, bottom left. The usual place for a name or a lower third.
    BottomLeft,
    /// Small, top right. Where a logo or a clock goes.
    TopRight,
    /// Half the frame, on the right. For a talking head beside a slide.
    RightHalf,
    /// Centred and large, with the programme showing around the edge.
    Centre,
}

impl OverlayMode {
    pub const ALL: [OverlayMode; 5] = [
        OverlayMode::Full,
        OverlayMode::BottomLeft,
        OverlayMode::TopRight,
        OverlayMode::RightHalf,
        OverlayMode::Centre,
    ];

    pub fn label(self) -> &'static str {
        match self {
            OverlayMode::Full => "FULL",
            OverlayMode::BottomLeft => "LOWER L",
            OverlayMode::TopRight => "UPPER R",
            OverlayMode::RightHalf => "RIGHT ½",
            OverlayMode::Centre => "CENTRE",
        }
    }

    /// What it means, in the words someone running a service would use.
    pub fn hint(self) -> &'static str {
        match self {
            OverlayMode::Full => "covers the programme completely",
            OverlayMode::BottomLeft => "small, bottom left — a name or a lower third",
            OverlayMode::TopRight => "small, top right — a logo or a clock",
            OverlayMode::RightHalf => "half the frame on the right, beside the shot",
            OverlayMode::Centre => "large and centred, programme showing around it",
        }
    }

    fn rect(self) -> Rect {
        let w = OUTPUT_WIDTH() as f32;
        let h = OUTPUT_HEIGHT() as f32;
        match self {
            OverlayMode::Full => Rect::full(OUTPUT_WIDTH(), OUTPUT_HEIGHT()),
            OverlayMode::BottomLeft => Rect::new(w * 0.04, h * 0.62, w * 0.30, h * 0.30),
            OverlayMode::TopRight => Rect::new(w * 0.66, h * 0.06, w * 0.30, h * 0.30),
            OverlayMode::RightHalf => Rect::new(w * 0.50, h * 0.08, w * 0.46, h * 0.84),
            OverlayMode::Centre => Rect::new(w * 0.12, h * 0.10, w * 0.76, h * 0.76),
        }
    }
}

/// How an overlay arrives and leaves.
///
/// A lower third that appears between one frame and the next looks like a
/// fault. Every broadcast graphic moves on and moves off, and it is the
/// movement that makes it read as deliberate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OverlayAnimation {
    /// Straight on, straight off. For a graphic that has to be exact.
    Cut,
    /// Fades up and down. Works for anything, including a full-screen shot.
    Fade,
    /// Slides in from the left. The usual move for a lower third.
    #[default]
    SlideLeft,
    /// Slides in from the right.
    SlideRight,
    /// Rises from the bottom. For a strap along the foot of the frame.
    SlideUp,
    /// Wipes open from the leading edge, like a bar being drawn.
    Wipe,
}

impl OverlayAnimation {
    pub const ALL: [OverlayAnimation; 6] = [
        OverlayAnimation::Cut,
        OverlayAnimation::Fade,
        OverlayAnimation::SlideLeft,
        OverlayAnimation::SlideRight,
        OverlayAnimation::SlideUp,
        OverlayAnimation::Wipe,
    ];

    pub fn label(self) -> &'static str {
        match self {
            OverlayAnimation::Cut => "CUT",
            OverlayAnimation::Fade => "FADE",
            OverlayAnimation::SlideLeft => "IN \u{2190}",
            OverlayAnimation::SlideRight => "IN \u{2192}",
            OverlayAnimation::SlideUp => "IN \u{2191}",
            OverlayAnimation::Wipe => "WIPE",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            OverlayAnimation::Cut => "straight on and straight off",
            OverlayAnimation::Fade => "fades up and down — works for anything",
            OverlayAnimation::SlideLeft => "slides in from the left, the usual lower third",
            OverlayAnimation::SlideRight => "slides in from the right",
            OverlayAnimation::SlideUp => "rises from the bottom of the frame",
            OverlayAnimation::Wipe => "opens from the leading edge, like a bar being drawn",
        }
    }

    /// Turns progress into what the layer should look like.
    ///
    /// Eased rather than linear. A graphic that starts and stops abruptly
    /// reads as a jump however long it takes; easing is most of what
    /// separates a broadcast graphic from a moving rectangle.
    fn apply(self, progress: f32, rect: Rect, width: f32) -> (Rect, f32) {
        let p = progress.clamp(0.0, 1.0);
        // Smoothstep in, and the same curve out.
        let eased = p * p * (3.0 - 2.0 * p);
        match self {
            OverlayAnimation::Cut => (rect, if p > 0.5 { 1.0 } else { 0.0 }),
            OverlayAnimation::Fade => (rect, eased),
            OverlayAnimation::SlideLeft => {
                let travel = (rect.x + rect.width) * (1.0 - eased);
                (Rect { x: rect.x - travel, ..rect }, eased.min(1.0))
            }
            OverlayAnimation::SlideRight => {
                let travel = (width - rect.x) * (1.0 - eased);
                (Rect { x: rect.x + travel, ..rect }, eased.min(1.0))
            }
            OverlayAnimation::SlideUp => {
                let travel = rect.height * 1.6 * (1.0 - eased);
                (Rect { y: rect.y + travel, ..rect }, eased.min(1.0))
            }
            OverlayAnimation::Wipe => {
                // Grows from the leading edge, so the bar appears to be drawn.
                (Rect { width: rect.width * eased.max(0.001), ..rect }, 1.0)
            }
        }
    }
}

/// How long an overlay takes to arrive or leave.
///
/// Fast enough not to hold up a service, slow enough to read as a move
/// rather than a glitch. Broadcast lower thirds sit around here.
pub const OVERLAY_ANIMATION_SECONDS: f32 = 0.45;

/// The arrangement each slot starts out in.
///
/// All four full-screen, which is the thing that was asked for and the more
/// useful default: a slot whose graphic covers the screen is obvious the
/// moment it is turned on, whereas a corner box on a dark shot can be missed.
fn default_overlay_modes() -> [OverlayMode; 4] {
    [OverlayMode::Full; 4]
}

/// What the transport controls can be asked to do.
///
/// Not `Transport`: that name already belongs to how the stream leaves the
/// building, and confusing the two in a switcher would be a bad joke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaAction {
    /// Holds the clip where it is, or lets it carry on.
    PlayPause,
    /// Back to the top and held there, cued and ready.
    Stop,
    /// Five seconds back, which is the step for finding a cue point by ear.
    Back,
    /// Five seconds on.
    Forward,
}

/// How far the input gain can be taken.
///
/// Thirty decibels up is a factor of about thirty in level, which is what it
/// takes to bring a line output plugged into a microphone input up to
/// something a fader can balance. Twenty down is for the opposite mistake,
/// which is rarer but just as ruinous.
pub const TRIM_MIN_DB: f32 = -20.0;
pub const TRIM_MAX_DB: f32 = 30.0;

/// How far a step of the transport moves.
///
/// Five seconds because that is what the operator asked for and what every
/// player uses: long enough to be worth pressing, short enough to land on.
pub const TRANSPORT_STEP_SECONDS: f32 = 5.0;

/// Where a playable input has got to, as the transport bar needs it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MediaState {
    pub position_seconds: f32,
    /// None for a file with no measurable length, which is normal for a
    /// stream: the bar then shows the position and no scale.
    pub duration_seconds: Option<f32>,
    pub paused: bool,
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
    /// Shared rather than owned: the interface clones the whole snapshot on
    /// every repaint, and a full-size picture copied each time would cost
    /// more than compositing it did.
    pub program: Option<Arc<Frame>>,
    /// What is armed next, at full size.
    ///
    /// Its own picture rather than the input's thumbnail. The Preview monitor
    /// is the same size on screen as Program, and feeding it a 320x180
    /// thumbnail showed the operator a blurred picture of a sharp source —
    /// which is indistinguishable from the source being bad.
    pub preview: Option<Arc<Frame>>,
    pub streaming: bool,
    pub stream_error: Option<String>,
    pub stats: Stats,
    pub layout: Layout,
    /// Which source each overlay slot carries.
    pub overlay_source: [Option<usize>; 4],
    /// Which overlay slots are currently on air.
    pub overlay_on: [bool; 4],
    /// Where each slot draws when it is on.
    pub overlay_mode: [OverlayMode; 4],
    /// How each slot arrives and leaves.
    pub overlay_animation: [OverlayAnimation; 4],
    /// Where each slot is between off and on, 0 to 1. Mid-way means it is
    /// still moving.
    pub overlay_progress: [f32; 4],
    /// Every destination currently being fed, in the order they were added.
    pub destinations: Vec<DestinationState>,
    /// The name the programme is being published under over NDI, if it is.
    pub ndi_output: Option<String>,
    /// The device the programme is being listened on, if any.
    pub monitor: Option<String>,
    /// Listening level in dB, which is not the master fader.
    pub monitor_gain_db: f32,
    /// Discontinuities spliced into a clip's sound: silence handed over when
    /// there was not enough, and jumps when the buffer was trimmed. Both are
    /// heard as a pop.
    pub audio_padded: u64,
    pub audio_trimmed: u64,
    /// Blocks thrown away because the listening cushion outgrew its limit.
    pub audio_dropped: u64,
    /// How much sound is waiting to be played, in frames.
    pub audio_buffered: usize,
    /// What the stream is leaving at, and what it is allowed to use.
    pub stream_size: crate::settings::StreamSize,
    pub stream_kbps: u32,
    pub stream_audio_kbps: u32,
    /// What it is actually using, measured over the last few seconds.
    ///
    /// Not the same number as the one it was told to use: an encoder spends
    /// less than its allowance on an easy picture and overshoots on a hard
    /// one, and what matters to a hall's connection is what is really going
    /// out of the door.
    pub stream_measured_kbps: f32,
    /// How many times the sound card asked for audio and found none ready.
    ///
    /// Each one is a gap, and gaps arriving at the tick rate are heard as a
    /// crackle rather than as silence — which is how this was reported.
    /// Surfaced so that "the audio is crackling" is a number rather than an
    /// argument.
    pub audio_gaps: u64,
    /// How many inputs are still opening.
    ///
    /// A camera can take seconds to answer. Saying so is the difference
    /// between waiting and thinking the program has stopped responding.
    pub opening: usize,
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
    /// Input gain, applied before everything else on the channel.
    pub trim_db: f32,
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
    pub thumbnail: Option<Arc<Frame>>,
    /// Current text, when this input is a title. Lets the UI offer an edit
    /// without keeping its own copy of what the engine holds.
    pub title: Option<(String, String)>,
    pub settings: InputSettings,
    /// What kind of source this is, for the settings dialog and the filters.
    pub kind: &'static str,
    /// Set when this input is a file that can be held and cued.
    pub media: Option<MediaState>,
    /// Set when this input is a stack, holding it from bottom to top.
    pub layers: Option<Vec<StackLayer>>,
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
    /// Asks the engine to stop, and waits until it has.
    ///
    /// Sending and walking away is not the same thing. The engine has
    /// cameras to close, decoders to kill and a sound card to release, and
    /// until it has done all of that it is still using them -- so a caller
    /// that starts a second engine straight away ends up with two running,
    /// both starved, and no way to tell why. It is also why installing over
    /// a running copy used to fail: the process had been asked to go and had
    /// not finished going.
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);

        // Bounded. An engine wedged in a driver call must not take the
        // program down with it.
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.running.load(Ordering::Relaxed) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if self.running.load(Ordering::Relaxed) {
            tracing::warn!("the engine did not stop within five seconds");
        }
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
    /// Several inputs composited into one.
    ///
    /// Carries its own compositor rather than borrowing the programme's: this
    /// is rendered while building the inputs, before the programme scene
    /// exists, and it needs its own canvas to draw onto.
    Layered {
        layers: Vec<StackLayer>,
        compositor: Box<rhevia_engine::Compositor>,
    },
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

/// What the production runs at, read once at startup.
///
/// Not a constant any more: a 1080p file in a 720p production is a 720p file
/// from the moment it is decoded, so this is the operator's choice rather
/// than mine. It cannot change while running — the encoder, the compositor
/// and every decoder are built around it — which is why it is read here and
/// applied when Rhevia starts.
static OUTPUT_SIZE: std::sync::OnceLock<(usize, usize)> = std::sync::OnceLock::new();

pub fn output_size() -> (usize, usize) {
    *OUTPUT_SIZE.get_or_init(|| crate::settings::Settings::load().resolution.size())
}

#[allow(non_snake_case)]
fn OUTPUT_WIDTH() -> usize {
    output_size().0
}

#[allow(non_snake_case)]
fn OUTPUT_HEIGHT() -> usize {
    output_size().1
}

const TARGET_FPS: f32 = 30.0;
const THUMBNAIL_WIDTH: usize = 320;
const THUMBNAIL_HEIGHT: usize = 180;

/// The programme and preview pictures the interface draws.
///
/// Full programme size. These were half that, which cost nothing to send but
/// showed the operator a soft picture and made them doubt the stream — the
/// stream was always full quality; only the monitor on screen was not. The
/// frames are shared rather than copied into each snapshot, so the larger
/// size costs a reference count rather than four megabytes a tick.
#[allow(non_snake_case)]
fn PREVIEW_WIDTH() -> usize {
    OUTPUT_WIDTH()
}

#[allow(non_snake_case)]
fn PREVIEW_HEIGHT() -> usize {
    OUTPUT_HEIGHT()
}
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

/// A device that has finished opening on a worker thread.
///
/// Opening a device is slow — a camera can take seconds to answer, NDI waits
/// for a connection, a media file starts two decoders — and the engine loop
/// is what renders and encodes the programme. Doing it there stops the
/// picture until it finishes, which is what makes adding an input look like
/// the program has hung. The work happens elsewhere and arrives here ready.
enum Opened {
    /// A new input, with its sound if it brought any.
    Source { name: String, source: Source, audio: Option<CaptureHandle>, follow_program: bool },
    /// Sound for an input that already exists.
    Attach { input: usize, audio: CaptureHandle },
    /// A plugin for one channel's chain.
    Plugin { channel: usize, instance: Box<rhevia_plugin::vst3::Vst3Instance> },
    /// It could not be opened, and this is what to tell the operator.
    Failed(String),
}

fn run(
    commands: std::sync::mpsc::Receiver<Command>,
    snapshot: Arc<Mutex<Snapshot>>,
    running: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let settings = EncoderSettings {
        width: OUTPUT_WIDTH(),
        height: OUTPUT_HEIGHT(),
        bitrate_bps: crate::settings::Settings::load().resolution.bitrate(),
        fps: TARGET_FPS,
        keyframe_interval: (TARGET_FPS as u32) * 2,
    };
    let mut mixer = Mixer::new(settings)?;
    {
        // Built for the stream's own size from the start, so going live does
        // not change what has already been recorded.
        let chosen = crate::settings::Settings::load();
        let (w, h) = chosen.stream_size.size();
        let _ = mixer.set_encoder(EncoderSettings {
            width: w,
            height: h,
            bitrate_bps: chosen.stream_kbps * 1000,
            fps: TARGET_FPS,
            keyframe_interval: (TARGET_FPS * 2.0) as u32,
        });
    }
    let mut audio = AudioMixer::new();
    // Fed from the master bus every tick, so the integrated figure covers the
    // whole session rather than only the part that was streamed.
    let mut loudness = rhevia_audio::LoudnessMeter::new();
    // Publishes the programme for other machines on the network. Independent
    // of streaming: a production often sends NDI to a recorder or a second
    // switcher while streaming to a platform.
    let mut ndi_output: Option<rhevia_ndi::NdiSender> = None;
    // Hearing the programme. Off until asked for, because a machine with the
    // speakers next to the microphone would howl the moment it started.
    // On from the start. An operator who adds a video expects to hear it;
    // making that a button they have to find means the program looks broken
    // until they find it.
    // Listening from the start, on the device that was chosen last time.
    // Someone who adds a video and hears nothing has found a fault, not a
    // preference, so this is on unless it was deliberately turned off.
    let chosen = crate::settings::Settings::load();
    // Not under test unless asked for. Every engine test starts a real
    // engine, and a real engine listens by default -- so running the suite
    // played every test tone and every test clip out of the speakers of
    // whoever was sitting at the machine. Proving the monitor works needs
    // real sound; proving a transition works does not.
    let allowed = !cfg!(test)
        || std::env::var("RHEVIA_AUDIBLE_TESTS").is_ok_and(|v| v != "0");
    let mut monitor = if chosen.monitor && allowed {
        match rhevia_audio::AudioMonitor::open(chosen.monitor_device.as_deref()) {
            Ok(monitor) => Some(monitor),
            // Falling back to the default rather than going silent: a device
            // that was remembered may have been unplugged since.
            Err(e) if chosen.monitor_device.is_some() => {
                tracing::warn!(
                    error = %e,
                    device = ?chosen.monitor_device,
                    "the chosen device would not open; using the default"
                );
                rhevia_audio::AudioMonitor::open(None).ok()
            }
            Err(e) => {
                tracing::warn!(error = %e, "nothing to listen on");
                None
            }
        }
    } else {
        None
    };
    let mut monitor_gain_db: f32 = 0.0;
    // True while the operator has hold of the T-bar, so the automatic advance
    // does not fight them for it.
    let mut dragging_transition = false;
    let mut sources: Vec<SourceSlot> = Vec::new();

    // The first two mixer inputs belong to the always-on graphics and are
    // never handed to a source. They are held at the bottom deliberately:
    // removing an input shifts every index above it, and graphics that moved
    // when an operator closed a camera would end up drawing whatever took
    // their place.
    mixer.set_input_name(WATERMARK_INPUT, "Watermark");
    mixer.set_input_name(TICKER_INPUT, "Ticker");

    // Two sources up front so the window is never an empty grid.
    sources.push(SourceSlot {
        source: Source::Bars,
        mixer_input: 2,
        audio: None,
        settings: InputSettings::default(),
        needs_push: true,
    });
    mixer.set_input_name(2, "Colour Bars");
    audio.add_channel("Colour Bars");
    sources.push(SourceSlot {
        source: Source::Colour([20, 90, 160]),
        mixer_input: 3,
        audio: None,
        settings: InputSettings::default(),
        needs_push: true,
    });
    mixer.set_input_name(3, "Blue");
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
    let mut overlay_mode = default_overlay_modes();
    // Input gain per channel, index-aligned with the mixer's channels. Zero
    // is "leave it alone", which is where every channel starts.
    let mut trim: Vec<f32> = Vec::new();
    let mut overlay_animation = [OverlayAnimation::default(); 4];
    // Where each slot is between off and on. Animated towards its switch
    // rather than following it, which is what makes it a move.
    let mut overlay_progress = [0.0f32; 4];
    let mut watermark: Option<Frame> = None;
    let mut watermark_path = String::new();
    let mut watermark_corner = 1usize;
    let mut watermark_scale = 0.12f32;
    let mut watermark_opacity = 0.75f32;
    let mut ticker_text = String::new();
    let mut ticker_on = false;
    let mut ticker_speed = 120.0f32;
    let mut ticker_background = [12u8, 18, 32];
    let mut ticker_colour = [235u8, 238, 245];
    let mut ticker_strip: Option<Frame> = None;
    let mut ticker_offset = 0.0f32;
    let chosen = crate::settings::Settings::load();
    let mut stream_size = chosen.stream_size;
    let mut stream_kbps = chosen.stream_kbps;
    let mut stream_audio_kbps = chosen.audio_kbps;
    // What is really leaving, smoothed over a few seconds. An encoder spends
    // less than its allowance on an easy picture and overshoots on a hard
    // one, and what a hall's connection has to carry is the real number.
    let mut measured_kbps = 0.0f32;
    let mut measured_bytes = 0u64;
    let mut measured_since = Instant::now();
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

    // Devices being opened elsewhere arrive here. Unbounded on purpose: an
    // operator cannot click fast enough for the queue to matter, and a bound
    // would mean dropping something they asked for.
    let (opened_tx, opened_rx) = std::sync::mpsc::channel::<Opened>();
    // How many are still opening, so the interface can say so.
    let mut opening: usize = 0;
    let mut recorder: Option<(std::io::BufWriter<std::fs::File>, String, u64)> = None;

    let frame_budget = Duration::from_secs_f32(1.0 / TARGET_FPS);
    // When audio was last generated, and the fraction of a sample carried
    // over from that tick. Together these are the engine's audio clock.
    let mut audio_clock = Instant::now();
    let mut audio_owed: f64 = 0.0;
    let start = Instant::now();
    let mut frame_number: u64 = 0;
    let mut fps_window = Instant::now();
    let mut fps_frames = 0u32;
    let mut measured_fps = 0.0f32;
    // Held between regenerations so the UI still has pictures on the frames
    // where none are made.
    let mut cached_inputs: Vec<InputInfo> = Vec::new();
    let mut cached_program: Option<Arc<Frame>> = None;

    while running.load(Ordering::Relaxed) {
        let tick = Instant::now();

        // ---- commands ------------------------------------------------------
        while let Ok(command) = commands.try_recv() {
            match command {
                Command::Shutdown => break,
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
                Command::SetTransitionProgress(p) => {
                    let p = p.clamp(0.0, 1.0);
                    if program_input == preview_input {
                        // Nothing to transition to; dragging would dissolve
                        // the programme into itself.
                    } else if p >= 1.0 {
                        // All the way is the same as having finished.
                        transition = None;
                        std::mem::swap(&mut program_input, &mut preview_input);
                        mixer.request_keyframe();
                        dragging_transition = false;
                    } else {
                        transition = Some(p);
                        // Held, so the automatic advance below leaves it
                        // alone while the operator has hold of it.
                        dragging_transition = true;
                    }
                }
                Command::ReleaseTransition => {
                    dragging_transition = false;
                    // Let go short of the end: the transition is abandoned
                    // rather than completed, which is what a director expects
                    // when they change their mind halfway.
                    if transition.is_some() {
                        transition = None;
                    }
                }
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
                    opening += 1;
                    open_elsewhere(&opened_tx, move || {
                        let Some(cid) = rhevia_plugin::parse_cid(&cid) else {
                            return Opened::Failed(format!("{name}: bad plugin identifier"));
                        };
                        match rhevia_plugin::vst3::Vst3Instance::open(
                            std::path::Path::new(&path),
                            cid,
                            &name,
                            SAMPLE_RATE as f64,
                            PLUGIN_BLOCK,
                        ) {
                            Ok(instance) => {
                                Opened::Plugin { channel, instance: Box::new(instance) }
                            }
                            Err(e) => Opened::Failed(format!("{name}: {e}")),
                        }
                    });
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
                Command::AddLayeredSource { name } => {
                    if let Ok(input) = mixer.add_input(name.clone()) {
                        sources.push(SourceSlot {
                            mixer_input: input,
                            source: Source::Layered {
                                layers: Vec::new(),
                                compositor: Box::new(rhevia_engine::Compositor::new(
                                    OUTPUT_WIDTH(),
                                    OUTPUT_HEIGHT(),
                                )),
                            },
                            audio: None,
                            needs_push: true,
                            settings: InputSettings::default(),
                        });
                        audio.add_channel(name);
                    }
                }
                Command::AddLayer { input, layer } => {
                    // A stack that contains itself would ask for the picture
                    // it is in the middle of making.
                    if input != layer {
                        if let Some(SourceSlot { source: Source::Layered { layers, .. }, .. }) =
                            sources.get_mut(input)
                        {
                            if layers.len() < MAX_LAYERS {
                                layers.push(StackLayer { input: layer, ..Default::default() });
                            }
                        }
                    }
                }
                Command::RemoveLayer { input, at } => {
                    if let Some(SourceSlot { source: Source::Layered { layers, .. }, .. }) =
                        sources.get_mut(input)
                    {
                        if at < layers.len() {
                            layers.remove(at);
                        }
                    }
                }
                Command::SetLayer { input, at, layer } => {
                    if input != layer.input {
                        if let Some(SourceSlot { source: Source::Layered { layers, .. }, .. }) =
                            sources.get_mut(input)
                        {
                            if let Some(existing) = layers.get_mut(at) {
                                *existing = layer.sane();
                            }
                        }
                    }
                }
                Command::MoveLayer { input, at, up } => {
                    if let Some(SourceSlot { source: Source::Layered { layers, .. }, .. }) =
                        sources.get_mut(input)
                    {
                        // Up the stack means later in the list, because later
                        // layers are drawn over earlier ones.
                        let to = if up { at + 1 } else { at.wrapping_sub(1) };
                        if at < layers.len() && to < layers.len() {
                            layers.swap(at, to);
                        }
                    }
                }
                Command::SetStreamQuality { size, kbps, audio_kbps } => {
                    let (w, h) = size.size();
                    let kbps = kbps.clamp(size.kbps_range().0, size.kbps_range().1);
                    if let Err(e) = mixer.set_encoder(EncoderSettings {
                        width: w,
                        height: h,
                        bitrate_bps: kbps * 1000,
                        fps: TARGET_FPS,
                        keyframe_interval: (TARGET_FPS * 2.0) as u32,
                    }) {
                        stream_error = Some(format!("could not change the stream quality: {e}"));
                    } else {
                        stream_size = size;
                        stream_kbps = kbps;
                        stream_audio_kbps = audio_kbps;
                        // Anything already watching needs a picture it can
                        // decode from, and the new encoder's first frame is
                        // one whether it is asked for or not -- asked for so
                        // that is true of the recording as well.
                        mixer.request_keyframe();
                        let mut chosen = crate::settings::Settings::load();
                        chosen.stream_size = size;
                        chosen.stream_kbps = kbps;
                        chosen.audio_kbps = audio_kbps;
                        chosen.save();
                    }
                }
                Command::SetOverlayAnimation { slot, animation } => {
                    if slot < 4 {
                        overlay_animation[slot] = animation;
                    }
                }
                Command::SetWatermark { path } => match rhevia_engine::load_image(&path) {
                    Ok(frame) => {
                        watermark_path = path;
                        watermark = Some(frame);
                    }
                    Err(e) => stream_error = Some(format!("could not load the watermark: {e}")),
                },
                Command::ClearWatermark => {
                    watermark = None;
                    watermark_path.clear();
                }
                Command::SetWatermarkLook { corner, scale, opacity } => {
                    watermark_corner = corner.min(3);
                    watermark_scale = scale.clamp(0.02, 0.5);
                    watermark_opacity = opacity.clamp(0.05, 1.0);
                }
                Command::SetTicker { text, on } => {
                    if text != ticker_text {
                        ticker_text = text;
                        // Re-drawn only when the words change; scrolling it is
                        // a matter of where it is read from, not of redrawing
                        // the letters thirty times a second.
                        ticker_strip = None;
                        ticker_offset = 0.0;
                    }
                    ticker_on = on && !ticker_text.trim().is_empty();
                }
                Command::SetTickerLook { speed, background, colour } => {
                    ticker_speed = speed.clamp(20.0, 600.0);
                    if background != ticker_background || colour != ticker_colour {
                        ticker_background = background;
                        ticker_colour = colour;
                        ticker_strip = None;
                    }
                }
                Command::SetOverlayMode { slot, mode } => {
                    if slot < 4 {
                        overlay_mode[slot] = mode;
                    }
                }
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
                    opening += 1;
                    open_elsewhere(&opened_tx, move || match rhevia_ndi::NdiReceiver::connect(source)
                    {
                        Ok(receiver) => Opened::Source {
                            name,
                            source: Source::Ndi(Box::new(receiver)),
                            audio: None,
                            follow_program: true,
                        },
                        Err(e) => Opened::Failed(e.to_string()),
                    });
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
                    opening += 1;
                    open_elsewhere(&opened_tx, move || {
                        match rhevia_media::MediaSource::open(
                            &path,
                            OUTPUT_WIDTH() as u32,
                            OUTPUT_HEIGHT() as u32,
                            TARGET_FPS,
                        ) {
                            Ok(media) => Opened::Source {
                                name,
                                source: Source::Media(Box::new(media)),
                                audio: None,
                                // A clip's own sound belongs to the clip, not
                                // to whether it happens to be on air.
                                follow_program: false,
                            },
                            Err(e) => Opened::Failed(e.to_string()),
                        }
                    });
                }
                Command::AddCameraSource { name, target } => {
                    opening += 1;
                    open_elsewhere(&opened_tx, move || {
                        match rhevia_capture::CameraCapture::start(target) {
                            Ok(capture) => Opened::Source {
                                name,
                                source: Source::Camera(capture),
                                audio: None,
                                follow_program: true,
                            },
                            Err(e) => Opened::Failed(format!("camera: {e}")),
                        }
                    });
                }
                Command::AddScreenSource { name, target } => {
                    opening += 1;
                    open_elsewhere(&opened_tx, move || {
                        match rhevia_capture::ScreenCapture::start(target, TARGET_FPS) {
                            Ok(capture) => Opened::Source {
                                name,
                                source: Source::Screen(capture),
                                audio: None,
                                follow_program: true,
                            },
                            Err(e) => Opened::Failed(format!("screen capture: {e}")),
                        }
                    });
                }
                Command::AddAudioSource { name, device } => {
                    opening += 1;
                    open_elsewhere(&opened_tx, move || {
                        match CaptureHandle::open(device.as_deref()) {
                            Ok(handle) => Opened::Source {
                                name,
                                source: Source::AudioOnly,
                                audio: Some(handle),
                                // Sound with no picture is almost always a
                                // microphone, and a microphone must stay live
                                // when the camera beside it goes off air.
                                follow_program: false,
                            },
                            Err(e) => Opened::Failed(format!("audio device: {e}")),
                        }
                    });
                }
                Command::AttachAudio { input, device } => {
                    opening += 1;
                    open_elsewhere(&opened_tx, move || {
                        match CaptureHandle::open(device.as_deref()) {
                            Ok(handle) => Opened::Attach { input, audio: handle },
                            Err(e) => Opened::Failed(format!("audio device: {e}")),
                        }
                    });
                }
                Command::SetChannelTrim { channel, db } => {
                    if channel < trim.len() {
                        trim[channel] = db.clamp(TRIM_MIN_DB, TRIM_MAX_DB);
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
                Command::SetMonitor { device, on } => {
                    if !on {
                        monitor = None;
                    } else {
                        match rhevia_audio::AudioMonitor::open(device.as_deref()) {
                            Ok(opened) => {
                                opened.set_gain_db(monitor_gain_db);
                                monitor = Some(opened);
                            }
                            Err(e) => stream_error = Some(format!("monitor: {e}")),
                        }
                    }
                }
                Command::SetMonitorGain(db) => {
                    monitor_gain_db = db.clamp(rhevia_audio::SILENCE_DB, 12.0);
                    if let Some(monitor) = &monitor {
                        monitor.set_gain_db(monitor_gain_db);
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
                            rhevia_engine::render_title(font, &style, OUTPUT_WIDTH(), OUTPUT_HEIGHT());
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
                Command::MediaTransport { input, action } => {
                    if let Some(SourceSlot { source: Source::Media(media), .. }) =
                        sources.get_mut(input)
                    {
                        // A skip restarts ffmpeg, which takes a moment. It is
                        // done here on the engine thread rather than a worker
                        // because the alternative is two transports racing for
                        // the same decoder, and a clip that ends up wherever
                        // the presses happened to land.
                        let outcome = match action {
                            MediaAction::PlayPause => {
                                media.set_paused(!media.paused());
                                Ok(())
                            }
                            MediaAction::Stop => media.stop(),
                            MediaAction::Back => media.skip(-TRANSPORT_STEP_SECONDS),
                            MediaAction::Forward => media.skip(TRANSPORT_STEP_SECONDS),
                        };
                        if let Err(e) = outcome {
                            tracing::warn!(error = %e, ?action, "the transport could not move the clip");
                            stream_error = Some(format!("could not move the clip: {e}"));
                        }
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
                                OUTPUT_WIDTH(),
                                OUTPUT_HEIGHT(),
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
                    match open_delivery(&url, &key, stream_audio_kbps) {
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
        // Measured in real time, not in ticks.
        //
        // A fixed block per tick looks like it locks sound to picture, and
        // would if the loop ran at exactly thirty. It does not: the loop
        // sleeps to fill the frame budget and that sleep rounds up, so a tick
        // is nearer 33.6 ms than 33.3 and the engine hands the sound card
        // about 47,600 samples for every 48,000 it asks for. The card fills
        // the shortfall with silence thirty times a second, which is not
        // heard as silence -- it is heard as a crackle over everything.
        //
        // So the sound card's clock wins, as it always must, and the block is
        // however much real time has passed. The fraction of a sample left
        // over is carried rather than dropped, or the rounding puts the drift
        // straight back.
        let now = Instant::now();
        // How long this tick really took. Everything that moves over time --
        // sound, transitions, graphics -- is measured against this rather
        // than against the frame budget, because the budget is what the loop
        // aims at and not what it achieves. Clocking a one second fade in
        // frames makes it take one second at thirty a second and eight at
        // four, which is a lower third crawling onto air on a loaded
        // machine.
        let elapsed = now.duration_since(audio_clock).as_secs_f32().min(0.25);
        audio_owed += now.duration_since(audio_clock).as_secs_f64() * SAMPLE_RATE as f64;
        audio_clock = now;
        // After a stall, catch up over the following ticks rather than
        // generating a second of sound in one go and blocking the picture.
        audio_owed = audio_owed.min(SAMPLE_RATE as f64 / 4.0);
        let audio_frames = audio_owed as usize;
        audio_owed -= audio_frames as f64;
        // One DSP chain per channel. Grown here rather than at every add
        // site, so a chain can never be missing for a channel that exists.
        while dsp.len() < audio.channels.len() {
            dsp.push(rhevia_audio::ChannelDsp::new());
            plugins.push(Vec::new());
            trim.push(0.0);
        }
        dsp.truncate(audio.channels.len());
        trim.truncate(audio.channels.len());

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
            // Input gain first, before anything else touches the channel.
            // A gate set to open at -45 dB is useless on a source that never
            // reaches it, and a compressor set for a normal level does
            // nothing to one twenty decibels below it -- so the level has to
            // be right before the processing sees it, exactly as the gain
            // knob on a desk comes before the channel strip.
            if let (Some(buffer), Some(&db)) = (buffer.as_mut(), trim.get(index)) {
                if db.abs() > 0.01 {
                    let scale = rhevia_audio::db_to_amplitude(db);
                    for sample in &mut buffer.samples {
                        *sample *= scale;
                    }
                }
            }
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

        // ---- devices that finished opening ---------------------------------
        // Drained rather than waited on: whatever is ready joins the show, and
        // anything still opening arrives on a later tick.
        while let Ok(finished) = opened_rx.try_recv() {
            opening = opening.saturating_sub(1);
            match finished {
                Opened::Source { name, source, audio: capture, follow_program } => {
                    if let Ok(input) = mixer.add_input(name.clone()) {
                        sources.push(SourceSlot {
                            source,
                            mixer_input: input,
                            audio: capture,
                            settings: InputSettings::default(),
                            needs_push: true,
                        });
                        let channel = audio.add_channel(name);
                        if let Some(strip) = audio.channel_mut(channel) {
                            strip.follow_program = follow_program;
                        }
                    }
                }
                Opened::Attach { input, audio: capture } => {
                    if let Some(slot) = sources.get_mut(input) {
                        slot.audio = Some(capture);
                        // Sound belonging to a camera should come up with that
                        // camera, so this follows Program.
                        if let Some(strip) = audio.channel_mut(input) {
                            strip.follow_program = true;
                        }
                    }
                }
                Opened::Plugin { channel, instance } => {
                    if let Some(chain) = plugins.get_mut(channel) {
                        chain.push(*instance);
                    }
                }
                Opened::Failed(reason) => stream_error = Some(reason),
            }
        }

        // ---- sources -------------------------------------------------------
        let seconds = start.elapsed().as_secs_f32();
        // Which mixer input each place in the matrix uses. Taken before the
        // loop because a layered input needs to look up its layers' inputs
        // while the list is being walked.
        let mixer_inputs: Vec<usize> = sources.iter().map(|s| s.mixer_input).collect();
        for slot in &mut sources {
            match &mut slot.source {
                Source::Colour(rgb) => {
                    if slot.needs_push {
                        let _ = mixer.push_frame(
                            slot.mixer_input,
                            Frame::filled(OUTPUT_WIDTH(), OUTPUT_HEIGHT(), *rgb),
                        );
                        slot.needs_push = false;
                    }
                }
                Source::Bars => {
                    let _ = mixer.push_frame(slot.mixer_input, bars(OUTPUT_WIDTH(), OUTPUT_HEIGHT(), seconds));
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
                        audio_tile(OUTPUT_WIDTH(), OUTPUT_HEIGHT(), level),
                    );
                }
                Source::Layered { layers, compositor } => {
                    // Built every tick rather than only when something
                    // changes: the layers are live inputs, and a stack that
                    // only redrew on an edit would freeze the video inside it.
                    let mut scene = Scene::new();
                    scene.background = [0, 0, 0];
                    for layer in layers.iter().filter(|l| l.visible) {
                        let Some(&input) = mixer_inputs.get(layer.input) else { continue };
                        if input == slot.mixer_input {
                            continue;
                        }
                        scene.push(Layer {
                            opacity: layer.opacity,
                            preserve_aspect: layer.preserve_aspect,
                            ..Layer::new(input, layer.rect(OUTPUT_WIDTH(), OUTPUT_HEIGHT()))
                        });
                    }

                    // Cloned out so the borrow of the mixer ends before the
                    // result is pushed back into it.
                    let composed = {
                        let frames: Vec<Option<&Frame>> =
                            (0..mixer.input_count()).map(|i| mixer.input_frame(i)).collect();
                        compositor.render(&scene, &frames).clone()
                    };
                    let _ = mixer.push_frame(slot.mixer_input, composed);
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

        // ---- graphics ------------------------------------------------------
        // Each slot walks towards its switch rather than following it.
        let step = elapsed / OVERLAY_ANIMATION_SECONDS;
        for slot in 0..4 {
            let want = if overlay_on[slot] && overlay_source[slot].is_some() { 1.0 } else { 0.0 };
            if overlay_progress[slot] < want {
                overlay_progress[slot] = (overlay_progress[slot] + step).min(want);
            } else if overlay_progress[slot] > want {
                overlay_progress[slot] = (overlay_progress[slot] - step).max(want);
            }
        }

        // The watermark occupies a reserved mixer input rather than a place
        // in the matrix: it is not a source an operator cuts to, it is
        // something that is simply always there.
        let watermark_layer = watermark.as_ref().and_then(|mark| {
            let slot = WATERMARK_INPUT;
            let _ = mixer.push_frame(slot, mark.clone());
            let (w, h) = (OUTPUT_WIDTH() as f32, OUTPUT_HEIGHT() as f32);
            // Scaled by height against the frame, keeping its own shape, so a
            // wide logo and a square one both come out the size they look.
            let height = h * watermark_scale;
            let width = height * (mark.width.max(1) as f32 / mark.height.max(1) as f32);
            let margin = w * 0.025;
            let (x, y) = match watermark_corner {
                0 => (margin, margin),
                1 => (w - width - margin, margin),
                2 => (margin, h - height - margin),
                _ => (w - width - margin, h - height - margin),
            };
            let mut layer = Layer::new(slot, Rect::new(x, y, width, height));
            layer.opacity = watermark_opacity;
            layer.preserve_aspect = true;
            Some(layer)
        });

        // The ticker is drawn once and then scrolled by reading a moving
        // window out of it. Re-rendering the letters thirty times a second
        // to move them sideways would cost more than the programme does.
        if ticker_on && ticker_strip.is_none() {
            if let Some(font) = font.as_ref() {
                ticker_strip = Some(render_ticker(
                    font,
                    &ticker_text,
                    OUTPUT_WIDTH(),
                    OUTPUT_HEIGHT(),
                    ticker_background,
                    ticker_colour,
                ));
            }
        }
        let ticker_layer = if ticker_on {
            ticker_strip.as_ref().and_then(|strip| {
                let slot = TICKER_INPUT;
                let (w, h) = (OUTPUT_WIDTH(), OUTPUT_HEIGHT());
                let band = ticker_band(h);
                ticker_offset += ticker_speed * frame_budget.as_secs_f32();
                // Wrapped so the text runs round for as long as it is wanted.
                let loop_width = (strip.width - w) as f32;
                if loop_width > 0.0 && ticker_offset >= loop_width {
                    ticker_offset -= loop_width;
                }
                let window = crop(strip, ticker_offset as usize, w, band);
                let _ = mixer.push_frame(slot, window);
                Some(Layer::new(
                    slot,
                    Rect::new(0.0, (h - band) as f32, w as f32, band as f32),
                ))
            })
        } else {
            None
        };

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
            let w = OUTPUT_WIDTH() as f32;
            let h = OUTPUT_HEIGHT() as f32;
            match layout {
                Layout::Full => {
                    scene.push(layer_for(program, Rect::full(OUTPUT_WIDTH(), OUTPUT_HEIGHT())));
                }
                Layout::Pip => {
                    scene.push(layer_for(program, Rect::full(OUTPUT_WIDTH(), OUTPUT_HEIGHT())));
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
            for slot in 0..4 {
                // Drawn while it is still on its way in or out, which is the
                // whole point of animating it.
                if overlay_progress[slot] <= 0.0 {
                    continue;
                }
                if let Some(source_index) = overlay_source[slot] {
                    if let Some(source) = sources.get(source_index) {
                        let (rect, opacity) = overlay_animation[slot].apply(
                            overlay_progress[slot],
                            overlay_mode[slot].rect(),
                            OUTPUT_WIDTH() as f32,
                        );
                        let mut layer = layer_for(source.mixer_input, rect);
                        layer.opacity = opacity;
                        scene.push(layer);
                    }
                }
            }

            // Always-on graphics, above every overlay: a watermark that
            // disappears when a lower third comes up is not a watermark, and
            // a ticker that does is not a ticker.
            // Cloned rather than moved: this closure builds both sides of a
            // transition, so it runs twice.
            if let Some(mark) = watermark_layer.clone() {
                scene.push(mark);
            }
            if let Some(strip) = ticker_layer.clone() {
                scene.push(strip);
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

        measured_bytes += encoded.len() as u64;
        let window = measured_since.elapsed().as_secs_f32();
        if window >= 2.0 {
            measured_kbps = measured_bytes as f32 * 8.0 / 1000.0 / window;
            measured_bytes = 0;
            measured_since = Instant::now();
        }

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

        // ---- let the operator hear it --------------------------------------
        // Fed every tick from the master bus, whether or not anything is being
        // streamed: hearing the show is how a fader gets ridden.
        if let Some(monitor) = &monitor {
            monitor.play(&audio.output().samples);
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
        if let (Some(progress), false) = (transition, dragging_transition) {
            let step = elapsed / transition_seconds.max(0.001);
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
                    mixer.input_frame(slot.mixer_input).map(|f| Arc::new(thumbnail(f)))
                } else {
                    cached_inputs.get(index).and_then(|i| i.thumbnail.clone())
                },
                title: match &slot.source {
                    Source::Title { style, .. } => {
                        Some((style.text.clone(), style.subtitle.clone()))
                    }
                    _ => None,
                },
                layers: match &slot.source {
                    Source::Layered { layers, .. } => Some(layers.clone()),
                    _ => None,
                },
                media: match &slot.source {
                    Source::Media(m) => Some(MediaState {
                        position_seconds: m.position_seconds(),
                        duration_seconds: m.duration_seconds(),
                        paused: m.paused(),
                    }),
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
                    Source::Layered { .. } => "Layers",
                },
            })
            .collect();
        if redraw_thumbnails {
            cached_inputs = infos.clone();
        }

        // Every frame, not every third: these are the two biggest pictures on
        // screen and the ones an operator judges the production by. Tying them
        // to the thumbnail tick ran them at ten frames a second against a
        // thirty frame stream, which looks like a fault in the source.
        cached_program = Some(Arc::new(downscale(mixer.program(), PREVIEW_WIDTH(), PREVIEW_HEIGHT())));
        let cached_preview = sources
            .get(preview_input)
            .and_then(|slot| mixer.input_frame(slot.mixer_input))
            .map(|f| Arc::new(downscale(f, PREVIEW_WIDTH(), PREVIEW_HEIGHT())));

        if let Ok(mut s) = snapshot.lock() {
            s.inputs = infos;
            s.program_input = program_input;
            s.preview_input = preview_input;
            s.transition = transition;
            s.transition_seconds = transition_seconds;
            s.program = cached_program.clone();
            s.preview = cached_preview.clone();
            s.streaming = !deliveries.is_empty();
            s.ndi_output = ndi_output.as_ref().map(|s| s.name.clone());
            s.opening = opening;
            s.monitor = monitor.as_ref().map(|m| m.device_name.clone());
            s.monitor_gain_db = monitor_gain_db;
            s.audio_gaps = monitor.as_ref().map(|m| m.starved()).unwrap_or(0);
            s.audio_dropped = monitor.as_ref().map(|m| m.dropped()).unwrap_or(0);
            s.audio_buffered = monitor.as_ref().map(|m| m.buffered_frames()).unwrap_or(0);
            s.stream_size = stream_size;
            s.stream_kbps = stream_kbps;
            s.stream_audio_kbps = stream_audio_kbps;
            s.stream_measured_kbps = measured_kbps;
            let (padded, trimmed) = sources
                .iter()
                .filter_map(|slot| match &slot.source {
                    Source::Media(m) => Some(m.audio_faults()),
                    _ => None,
                })
                .fold((0, 0), |(p, t), (a, b)| (p + a, t + b));
            s.audio_padded = padded;
            s.audio_trimmed = trimmed;
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
            s.overlay_mode = overlay_mode;
            s.overlay_animation = overlay_animation;
            s.overlay_progress = overlay_progress;
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
                    trim_db: trim.get(i).copied().unwrap_or(0.0),
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

fn open_delivery(url: &str, key: &str, audio_kbps: u32) -> anyhow::Result<Delivery> {
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
    let audio = match rhevia_output::AacEncoder::new(SAMPLE_RATE, 2, audio_kbps * 1000) {
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

/// Runs a slow device open on its own thread and posts the result back.
///
/// Named for what it is for: everything it is handed would otherwise run on
/// the thread that renders and encodes, and stop the picture while it waited.
fn open_elsewhere(
    ready: &std::sync::mpsc::Sender<Opened>,
    work: impl FnOnce() -> Opened + Send + 'static,
) {
    let sender = ready.clone();
    if std::thread::Builder::new()
        .name("rhevia-open-device".into())
        .spawn(move || {
            let _ = sender.send(work());
        })
        .is_err()
    {
        // The thread could not be started, so nothing will ever report back.
        // Said here, or the interface waits forever for an input that is not
        // coming.
        let _ = ready.send(Opened::Failed("could not start opening the device".into()));
    }
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
    if frame.width == width && frame.height == height {
        // The usual case now that the monitors are shown at full size. One
        // memcpy rather than two million four-byte ones.
        return frame.clone();
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

/// The mixer inputs the always-on graphics draw from.
///
/// Reserved at the bottom so that removing a source, which shifts every
/// index above it, can never move them.
const WATERMARK_INPUT: usize = 0;
const TICKER_INPUT: usize = 1;

/// How tall the ticker strip is.
///
/// A twelfth of the frame. Broadcast straps sit near this; taller starts
/// covering the shot, shorter and the words are unreadable on a phone.
fn ticker_band(height: usize) -> usize {
    (height / 12).max(24)
}

/// Draws the ticker text once, wider than the frame, ready to be scrolled.
///
/// The text is followed by a gap and then repeated, so that reading a window
/// out of it and wrapping round produces a continuous crawl rather than a
/// message that runs off and leaves an empty bar behind it.
fn render_ticker(
    font: &rhevia_engine::FontVec,
    text: &str,
    width: usize,
    height: usize,
    background: [u8; 3],
    colour: [u8; 3],
) -> Frame {
    let band = ticker_band(height);
    let message = text.trim();
    // Enough copies to fill something wider than the frame, so there is
    // always text arriving from the right.
    let estimated = (message.chars().count() as f32 * band as f32 * 0.5) as usize;
    let one = estimated.max(width / 2) + width / 4;
    let copies = (width * 2 / one.max(1)).max(2) + 1;

    let strip_width = one * copies;
    let mut strip = Frame::filled(strip_width, band, background);

    let style = TitleStyle {
        text: message.to_string(),
        subtitle: String::new(),
        size: 0.62,
        colour,
        subtitle_colour: colour,
        background,
        background_alpha: 0,
        lower_third: false,
        accent: background,
    };
    let piece = rhevia_engine::render_title(font, &style, one, band);

    for copy in 0..copies {
        let at = copy * one;
        for y in 0..band {
            let from = y * one * 4;
            let to = y * strip_width * 4 + at * 4;
            let run = one.min(strip_width - at) * 4;
            // The rendered piece is drawn over the background rather than
            // replacing it, so the bar stays solid behind the letters.
            for i in 0..run {
                let alpha = piece.data[from + (i / 4) * 4 + 3] as u32;
                if i % 4 == 3 {
                    strip.data[to + i] = 255;
                } else if alpha > 0 {
                    let over = piece.data[from + i] as u32;
                    let under = strip.data[to + i] as u32;
                    strip.data[to + i] =
                        ((over * alpha + under * (255 - alpha)) / 255) as u8;
                }
            }
        }
    }
    strip
}

/// A window out of a wider frame, wrapping round at the end.
fn crop(source: &Frame, from_x: usize, width: usize, height: usize) -> Frame {
    let mut out = Frame::new(width, height);
    if source.is_empty() || source.width == 0 {
        return out;
    }
    let rows = height.min(source.height);
    for y in 0..rows {
        for x in 0..width {
            let sx = (from_x + x) % source.width;
            let s = (y * source.width + sx) * 4;
            let d = (y * width + x) * 4;
            out.data[d..d + 4].copy_from_slice(&source.data[s..s + 4]);
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

    /// Driving the real engine, the way the interface does.
    ///
    /// Everything else tests a piece: that ffmpeg decodes a file, that the
    /// mixer sums channels, that the compositor draws a layer. None of that
    /// answers the only question an operator has — put a file in, does it
    /// come out on the programme with its sound on a fader. This sends the
    /// same commands the buttons send and reads the same snapshot the
    /// interface draws.
    mod through_the_engine {
        use super::*;
        use std::time::{Duration, Instant};

        /// One engine at a time.
        ///
        /// Each of these starts a real engine compositing a real 1080p
        /// programme. Run a dozen at once and every one of them is starved,
        /// which turns any test with a deadline into a coin toss -- a
        /// transition that should take a second takes four, and the test
        /// that was measuring the transition reports a fault that is not
        /// there. Taking turns costs wall clock and buys results that mean
        /// something.
        static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

        /// An engine, and the turn it is taking.
        ///
        /// Derefs to the handle so the tests read as though they held one.
        struct Solo {
            engine: EngineHandle,
            // Dropped after the engine, which is what releases the turn.
            _turn: std::sync::MutexGuard<'static, ()>,
        }

        impl std::ops::Deref for Solo {
            type Target = EngineHandle;
            fn deref(&self) -> &EngineHandle {
                &self.engine
            }
        }

        /// Starts an engine, waiting for any other test's engine to finish.
        fn start() -> Solo {
            // Poisoned only means an earlier test panicked while holding it,
            // which says nothing about whether this one can run.
            let turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
            Solo { engine: super::super::start(), _turn: turn }
        }

            /// Whether tests are allowed to make a noise on this machine's speakers.
            ///
            /// Several tests here prove something that can only be proved by playing
            /// sound through a real device and listening to what comes back. Running
            /// them is the right thing to do before shipping; running them every time
            /// anyone types `cargo test` means beeps and tones out of the speakers of
            /// whoever is sitting at the machine, which is exactly what happened.
            ///
            /// Set `RHEVIA_AUDIBLE_TESTS=1` to run them.
            fn may_make_a_noise() -> bool {
                std::env::var("RHEVIA_AUDIBLE_TESTS").is_ok_and(|v| v != "0")
            }

        fn have_ffmpeg() -> bool {
            rhevia_media::available()
        }

        /// Builds a clip with picture and a tone.
        /// A test clip, built once and kept.
        ///
        /// These are half a minute of 1080p H.264 each, and several tests
        /// want one. Re-encoding them on every run put four ffmpeg processes
        /// on the machine at once, which starved the engines the tests were
        /// measuring and turned every deadline into a coin toss. The recipe
        /// is written down beside the file so that changing a clip's
        /// arguments still rebuilds it.
        fn fixture(name: &str, extra: &[&str]) -> std::path::PathBuf {
            let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/engine-media-tests");
            std::fs::create_dir_all(&dir).ok();
            let path = dir.join(name);
            let recipe = dir.join(format!("{name}.recipe"));
            let wanted = extra.join(" ");

            let usable = path.metadata().map(|m| m.len() > 0).unwrap_or(false)
                && std::fs::read_to_string(&recipe).map(|r| r == wanted).unwrap_or(false);
            if usable {
                return path;
            }

            // One at a time, so several tests asking for their clip at once
            // do not all start an encoder.
            let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
            // Checked again: another test may have built it while this one
            // was waiting for its turn.
            let usable = path.metadata().map(|m| m.len() > 0).unwrap_or(false)
                && std::fs::read_to_string(&recipe).map(|r| r == wanted).unwrap_or(false);
            if usable {
                return path;
            }

            let mut command = std::process::Command::new("ffmpeg");
            command.args(["-hide_banner", "-loglevel", "error", "-y"]);
            command.args(extra);
            let status = command.arg(&path).status().expect("ffmpeg should run");
            assert!(status.success(), "could not build {name}");
            let _ = std::fs::write(&recipe, wanted);
            path
        }

        /// Waits for the snapshot to satisfy `ready`, or gives up.
        fn wait_for(
            engine: &EngineHandle,
            within: Duration,
            ready: impl Fn(&Snapshot) -> bool,
        ) -> Option<Snapshot> {
            let deadline = Instant::now() + within;
            while Instant::now() < deadline {
                let snapshot = engine.snapshot();
                if ready(&snapshot) {
                    return Some(snapshot);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            None
        }

        #[test]
        fn a_video_file_becomes_an_input_that_reaches_the_programme() {
            if !have_ffmpeg() {
                eprintln!("SKIP: ffmpeg not installed");
                return;
            }

            let clip = fixture(
                "engine-clip.mp4",
                &[
                    "-f", "lavfi", "-i", "testsrc=size=640x360:rate=25",
                    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
                    "-t", "4",
                    "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                    "-c:a", "aac",
                ],
            );

            let engine = start();
            engine.send(Command::AddMediaSource {
                name: "Clip".into(),
                path: clip.to_str().unwrap().to_string(),
            });

            // ---- it appears as an input ---------------------------------
            let snapshot = wait_for(&engine, Duration::from_secs(20), |s| {
                s.inputs.iter().any(|i| i.name == "Clip")
            })
            .expect("the clip never became an input");

            let index = snapshot.inputs.iter().position(|i| i.name == "Clip").unwrap();
            assert_eq!(
                snapshot.inputs[index].kind, "Media",
                "a video file should be listed as Media"
            );

            // ---- it produces a picture -----------------------------------
            let snapshot = wait_for(&engine, Duration::from_secs(20), |s| {
                s.inputs
                    .get(index)
                    .and_then(|i| i.thumbnail.as_ref())
                    .map(|t| !t.is_empty())
                    .unwrap_or(false)
            })
            .expect("the clip produced no picture");

            let thumbnail = snapshot.inputs[index].thumbnail.as_ref().unwrap();
            let first = thumbnail.data.first().copied().unwrap_or(0);
            assert!(
                thumbnail.data.iter().any(|&b| b != first),
                "the clip's picture is a flat colour, so nothing was decoded into it"
            );

            // ---- it can be put on air ------------------------------------
            engine.send(Command::CutTo(index));
            let snapshot = wait_for(&engine, Duration::from_secs(10), |s| {
                s.program_input == index && s.program.as_ref().map(|f| !f.is_empty()).unwrap_or(false)
            })
            .expect("the clip never reached the programme");

            let programme = snapshot.program.as_ref().unwrap();
            let first = programme.data.first().copied().unwrap_or(0);
            assert!(
                programme.data.iter().any(|&b| b != first),
                "the programme is a flat colour with the clip on air"
            );

            // ---- its sound reaches a fader -------------------------------
            // The channel is the one named after the input, and it has to
            // show level: a clip that plays silently is the failure that
            // looks most like success.
            let heard = wait_for(&engine, Duration::from_secs(20), |s| {
                s.audio
                    .iter()
                    .find(|c| c.name == "Clip")
                    .map(|c| c.peak_db > -40.0)
                    .unwrap_or(false)
            });
            let heard = heard.expect("the clip's audio never reached its channel");
            let channel = heard.audio.iter().find(|c| c.name == "Clip").unwrap();
            eprintln!("  clip on air, its channel at {:.1} dB", channel.peak_db);

            // ---- and it reaches the master -------------------------------
            assert!(
                heard.master.peak_db > -40.0,
                "the clip's sound reached its own channel but not the master: {:.1} dB",
                heard.master.peak_db
            );
        }

        #[test]
        fn both_monitors_show_the_production_at_full_size_and_full_rate() {
            // What "the display quality is very worst" turned out to be. The
            // Preview monitor was fed the input's 320x180 thumbnail and shown
            // at the same size as Program, so a sharp source arrived on screen
            // as a blur; and both monitors were refreshed on the thumbnail
            // tick, one frame in three, so a thirty frame production was
            // watched at ten.
            if !have_ffmpeg() {
                eprintln!("SKIP: ffmpeg not installed");
                return;
            }

            let clip = fixture(
                "sharp.mp4",
                &[
                    "-f", "lavfi", "-i", "testsrc=size=1280x720:rate=30",
                    "-t", "8",
                    "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                ],
            );

            let engine = start();
            engine.send(Command::AddMediaSource {
                name: "Sharp".into(),
                path: clip.to_str().unwrap().to_string(),
            });

            let up = wait_for(&engine, Duration::from_secs(25), |s| {
                s.inputs.iter().any(|i| i.name == "Sharp")
            })
            .expect("the clip never became an input");
            let index = up.inputs.iter().position(|i| i.name == "Sharp").unwrap();

            // Armed in Preview, which is where a newly added input is judged.
            engine.send(Command::SetPreview(index));

            let (want_w, want_h) = output_size();
            let shown = wait_for(&engine, Duration::from_secs(10), |s| {
                s.preview.as_ref().is_some_and(|f| !f.is_empty())
            })
            .expect("the Preview monitor never got a picture");

            let preview = shown.preview.as_ref().unwrap();
            eprintln!(
                "  production {want_w}x{want_h}, preview {}x{}, thumbnail {}x{}",
                preview.width,
                preview.height,
                shown.inputs[index].thumbnail.as_ref().map_or(0, |f| f.width),
                shown.inputs[index].thumbnail.as_ref().map_or(0, |f| f.height),
            );
            assert_eq!(
                (preview.width, preview.height),
                (want_w, want_h),
                "Preview is not being shown at production size"
            );
            assert!(
                preview.width > THUMBNAIL_WIDTH,
                "Preview is still being fed a thumbnail"
            );

            // And both change every tick rather than every third one. Frames
            // are shared, so a new picture is a new allocation: comparing the
            // pointers says whether anything actually arrived.
            let mut program_changes = 0;
            let mut preview_changes = 0;
            let mut ticks = 0;
            let mut last: (usize, usize) = (0, 0);
            // Measured against what the engine actually rendered in the same
            // window, not against thirty. The claim is "the monitors refresh
            // as often as the programme is composited", and on an
            // unoptimised build that is a good deal slower than thirty --
            // which says nothing about whether the monitors keep up with it.
            let rendered_before = engine.snapshot().stats.frames_rendered;
            let started = Instant::now();
            let deadline = Instant::now() + Duration::from_secs(4);
            while Instant::now() < deadline && ticks < 40 {
                let s = engine.snapshot();
                let now = (
                    s.program.as_ref().map_or(0, |f| Arc::as_ptr(f) as usize),
                    s.preview.as_ref().map_or(0, |f| Arc::as_ptr(f) as usize),
                );
                if now.0 != 0 && now != last {
                    if now.0 != last.0 {
                        program_changes += 1;
                    }
                    if now.1 != last.1 {
                        preview_changes += 1;
                    }
                    last = now;
                    ticks += 1;
                }
                std::thread::sleep(Duration::from_millis(12));
            }

            let rendered = engine.snapshot().stats.frames_rendered - rendered_before;
            eprintln!(
                "  program {program_changes} new pictures, preview {preview_changes},                  against {rendered} composited in {:.1}s",
                started.elapsed().as_secs_f32()
            );

            // Sampling at roughly the frame rate cannot catch every frame, so
            // this only has to rule out the one-in-three it used to be. Half
            // of what was composited is comfortably above a third and
            // comfortably below everything.
            let expected = (rendered / 2).max(4);
            assert!(
                program_changes as u64 >= expected,
                "the Program monitor refreshed {program_changes} times while the engine                  composited {rendered} pictures"
            );
            assert!(
                preview_changes as u64 >= expected,
                "the Preview monitor refreshed {preview_changes} times while the engine                  composited {rendered} pictures"
            );
        }

        /// The operator's own file, when it is on this machine.
        ///
        /// A generated fixture proves the pipeline; it does not prove the
        /// thing that was actually reported. Set `RHEVIA_TEST_CLIP` to a real
        /// file to check it end to end — picture, sound and cost.
        #[test]
        fn a_real_clip_plays_with_its_sound_and_at_its_size() {
            if !may_make_a_noise() {
                eprintln!("SKIP: would play sound; set RHEVIA_AUDIBLE_TESTS=1 to run it");
                return;
            }
            let Ok(path) = std::env::var("RHEVIA_TEST_CLIP") else {
                eprintln!("SKIP: set RHEVIA_TEST_CLIP to a file to check it");
                return;
            };
            if !std::path::Path::new(&path).exists() {
                eprintln!("SKIP: {path} is not on this machine");
                return;
            }

            let engine = start();
            engine.send(Command::AddMediaSource { name: "Clip".into(), path: path.clone() });

            let up = wait_for(&engine, Duration::from_secs(30), |s| {
                s.inputs.iter().any(|i| i.name == "Clip")
            })
            .expect("the file never became an input");
            let index = up.inputs.iter().position(|i| i.name == "Clip").unwrap();

            engine.send(Command::SetPreview(index));
            engine.send(Command::Cut);

            // Picture on air at production size, and sound at the master.
            let live = wait_for(&engine, Duration::from_secs(25), |s| {
                s.program_input == index
                    && s.program.as_ref().is_some_and(|f| !f.is_empty())
                    && s.master.peak_db > -50.0
            })
            .expect("the clip did not reach air with its sound");

            let picture = live.program.as_ref().unwrap();
            eprintln!(
                "  {} on air at {}x{}, master {:.1} dB, listening on {}",
                std::path::Path::new(&path).file_name().unwrap().to_string_lossy(),
                picture.width,
                picture.height,
                live.master.peak_db,
                live.monitor.as_deref().unwrap_or("nothing"),
            );

            assert_eq!((picture.width, picture.height), output_size());
            assert!(
                live.master.peak_db > -50.0,
                "the file's sound never reached the master: {:.1} dB",
                live.master.peak_db
            );

            // Not silently costing more than the machine has. Measured rather
            // than assumed: raising the production size and the monitor rate
            // both cost real work.
            std::thread::sleep(Duration::from_secs(3));
            let s = engine.snapshot().stats;
            eprintln!(
                "  {:.1} fps, {:.2} ms a frame ({:.0}% of the budget)",
                s.fps,
                s.render_ms,
                s.render_ms / (1000.0 / TARGET_FPS) * 100.0
            );
            // Only held to the frame rate when built the way it ships.
            // Compositing two million pixels a frame in an unoptimised build
            // is an order of magnitude slower and says nothing useful.
            if cfg!(debug_assertions) {
                eprintln!("  (debug build — the frame rate is not judged here)");
            } else {
                assert!(
                    s.fps > TARGET_FPS * 0.9,
                    "the production is dropping frames: {:.1} fps",
                    s.fps
                );
            }
        }

        #[test]
        fn a_clip_can_be_held_cued_and_skipped_from_the_interface() {
            // The controls an operator expects on anything playable: a walk-in
            // video has to be stopped at the top and taken when the service
            // starts, not wherever it has looped round to.
            if !have_ffmpeg() {
                eprintln!("SKIP: ffmpeg not installed");
                return;
            }

            let clip = fixture(
                "transport.mp4",
                &[
                    "-f", "lavfi", "-i", "testsrc=size=640x360:rate=30",
                    "-t", "30",
                    "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                    "-g", "15",
                ],
            );

            let engine = start();
            engine.send(Command::AddMediaSource {
                name: "Walk-in".into(),
                path: clip.to_str().unwrap().to_string(),
            });

            let up = wait_for(&engine, Duration::from_secs(30), |s| {
                s.inputs.iter().any(|i| i.media.is_some_and(|m| m.position_seconds > 0.2))
            })
            .expect("the clip never started playing");
            let index = up.inputs.iter().position(|i| i.media.is_some()).unwrap();
            let state = |s: &Snapshot| s.inputs[index].media.unwrap();

            assert_eq!(
                state(&up).duration_seconds.map(|d| d.round()),
                Some(30.0),
                "the length of the file did not reach the interface"
            );

            // ---- pause holds it -----------------------------------------
            engine.send(Command::MediaTransport { input: index, action: MediaAction::PlayPause });
            let held = wait_for(&engine, Duration::from_secs(5), |s| state(s).paused)
                .expect("pause never took");
            let at = state(&held).position_seconds;
            std::thread::sleep(Duration::from_millis(1200));
            let after = state(&engine.snapshot()).position_seconds;
            eprintln!("  paused at {at:.2}s, still {after:.2}s a second later");
            assert!((after - at).abs() < 0.2, "it kept playing while held");

            // ---- and lets go --------------------------------------------
            engine.send(Command::MediaTransport { input: index, action: MediaAction::PlayPause });
            let moving = wait_for(&engine, Duration::from_secs(5), |s| {
                !state(s).paused && state(s).position_seconds > at + 0.2
            })
            .expect("letting go did not restart it");
            let running_at = state(&moving).position_seconds;

            // ---- five seconds on ----------------------------------------
            engine.send(Command::MediaTransport { input: index, action: MediaAction::Forward });
            let forward = wait_for(&engine, Duration::from_secs(20), |s| {
                state(s).position_seconds >= running_at + 4.0
            })
            .expect("skipping forward did nothing");
            let forward_at = state(&forward).position_seconds;
            eprintln!("  {running_at:.2}s -> +5s -> {forward_at:.2}s");

            // ---- and five back ------------------------------------------
            engine.send(Command::MediaTransport { input: index, action: MediaAction::Back });
            let back = wait_for(&engine, Duration::from_secs(20), |s| {
                state(s).position_seconds < forward_at - 3.0
            })
            .expect("skipping back did nothing");
            eprintln!("  {forward_at:.2}s -> -5s -> {:.2}s", state(&back).position_seconds);

            // ---- stop cues it at the top --------------------------------
            engine.send(Command::MediaTransport { input: index, action: MediaAction::Stop });
            let stopped = wait_for(&engine, Duration::from_secs(20), |s| {
                state(s).paused && state(s).position_seconds < 0.5
            })
            .expect("stop did not cue the clip at the beginning");
            eprintln!(
                "  stopped at {:.2}s, held: {}",
                state(&stopped).position_seconds,
                state(&stopped).paused
            );

            // ---- and the production never stopped running ---------------
            let s = engine.snapshot().stats;
            eprintln!("  engine still at {:.1} fps through all of that", s.fps);
            // Judged only in an optimised build, where the production runs at
            // rate to begin with. Compositing two million pixels a frame
            // unoptimised is a third of that before anything else happens.
            if !cfg!(debug_assertions) {
                assert!(
                    s.fps > TARGET_FPS * 0.8,
                    "working the transport stalled the production: {:.1} fps",
                    s.fps
                );
            }
        }

        #[test]
        fn a_camera_or_a_colour_has_no_transport() {
            // The bar is only drawn when there is something to move, and the
            // interface decides that from this being None.
            let engine = start();
            let snapshot = wait_for(&engine, Duration::from_secs(10), |s| !s.inputs.is_empty())
                .expect("the engine started with no inputs at all");
            assert!(
                snapshot.inputs.iter().all(|i| i.media.is_none()),
                "a generated input is claiming to be playable"
            );
        }

        #[test]
        fn taking_a_playing_clip_to_air_does_not_stall_the_production() {
            // Reported as lag and struggle when a playing video is taken from
            // Preview to Program and back. A transition composites both
            // arrangements in full and blends them, so it is the most
            // expensive thing the engine ever does — and it happens at the
            // exact moment an operator is watching.
            if !have_ffmpeg() {
                eprintln!("SKIP: ffmpeg not installed");
                return;
            }

            let clip = fixture(
                "cutover.mp4",
                &[
                    "-f", "lavfi", "-i", "testsrc=size=1920x1080:rate=30",
                    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
                    "-t", "30",
                    "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                    "-c:a", "aac",
                ],
            );

            let engine = start();
            engine.send(Command::AddMediaSource {
                name: "Clip".into(),
                path: clip.to_str().unwrap().to_string(),
            });
            let up = wait_for(&engine, Duration::from_secs(30), |s| {
                s.inputs.iter().any(|i| i.media.is_some_and(|m| m.position_seconds > 0.3))
            })
            .expect("the clip never started playing");
            let index = up.inputs.iter().position(|i| i.media.is_some()).unwrap();

            /// Watches the engine closely for a while and reports the worst it saw.
            fn watch(engine: &EngineHandle, seconds: f32) -> (f32, f32, f32) {
                let deadline = Instant::now() + Duration::from_secs_f32(seconds);
                let (mut worst_ms, mut worst_fps, mut total_ms, mut n) =
                    (0.0f32, f32::MAX, 0.0f32, 0u32);
                while Instant::now() < deadline {
                    let s = engine.snapshot().stats;
                    // The rate is smoothed and reads zero until it has
                    // something to smooth, which is not a stall.
                    if s.frames_rendered > 0 && s.fps > 0.0 {
                        worst_ms = worst_ms.max(s.render_ms);
                        worst_fps = worst_fps.min(s.fps);
                        total_ms += s.render_ms;
                        n += 1;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                (worst_ms, if n > 0 { worst_fps } else { 0.0 }, total_ms / n.max(1) as f32)
            }

            engine.send(Command::SetPreview(index));
            std::thread::sleep(Duration::from_millis(500));
            let (still_ms, still_fps, still_mean) = watch(&engine, 2.0);
            eprintln!(
                "  sitting in preview: {still_mean:.2} ms mean, {still_ms:.2} ms worst, \
                 {still_fps:.1} fps worst"
            );

            // A one second fade, taken the way an operator takes it.
            engine.send(Command::SetTransitionSeconds(1.0));
            engine.send(Command::Auto);
            let (fade_ms, fade_fps, fade_mean) = watch(&engine, 1.5);
            eprintln!(
                "  through the fade:   {fade_mean:.2} ms mean, {fade_ms:.2} ms worst, \
                 {fade_fps:.1} fps worst"
            );

            let live = wait_for(&engine, Duration::from_secs(5), |s| s.program_input == index)
                .expect("the clip never reached air");
            assert!(live.program.as_ref().is_some_and(|f| !f.is_empty()));

            // And back the other way, which is the half he mentioned second.
            engine.send(Command::Auto);
            let (back_ms, back_fps, back_mean) = watch(&engine, 1.5);
            eprintln!(
                "  and back again:     {back_mean:.2} ms mean, {back_ms:.2} ms worst, \
                 {back_fps:.1} fps worst"
            );

            if cfg!(debug_assertions) {
                eprintln!("  (debug build — the numbers are not judged here)");
                return;
            }

            let budget = 1000.0 / TARGET_FPS;
            for (what, worst, fps) in [
                ("sitting still", still_ms, still_fps),
                ("through the fade", fade_ms, fade_fps),
                ("coming back", back_ms, back_fps),
            ] {
                assert!(
                    worst < budget,
                    "{what}: a frame took {worst:.1} ms of a {budget:.1} ms budget"
                );
                assert!(
                    fps > TARGET_FPS * 0.9,
                    "{what}: the production dropped to {fps:.1} fps"
                );
            }
        }

        #[test]
        fn a_clip_plays_without_the_sound_card_ever_running_dry() {
            if !may_make_a_noise() {
                eprintln!("SKIP: would play sound; set RHEVIA_AUDIBLE_TESTS=1 to run it");
                return;
            }
            // Reported as "pori pori" -- a steady crackle over sound that is
            // clean in the file. The engine handed the card a fixed block per
            // tick while running at 29.7 ticks a second, so it supplied about
            // 47,600 frames a second against the 48,000 the card asked for.
            // The card filled the shortfall with silence thirty times a
            // second, and thirty tiny gaps a second is a crackle.
            if !have_ffmpeg() {
                eprintln!("SKIP: ffmpeg not installed");
                return;
            }

            let clip = fixture(
                "steady.mp4",
                &[
                    "-f", "lavfi", "-i", "testsrc=size=1280x720:rate=30",
                    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
                    "-t", "20",
                    "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                    "-c:a", "aac",
                ],
            );

            let engine = start();
            let Some(_) = wait_for(&engine, Duration::from_secs(15), |s| s.monitor.is_some())
            else {
                eprintln!("SKIP: this machine has nothing to listen on");
                return;
            };

            engine.send(Command::AddMediaSource {
                name: "Clip".into(),
                path: clip.to_str().unwrap().to_string(),
            });
            let up = wait_for(&engine, Duration::from_secs(25), |s| {
                s.audio.iter().any(|c| c.name == "Clip" && c.peak_db > -40.0)
                    && s.master.peak_db > -40.0
            })
            .expect("the clip never reached the master");
            let index = up.inputs.iter().position(|i| i.media.is_some()).unwrap();
            engine.send(Command::SetPreview(index));
            engine.send(Command::Cut);

            // Priming costs a gap or two at the very start, which is a pause
            // before the first sound rather than a fault. Measured from after.
            std::thread::sleep(Duration::from_secs(1));
            let settled = engine.snapshot().audio_gaps;

            std::thread::sleep(Duration::from_secs(6));
            let after = engine.snapshot();
            let gaps = after.audio_gaps - settled;

            eprintln!(
                "  {gaps} gaps in six seconds of playback, master {:.1} dB, on {}",
                after.master.peak_db,
                after.monitor.as_deref().unwrap_or("?")
            );
            assert!(
                after.master.peak_db > -40.0,
                "the clip stopped reaching the master: {:.1} dB",
                after.master.peak_db
            );
            // A handful over six seconds would be an occasional pause. Before
            // the clock was taken from real time this was hundreds.
            assert!(
                gaps < 10,
                "{gaps} gaps in six seconds is a crackle, not an occasional pause"
            );
        }

        /// The average colour of a rectangle of the programme, as a fraction
        /// of the frame.
        fn patch(frame: &Frame, x: f32, y: f32, w: f32, h: f32) -> [u8; 3] {
            let (fw, fh) = (frame.width as f32, frame.height as f32);
            let (x0, y0) = ((x * fw) as usize, (y * fh) as usize);
            let (x1, y1) = (((x + w) * fw) as usize, ((y + h) * fh) as usize);
            let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
            for py in y0..y1.min(frame.height) {
                for px in x0..x1.min(frame.width) {
                    let i = (py * frame.width + px) * 4;
                    r += frame.data[i] as u64;
                    g += frame.data[i + 1] as u64;
                    b += frame.data[i + 2] as u64;
                    n += 1;
                }
            }
            let n = n.max(1);
            [(r / n) as u8, (g / n) as u8, (b / n) as u8]
        }

        fn near(a: [u8; 3], b: [u8; 3], tolerance: i32) -> bool {
            (0..3).all(|c| (a[c] as i32 - b[c] as i32).abs() <= tolerance)
        }

        #[test]
        fn several_inputs_can_be_stacked_into_one() {
            // Asked for as clubbing a video and a logo together and taking
            // them to air as a single input: a blank input, then layers added
            // into it and each one placed. This builds a stack out of two
            // colours, puts it on air, and reads the programme back to check
            // each layer landed where it was put.
            let engine = start();
            let up = wait_for(&engine, Duration::from_secs(10), |s| s.inputs.len() >= 5)
                .expect("the engine started with too few inputs");
            // Input 2 is Magenta and input 4 is Amber in the starting set.
            let magenta = up.inputs.iter().position(|i| i.name == "Magenta").unwrap();
            let amber = up.inputs.iter().position(|i| i.name == "Amber").unwrap();

            engine.send(Command::AddLayeredSource { name: "Stack".into() });
            let made = wait_for(&engine, Duration::from_secs(10), |s| {
                s.inputs.iter().any(|i| i.name == "Stack")
            })
            .expect("the layered input was never made");
            let stack = made.inputs.iter().position(|i| i.name == "Stack").unwrap();
            assert_eq!(made.inputs[stack].kind, "Layers");
            assert_eq!(made.inputs[stack].layers.as_deref(), Some(&[][..]));

            // A background across the whole frame, then a box in the corner.
            engine.send(Command::AddLayer { input: stack, layer: magenta });
            engine.send(Command::AddLayer { input: stack, layer: amber });
            let two = wait_for(&engine, Duration::from_secs(10), |s| {
                s.inputs[stack].layers.as_ref().is_some_and(|l| l.len() == 2)
            })
            .expect("the layers were never added");

            let mut box_layer = two.inputs[stack].layers.as_ref().unwrap()[1];
            box_layer.x = 0.66;
            box_layer.y = 0.62;
            box_layer.width = 0.30;
            box_layer.height = 0.30;
            box_layer.preserve_aspect = false;
            engine.send(Command::SetLayer { input: stack, at: 1, layer: box_layer });

            engine.send(Command::SetPreview(stack));
            engine.send(Command::Cut);

            let live = wait_for(&engine, Duration::from_secs(10), |s| {
                s.program_input == stack && s.program.as_ref().is_some_and(|f| !f.is_empty())
            })
            .expect("the stack never reached air");
            let frame = live.program.as_ref().unwrap();

            // Top left is the background layer only; bottom right is the box.
            let background = patch(frame, 0.05, 0.05, 0.2, 0.2);
            let corner = patch(frame, 0.72, 0.68, 0.16, 0.16);
            eprintln!(
                "  background {background:?}, corner {corner:?} \
                 (magenta and amber, stacked)"
            );
            assert!(
                !near(background, corner, 24),
                "both parts of the stack are the same colour, so only one layer drew"
            );
            assert!(
                background.iter().any(|&c| c > 24),
                "the bottom layer never drew: {background:?}"
            );
            assert!(
                corner.iter().any(|&c| c > 24),
                "the top layer never drew: {corner:?}"
            );

            // Hiding the top layer leaves the background showing through.
            box_layer.visible = false;
            engine.send(Command::SetLayer { input: stack, at: 1, layer: box_layer });
            let hidden = wait_for(&engine, Duration::from_secs(5), |s| {
                s.program
                    .as_ref()
                    .is_some_and(|f| near(patch(f, 0.72, 0.68, 0.16, 0.16), background, 24))
            });
            assert!(hidden.is_some(), "hiding a layer did not take it off the stack");
        }

        #[test]
        fn a_stack_cannot_be_built_out_of_itself() {
            // It would be asked for the picture it is in the middle of
            // making. Refused where it is asked for rather than guarded in
            // the render loop, so the stack never holds a layer that is
            // silently skipped.
            let engine = start();
            engine.send(Command::AddLayeredSource { name: "Stack".into() });
            let made = wait_for(&engine, Duration::from_secs(10), |s| {
                s.inputs.iter().any(|i| i.name == "Stack")
            })
            .expect("the layered input was never made");
            let stack = made.inputs.iter().position(|i| i.name == "Stack").unwrap();

            engine.send(Command::AddLayer { input: stack, layer: stack });
            std::thread::sleep(Duration::from_millis(400));
            assert_eq!(
                engine.snapshot().inputs[stack].layers.as_ref().map(|l| l.len()),
                Some(0),
                "a stack accepted itself as one of its own layers"
            );

            // And the engine is still running rather than chasing its tail.
            // Counted in frames rather than read off the rate, which is
            // smoothed and has nothing to smooth this early.
            let before = engine.snapshot().stats.frames_rendered;
            std::thread::sleep(Duration::from_millis(400));
            let after = engine.snapshot().stats.frames_rendered;
            assert!(after > before, "the engine stopped after being handed a stack of itself");
        }

        #[test]
        fn layers_can_be_reordered_and_removed() {
            let engine = start();
            let up = wait_for(&engine, Duration::from_secs(10), |s| s.inputs.len() >= 5)
                .expect("too few inputs");
            let (a, b) = (
                up.inputs.iter().position(|i| i.name == "Magenta").unwrap(),
                up.inputs.iter().position(|i| i.name == "Amber").unwrap(),
            );

            engine.send(Command::AddLayeredSource { name: "Stack".into() });
            let made = wait_for(&engine, Duration::from_secs(10), |s| {
                s.inputs.iter().any(|i| i.name == "Stack")
            })
            .unwrap();
            let stack = made.inputs.iter().position(|i| i.name == "Stack").unwrap();

            engine.send(Command::AddLayer { input: stack, layer: a });
            engine.send(Command::AddLayer { input: stack, layer: b });
            let two = wait_for(&engine, Duration::from_secs(10), |s| {
                s.inputs[stack].layers.as_ref().is_some_and(|l| l.len() == 2)
            })
            .expect("the layers were never added");
            assert_eq!(two.inputs[stack].layers.as_ref().unwrap()[0].input, a);

            engine.send(Command::MoveLayer { input: stack, at: 0, up: true });
            let swapped = wait_for(&engine, Duration::from_secs(5), |s| {
                s.inputs[stack].layers.as_ref().is_some_and(|l| l[0].input == b)
            });
            assert!(swapped.is_some(), "raising a layer did not move it up the stack");

            engine.send(Command::RemoveLayer { input: stack, at: 0 });
            let gone = wait_for(&engine, Duration::from_secs(5), |s| {
                s.inputs[stack].layers.as_ref().is_some_and(|l| l.len() == 1)
            });
            assert!(gone.is_some(), "removing a layer left it in the stack");
        }

        #[test]
        fn an_overlay_can_be_taken_full_screen() {
            // Asked for as "1 means verse fullscreen, 2 means ndi full
            // screen": the slot decides where it sits rather than its number
            // deciding for it.
            let engine = start();
            let up = wait_for(&engine, Duration::from_secs(10), |s| s.inputs.len() >= 5)
                .expect("too few inputs");
            let amber = up.inputs.iter().position(|i| i.name == "Amber").unwrap();

            // Every slot starts full screen, which is the useful default.
            assert!(
                up.overlay_mode.iter().all(|m| *m == OverlayMode::Full),
                "overlays did not start full screen"
            );

            engine.send(Command::SetOverlaySource { slot: 0, input: amber });
            engine.send(Command::ToggleOverlay(0));
            // Waited until it has finished arriving: overlays animate on now,
            // and sampling halfway through a move measures the move.
            let covered = wait_for(&engine, Duration::from_secs(10), |s| {
                s.overlay_progress[0] >= 1.0
                    && s.program.as_ref().is_some_and(|f| !f.is_empty())
            })
            .expect("the overlay never finished coming on");
            let frame = covered.program.as_ref().unwrap();

            let middle = patch(frame, 0.4, 0.4, 0.2, 0.2);
            let edge = patch(frame, 0.02, 0.02, 0.08, 0.08);
            eprintln!("  full screen: middle {middle:?}, corner {edge:?}");
            assert!(
                near(middle, edge, 20),
                "a full-screen overlay left the programme showing at the edge"
            );

            // And moved to a corner, the programme comes back around it.
            engine.send(Command::SetOverlayMode { slot: 0, mode: OverlayMode::BottomLeft });
            let boxed = wait_for(&engine, Duration::from_secs(5), |s| {
                s.program
                    .as_ref()
                    .is_some_and(|f| !near(patch(f, 0.4, 0.4, 0.2, 0.2), middle, 20))
            });
            assert!(boxed.is_some(), "moving the overlay to a corner changed nothing");
        }

        #[test]
        #[ignore = "diagnostic; run with --ignored and RHEVIA_TEST_CLIP"]
        fn where_the_pops_in_a_real_clip_come_from() {
            if !may_make_a_noise() {
                eprintln!("SKIP: would play sound; set RHEVIA_AUDIBLE_TESTS=1 to run it");
                return;
            }
            // He can still hear pops. A pop is a discontinuity in the
            // waveform, and there are four places one can be introduced: the
            // sound card running dry, the clip's buffer being handed over
            // short, the clip's buffer being trimmed, and the master clipping.
            // This runs his own file and reports all four rather than
            // guessing which.
            let Ok(path) = std::env::var("RHEVIA_TEST_CLIP") else {
                eprintln!("SKIP: set RHEVIA_TEST_CLIP");
                return;
            };

            let engine = start();
            let Some(_) = wait_for(&engine, Duration::from_secs(15), |s| s.monitor.is_some())
            else {
                eprintln!("SKIP: nothing to listen on");
                return;
            };
            engine.send(Command::AddMediaSource { name: "Clip".into(), path });
            let up = wait_for(&engine, Duration::from_secs(30), |s| {
                s.audio.iter().any(|c| c.name == "Clip" && c.peak_db > -50.0)
            })
            .expect("the clip never made a sound");
            let index = up.inputs.iter().position(|i| i.media.is_some()).unwrap();
            engine.send(Command::SetPreview(index));
            engine.send(Command::Cut);

            std::thread::sleep(Duration::from_secs(2));
            let first = engine.snapshot();
            let (g0, p0, t0) = (first.audio_gaps, first.audio_padded, first.audio_trimmed);
            let d0 = first.audio_dropped;

            // Sampled closely so a clipping peak is not averaged away.
            let mut worst_clip = f32::MIN;
            let mut clipped_ticks = 0;
            let deadline = Instant::now() + Duration::from_secs(60);
            while Instant::now() < deadline {
                let s = engine.snapshot();
                worst_clip = worst_clip.max(s.master.peak_db);
                if s.master.clipped {
                    clipped_ticks += 1;
                }
                std::thread::sleep(Duration::from_millis(15));
            }

            let last = engine.snapshot();
            eprintln!("  over ten seconds on air:");
            eprintln!("    sound card ran dry   {:>6}", last.audio_gaps - g0);
            eprintln!("    cushion thrown away  {:>6}", last.audio_dropped - d0);
            eprintln!(
                "    cushion now          {:>6} frames ({:.0} ms)",
                last.audio_buffered,
                last.audio_buffered as f32 / 48_000.0 * 1000.0
            );
            eprintln!("    clip handed short    {:>6}", last.audio_padded - p0);
            eprintln!("    clip buffer trimmed  {:>6}", last.audio_trimmed - t0);
            eprintln!("    master clipped on    {clipped_ticks:>6} samples");
            eprintln!("    loudest master peak  {worst_clip:>6.1} dB");
            for channel in &last.audio {
                eprintln!(
                    "    channel {:<14} {:>6.1} dB peak, clipped: {}",
                    channel.name, channel.peak_db, channel.clipped
                );
            }
        }

        #[test]
        fn an_overlay_moves_on_rather_than_appearing() {
            // A lower third that arrives between one frame and the next looks
            // like a fault. What makes a graphic read as deliberate is that
            // it moves, so this checks it is actually somewhere in between
            // for a while rather than jumping from off to on.
            let engine = start();
            let up = wait_for(&engine, Duration::from_secs(10), |s| s.inputs.len() >= 5)
                .expect("too few inputs");
            let amber = up.inputs.iter().position(|i| i.name == "Amber").unwrap();

            engine.send(Command::SetOverlaySource { slot: 0, input: amber });
            engine.send(Command::SetOverlayAnimation {
                slot: 0,
                animation: OverlayAnimation::SlideLeft,
            });
            engine.send(Command::ToggleOverlay(0));

            // Caught partway. Sampled fast, because the whole move is under
            // half a second by design.
            let mut seen_between = false;
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                let p = engine.snapshot().overlay_progress[0];
                if p > 0.05 && p < 0.95 {
                    seen_between = true;
                }
                if p >= 1.0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(8));
            }
            assert!(seen_between, "the overlay went from off to on with no move in between");

            let settled =
                wait_for(&engine, Duration::from_secs(3), |s| s.overlay_progress[0] >= 1.0);
            if settled.is_none() {
                let s = engine.snapshot();
                panic!(
"the overlay never finished arriving. progress {:?}, on {:?},                      source {:?}, engine at {:.1} fps after {} frames -- an animation                      that does not finish on a slow engine is clocked in frames rather                      than in time",
                    s.overlay_progress,
                    s.overlay_on,
                    s.overlay_source,
                    s.stats.fps,
                    s.stats.frames_rendered,
                );
            }

            // And it leaves the same way rather than vanishing.
            engine.send(Command::ToggleOverlay(0));
            let mut leaving = false;
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                let p = engine.snapshot().overlay_progress[0];
                if p > 0.05 && p < 0.95 {
                    leaving = true;
                }
                if p <= 0.0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(8));
            }
            eprintln!("  moved on and off rather than appearing: {seen_between} / {leaving}");
            assert!(leaving, "the overlay vanished instead of leaving");
        }

        /// How dark a pixel is, for finding the ticker's bar against bright
        /// colour bars behind it.
        fn is_strip(frame: &Frame, x: usize, y: usize) -> bool {
            let i = (y * frame.width + x) * 4;
            frame.data[i] < 70 && frame.data[i + 1] < 80 && frame.data[i + 2] < 100
        }

        #[test]
        fn a_ticker_crawls_along_the_foot_of_the_frame() {
            // A strap of text that does not move is a caption. What makes it
            // a ticker is that it travels, so this reads the bottom of the
            // programme twice and checks it changed.
            let engine = start();
            let up = wait_for(&engine, Duration::from_secs(10), |s| s.inputs.len() >= 5)
                .expect("too few inputs");
            // A flat bright shot behind it. Colour bars have a dark ramp
            // along their bottom edge, which is exactly where the strip goes
            // and would be mistaken for it.
            let amber = up.inputs.iter().position(|i| i.name == "Amber").unwrap();
            engine.send(Command::SetPreview(amber));
            engine.send(Command::Cut);
            let first = wait_for(&engine, Duration::from_secs(10), |s| {
                s.program_input == amber && s.program.as_ref().is_some_and(|f| !f.is_empty())
            })
            .expect("no programme");
            let height = first.program.as_ref().unwrap().height;
            let band = height - ticker_band(height) / 2;

            engine.send(Command::SetTicker {
                text: "WELCOME TO THE SUNDAY SERVICE  ·  9952978141  ·  ICA CHENNAI".into(),
                on: true,
            });

            let with_strip = wait_for(&engine, Duration::from_secs(10), |s| {
                s.program
                    .as_ref()
                    .is_some_and(|f| (0..f.width).step_by(19).any(|x| is_strip(f, x, band)))
            })
            .expect("the ticker never appeared along the bottom");

            fn sample(frame: &Frame, y: usize) -> Vec<u8> {
                let row = y * frame.width * 4;
                (0..frame.width).step_by(7).map(|x| frame.data[row + x * 4]).collect()
            }
            let before = sample(with_strip.program.as_ref().unwrap(), band);
            std::thread::sleep(Duration::from_millis(500));
            let after = sample(engine.snapshot().program.as_ref().expect("no programme"), band);

            let moved = before.iter().zip(after.iter()).filter(|(a, b)| a != b).count();
            eprintln!("  {moved} of {} points along the strip changed", before.len());
            assert!(moved > 0, "the ticker is drawn but standing still");

            engine.send(Command::SetTicker { text: String::new(), on: false });
            let gone = wait_for(&engine, Duration::from_secs(5), |s| {
                s.program
                    .as_ref()
                    .is_some_and(|f| (0..f.width).step_by(19).all(|x| !is_strip(f, x, band)))
            });
            assert!(gone.is_some(), "the ticker stayed up after being turned off");
        }

        #[test]
        fn a_watermark_sits_over_everything_and_stays_there() {
            // A mark that disappears when the shot changes is not a
            // watermark. It is drawn above every overlay and stays put
            // through a cut.
            let engine = start();
            let up = wait_for(&engine, Duration::from_secs(10), |s| s.inputs.len() >= 5)
                .expect("too few inputs");

            let path = std::env::temp_dir().join("rhevia-watermark-test.png");
            let mut mark = image::RgbaImage::new(64, 64);
            for px in mark.pixels_mut() {
                *px = image::Rgba([255, 0, 255, 255]);
            }
            mark.save(&path).expect("could not write the test mark");

            engine.send(Command::SetWatermark { path: path.to_str().unwrap().to_string() });
            engine.send(Command::SetWatermarkLook { corner: 3, scale: 0.2, opacity: 1.0 });

            let magenta = |f: &Frame| {
                let px = patch(f, 0.88, 0.85, 0.04, 0.04);
                px[0] > 150 && px[1] < 90 && px[2] > 150
            };
            let marked = wait_for(&engine, Duration::from_secs(10), |s| {
                s.program.as_ref().is_some_and(|f| magenta(f))
            });
            assert!(marked.is_some(), "the watermark never appeared in the corner");

            let green = up.inputs.iter().position(|i| i.name == "Green").unwrap();
            engine.send(Command::SetPreview(green));
            engine.send(Command::Cut);
            wait_for(&engine, Duration::from_secs(5), |s| s.program_input == green)
                .expect("the cut never happened");
            std::thread::sleep(Duration::from_millis(250));

            let frame = engine.snapshot().program.clone().expect("no programme");
            let px = patch(&frame, 0.88, 0.85, 0.04, 0.04);
            eprintln!("  after cutting to another shot the corner is {px:?}");
            assert!(magenta(&frame), "the watermark came off when the shot changed: {px:?}");

            engine.send(Command::ClearWatermark);
            let cleared = wait_for(&engine, Duration::from_secs(5), |s| {
                s.program.as_ref().is_some_and(|f| !magenta(f))
            });
            assert!(cleared.is_some(), "the watermark stayed after being cleared");
            let _ = std::fs::remove_file(&path);
        }

        #[test]
        fn the_suite_does_not_play_anything_out_of_the_speakers() {
            // Every test here starts a real engine, and a real engine listens
            // by default -- which meant running the suite played every test
            // tone and every test clip out of the speakers of whoever was
            // sitting at the machine. It was reported, twice, as a beeping
            // noise while work was going on.
            //
            // Proving the monitor works needs real sound and is asked for
            // deliberately with RHEVIA_AUDIBLE_TESTS. Proving a transition
            // works does not.
            if std::env::var("RHEVIA_AUDIBLE_TESTS").is_ok_and(|v| v != "0") {
                eprintln!("  (audible tests were asked for, so this one does not apply)");
                return;
            }

            let engine = start();
            let up = wait_for(&engine, Duration::from_secs(10), |s| !s.inputs.is_empty())
                .expect("the engine never started");
            assert!(
                up.monitor.is_none(),
                "the engine opened {} and will play the test's sound into the room",
                up.monitor.as_deref().unwrap_or("a device")
            );

            // And it stays shut, rather than opening a moment later.
            std::thread::sleep(Duration::from_millis(600));
            assert!(
                engine.snapshot().monitor.is_none(),
                "the engine opened a playback device after starting"
            );
        }

        /// Kills the test server if the test panics.
        struct Listener(std::process::Child);
        impl Drop for Listener {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        fn free_port() -> u16 {
            std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port()
        }

        #[test]
        fn going_live_actually_sends_a_picture_and_sound() {
            // Reported as: the stream connected and nothing went out. A
            // connection that reports healthy and carries nothing is the
            // worst failure this program can have, because every indicator
            // says it is working.
            //
            // The output crate already proves its own muxing against a real
            // server. This proves the whole path an operator uses: an input,
            // GO LIVE, and an independent decoder judging what came out the
            // other end.
            if !have_ffmpeg() {
                eprintln!("SKIP: ffmpeg not installed");
                return;
            }

            let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/engine-live-tests");
            std::fs::create_dir_all(&dir).ok();
            let received = dir.join("received.flv");
            let _ = std::fs::remove_file(&received);

            let port = free_port();
            let url = format!("rtmp://127.0.0.1:{port}/live/rheviatest");

            // ffmpeg as the server, copying through unmodified, so the file
            // it writes is exactly what Rhevia sent.
            let _server = Listener(
                std::process::Command::new("ffmpeg")
                    .args(["-hide_banner", "-loglevel", "error", "-listen", "1", "-i"])
                    .arg(&url)
                    .args(["-c", "copy", "-y"])
                    .arg(&received)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .expect("ffmpeg should start"),
            );
            // The server needs to be listening before anything connects.
            std::thread::sleep(Duration::from_millis(600));

            let clip = fixture(
                "live.mp4",
                &[
                    "-f", "lavfi", "-i", "testsrc=size=1280x720:rate=30",
                    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
                    "-t", "20",
                    "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                    "-c:a", "aac",
                ],
            );

            let engine = start();
            engine.send(Command::AddMediaSource {
                name: "Clip".into(),
                path: clip.to_str().unwrap().to_string(),
            });
            let up = wait_for(&engine, Duration::from_secs(30), |s| {
                s.inputs.iter().any(|i| i.media.is_some())
            })
            .expect("the clip never became an input");
            let index = up.inputs.iter().position(|i| i.media.is_some()).unwrap();
            engine.send(Command::SetPreview(index));
            engine.send(Command::Cut);
            wait_for(&engine, Duration::from_secs(10), |s| s.program_input == index)
                .expect("the clip never reached air");

            // Given the way an operator is given them: the ingest address in
            // one box and the stream key in another, exactly as YouTube and
            // Facebook hand them out. Joining the two is Rhevia's job, and
            // getting it wrong produces a stream that connects and carries
            // nothing -- which is what was reported.
            engine.send(Command::StartStream {
                url: format!("rtmp://127.0.0.1:{port}/live"),
                key: "rheviatest".into(),
            });
            let live = wait_for(&engine, Duration::from_secs(20), |s| s.streaming)
                .expect("the stream never came up");
            eprintln!(
                "  connected to {}",
                live.destinations.first().map(|d| d.address.as_str()).unwrap_or("?")
            );

            // Long enough for several seconds of programme to go out.
            std::thread::sleep(Duration::from_secs(8));
            let sending = engine.snapshot();
            let bytes = sending.destinations.first().map(|d| d.bytes_sent).unwrap_or(0);
            eprintln!(
                "  {bytes} bytes sent, {} frames encoded, {} error",
                sending.stats.frames_encoded,
                sending.stream_error.as_deref().unwrap_or("no")
            );

            engine.send(Command::StopStream);
            std::thread::sleep(Duration::from_secs(1));
            drop(engine);
            // Let the server finish writing its file.
            std::thread::sleep(Duration::from_secs(2));
            drop(_server);
            std::thread::sleep(Duration::from_millis(500));

            assert!(bytes > 0, "the stream connected and sent nothing at all");
            // Judged against what the engine composited in the same time, not
            // against a number. An unoptimised build encodes 1080p far slower
            // than thirty a second, which says nothing about whether the
            // stream is carrying what is being made.
            let rendered = sending.stats.frames_rendered;
            assert!(
                sending.stats.frames_encoded * 2 >= rendered.min(240),
                "{} pictures were composited and only {} of them were encoded",
                rendered,
                sending.stats.frames_encoded
            );

            // ---- what actually arrived ----------------------------------
            let probe = std::process::Command::new("ffprobe")
                .args([
                    "-hide_banner", "-loglevel", "error",
                    "-show_entries", "stream=codec_type,codec_name,width,height",
                    "-of", "default=noprint_wrappers=1",
                ])
                .arg(&received)
                .output()
                .expect("ffprobe should run");
            let described = String::from_utf8_lossy(&probe.stdout).to_string();
            let size = std::fs::metadata(&received).map(|m| m.len()).unwrap_or(0);
            eprintln!("  the server wrote {size} bytes; ffprobe says:");
            for line in described.lines() {
                eprintln!("    {line}");
            }

            assert!(size > 0, "the server received nothing");
            assert!(
                described.contains("codec_type=video"),
                "no video arrived at the other end:\n{described}"
            );
            assert!(
                described.contains("codec_type=audio"),
                "no sound arrived at the other end:\n{described}"
            );

            // And it decodes, rather than merely being labelled.
            let decode = std::process::Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-i"])
                .arg(&received)
                .args(["-f", "null", "-"])
                .output()
                .expect("ffmpeg should run");
            let complaints = String::from_utf8_lossy(&decode.stderr).to_string();
            assert!(
                complaints.trim().is_empty(),
                "what arrived does not decode cleanly:\n{complaints}"
            );
        }

        #[test]
        fn a_quiet_source_can_be_brought_up_with_the_input_gain() {
            // Asked for as: the input audio is low and I want to raise it.
            // The fader cannot do this -- it balances one source against
            // another and stops at +6 on the scale it is drawn on, which is
            // nowhere near enough for a line output plugged into a
            // microphone input. The input gain is a separate control with a
            // separate range, and it starts at nothing.
            if !have_ffmpeg() {
                eprintln!("SKIP: ffmpeg not installed");
                return;
            }

            // Deliberately quiet: a tone at a fortieth of full scale, which
            // is about -32 dB and the sort of level a phone on a stand gives.
            let quiet = fixture(
                "quiet.mp4",
                &[
                    "-f", "lavfi", "-i", "testsrc=size=640x360:rate=30",
                    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
                    "-filter:a", "volume=0.025",
                    "-t", "20",
                    "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                    "-c:a", "aac",
                ],
            );

            let engine = start();
            engine.send(Command::AddMediaSource {
                name: "Quiet".into(),
                path: quiet.to_str().unwrap().to_string(),
            });
            let up = wait_for(&engine, Duration::from_secs(30), |s| {
                s.audio.iter().any(|c| c.name == "Quiet" && c.peak_db > -60.0)
            })
            .expect("the quiet clip never made a sound at all");
            let channel = up.audio.iter().position(|c| c.name == "Quiet").unwrap();

            assert_eq!(
                up.audio[channel].trim_db, 0.0,
                "the input gain must start at nothing"
            );

            // Settled, so the reading is the clip rather than its first block.
            std::thread::sleep(Duration::from_secs(1));
            let before = engine.snapshot().audio[channel].peak_db;
            assert!(
                before < -20.0,
                "the test clip is not quiet enough to be worth the test: {before:.1} dB"
            );

            engine.send(Command::SetChannelTrim { channel, db: 20.0 });
            let raised = wait_for(&engine, Duration::from_secs(10), |s| {
                s.audio[channel].peak_db > before + 15.0
            })
            .expect("raising the input gain did not raise the level");

            let after = raised.audio[channel].peak_db;
            eprintln!(
                "  {before:.1} dB with the gain at nothing, {after:.1} dB with it at +20"
            );
            assert!(
                (after - before - 20.0).abs() < 4.0,
                "asking for twenty decibels gave {:.1}",
                after - before
            );
            assert_eq!(raised.audio[channel].trim_db, 20.0);

            // And it reaches the master, which is what actually goes out.
            assert!(
                raised.master.peak_db > -40.0,
                "the raised channel did not reach the master: {:.1} dB",
                raised.master.peak_db
            );

            // Turned back down, it comes back down.
            engine.send(Command::SetChannelTrim { channel, db: 0.0 });
            let back = wait_for(&engine, Duration::from_secs(10), |s| {
                s.audio[channel].peak_db < before + 5.0
            });
            assert!(back.is_some(), "turning the input gain down did not lower the level");
        }

        #[test]
        fn the_input_gain_will_not_be_pushed_past_what_it_offers() {
            // A control that silently accepts a number it will not honour is
            // worse than one that refuses it, because the operator then
            // believes the level is set.
            let engine = start();
            wait_for(&engine, Duration::from_secs(10), |s| !s.audio.is_empty())
                .expect("no channels");

            engine.send(Command::SetChannelTrim { channel: 0, db: 500.0 });
            let capped = wait_for(&engine, Duration::from_secs(5), |s| s.audio[0].trim_db != 0.0)
                .expect("the gain never moved");
            assert_eq!(capped.audio[0].trim_db, TRIM_MAX_DB);

            engine.send(Command::SetChannelTrim { channel: 0, db: -500.0 });
            let floored = wait_for(&engine, Duration::from_secs(5), |s| {
                s.audio[0].trim_db == TRIM_MIN_DB
            });
            assert!(floored.is_some(), "the gain was not held at its floor");
        }

        #[test]
        fn a_video_file_is_audible_without_being_asked_to_be() {
            if !may_make_a_noise() {
                eprintln!("SKIP: would play sound; set RHEVIA_AUDIBLE_TESTS=1 to run it");
                return;
            }
            // The complaint that mattered most: a video is added and nothing
            // is heard. Monitoring existed but had to be found and switched
            // on, which is not a preference — it is the program appearing
            // broken. This checks the engine is listening from the start and
            // that the device is really taking the sound.
            if !have_ffmpeg() {
                eprintln!("SKIP: ffmpeg not installed");
                return;
            }

            let clip = fixture(
                "audible.mp4",
                &[
                    "-f", "lavfi", "-i", "testsrc=size=640x360:rate=25",
                    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
                    "-t", "6",
                    "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                    "-c:a", "aac",
                ],
            );

            let engine = start();

            // Listening without anyone asking for it.
            let ready = wait_for(&engine, Duration::from_secs(15), |s| s.monitor.is_some());
            let Some(ready) = ready else {
                eprintln!("SKIP: this machine has nothing to listen on");
                return;
            };
            eprintln!("  listening on {}", ready.monitor.as_deref().unwrap_or("?"));

            engine.send(Command::AddMediaSource {
                name: "Clip".into(),
                path: clip.to_str().unwrap().to_string(),
            });

            // The clip's own channel carries sound, and so does the master —
            // which is what the monitor is fed from.
            let heard = wait_for(&engine, Duration::from_secs(25), |s| {
                s.audio.iter().any(|c| c.name == "Clip" && c.peak_db > -40.0)
                    && s.master.peak_db > -40.0
            })
            .expect("the clip never reached the master");

            eprintln!(
                "  clip {:.1} dB, master {:.1} dB, still listening: {}",
                heard.audio.iter().find(|c| c.name == "Clip").unwrap().peak_db,
                heard.master.peak_db,
                heard.monitor.is_some()
            );
            assert!(heard.monitor.is_some(), "the monitor stopped");
        }

        #[test]
        fn an_audio_file_becomes_a_channel_with_no_picture() {
            if !have_ffmpeg() {
                eprintln!("SKIP: ffmpeg not installed");
                return;
            }

            let song = fixture(
                "engine-song.mp3",
                &["-f", "lavfi", "-i", "sine=frequency=330:sample_rate=44100", "-t", "4"],
            );

            let engine = start();
            engine.send(Command::AddMediaSource {
                name: "Walk-in".into(),
                path: song.to_str().unwrap().to_string(),
            });

            let snapshot = wait_for(&engine, Duration::from_secs(20), |s| {
                s.inputs.iter().any(|i| i.name == "Walk-in")
            })
            .expect("the song never became an input");

            let index = snapshot.inputs.iter().position(|i| i.name == "Walk-in").unwrap();
            assert_eq!(
                snapshot.inputs[index].kind, "Audio File",
                "a file with no picture should say so rather than claiming to be video"
            );

            let heard = wait_for(&engine, Duration::from_secs(20), |s| {
                s.audio
                    .iter()
                    .find(|c| c.name == "Walk-in")
                    .map(|c| c.peak_db > -40.0)
                    .unwrap_or(false)
            })
            .expect("the song never made a sound");

            let channel = heard.audio.iter().find(|c| c.name == "Walk-in").unwrap();
            eprintln!("  song playing at {:.1} dB", channel.peak_db);
        }

        #[test]
        fn several_media_inputs_run_at_once_each_on_its_own_channel() {
            if !have_ffmpeg() {
                eprintln!("SKIP: ffmpeg not installed");
                return;
            }

            // What an actual show looks like: a clip and a music bed, each
            // with its own fader.
            let clip = fixture(
                "engine-two-a.mp4",
                &[
                    "-f", "lavfi", "-i", "testsrc=size=320x180:rate=25",
                    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
                    "-t", "4",
                    "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                    "-c:a", "aac",
                ],
            );
            let bed = fixture(
                "engine-two-b.mp3",
                &["-f", "lavfi", "-i", "sine=frequency=220:sample_rate=48000", "-t", "4"],
            );

            let engine = start();
            engine.send(Command::AddMediaSource {
                name: "Clip".into(),
                path: clip.to_str().unwrap().to_string(),
            });
            engine.send(Command::AddMediaSource {
                name: "Bed".into(),
                path: bed.to_str().unwrap().to_string(),
            });

            let heard = wait_for(&engine, Duration::from_secs(25), |s| {
                let clip = s.audio.iter().find(|c| c.name == "Clip");
                let bed = s.audio.iter().find(|c| c.name == "Bed");
                matches!((clip, bed), (Some(a), Some(b)) if a.peak_db > -40.0 && b.peak_db > -40.0)
            })
            .expect("the two files did not both reach their own channels");

            // Separate channels, which is what makes them mixable.
            let clip = heard.audio.iter().find(|c| c.name == "Clip").unwrap();
            let bed = heard.audio.iter().find(|c| c.name == "Bed").unwrap();
            eprintln!("  clip {:.1} dB, bed {:.1} dB", clip.peak_db, bed.peak_db);
            assert_eq!(heard.audio.iter().filter(|c| c.name == "Clip").count(), 1);
            assert_eq!(heard.audio.iter().filter(|c| c.name == "Bed").count(), 1);
        }

        #[test]
        fn a_desktop_capture_becomes_an_input_and_keeps_running() {
            // Adding a desktop capture from the interface took the whole
            // program down, so this drives the same command and then keeps
            // rendering for a while: a source that is a different size from
            // the programme goes through a different path in the compositor.
            let Ok(monitors) = rhevia_capture::monitors() else {
                eprintln!("SKIP: no monitors");
                return;
            };
            let Some(target) = monitors.into_iter().next() else {
                eprintln!("SKIP: no monitors");
                return;
            };
            eprintln!("  capturing {} at {}x{}", target.name, target.width, target.height);

            let engine = start();
            engine.send(Command::AddScreenSource { name: "Desktop".into(), target });

            let snapshot = wait_for(&engine, Duration::from_secs(20), |s| {
                s.inputs.iter().any(|i| i.name == "Desktop")
            })
            .expect("the desktop never became an input");

            let index = snapshot.inputs.iter().position(|i| i.name == "Desktop").unwrap();

            // On air, so it goes through the compositor and the encoder rather
            // than only the thumbnail path.
            engine.send(Command::CutTo(index));
            let on_air = wait_for(&engine, Duration::from_secs(20), |s| {
                s.program_input == index
                    && s.program.as_ref().map(|f| !f.is_empty()).unwrap_or(false)
            })
            .expect("the desktop never reached the programme");

            // The snapshot carries a reduced copy for the interface to draw,
            // so what matters is that it is a real picture of the right shape
            // rather than that it is the full programme size.
            let programme = on_air.program.as_ref().unwrap();
            assert!(programme.width > 0 && programme.height > 0);
            let aspect = programme.width as f32 / programme.height as f32;
            assert!(
                (aspect - 16.0 / 9.0).abs() < 0.01,
                "the programme came back {}x{}, which is not 16:9",
                programme.width,
                programme.height
            );

            // Kept running: a crash showed up a moment after the input
            // appeared, not at the moment it was added.
            std::thread::sleep(Duration::from_secs(4));
            assert!(engine.is_running(), "the engine stopped with a desktop capture on air");

            let later = engine.snapshot();
            assert!(
                later.stats.frames_rendered > on_air.stats.frames_rendered,
                "rendering stopped with a desktop capture on air"
            );
            eprintln!("  still rendering: {} frames", later.stats.frames_rendered);
        }

        #[test]
        fn a_file_that_does_not_exist_is_reported_rather_than_added() {
            let engine = start();
            engine.send(Command::AddMediaSource {
                name: "Missing".into(),
                path: "no-such-file-98765.mp4".into(),
            });

            let snapshot = wait_for(&engine, Duration::from_secs(10), |s| {
                s.stream_error.is_some()
            })
            .expect("adding a file that is not there said nothing");

            assert!(
                !snapshot.inputs.iter().any(|i| i.name == "Missing"),
                "a file that could not be opened was added anyway"
            );
            eprintln!("  reported: {}", snapshot.stream_error.unwrap());
        }
    }

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
