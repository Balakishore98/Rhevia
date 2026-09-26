//! Rhevia Studio's interface.
//!
//! Laid out the way a switcher is operated, following
//! `stitch_live_stream_studio_alternative/`. Telemetry across the top because
//! it is glanced at constantly; Preview and Program with the transition bus
//! between them because that is the axis your hands work along; the input
//! matrix beneath, every tile carrying its own controls so "cut to three"
//! needs no selection step; audio along the bottom where a mixer belongs.
//!
//! Everything shown here is measured. No reading is a placeholder — an
//! interface that invents numbers is worse than one that omits them, because
//! an operator will believe it during a show.

use std::collections::HashMap;

use eframe::egui::{self, Rect, RichText, Rounding, Stroke, Vec2};
use rhevia_engine::Frame;

use crate::audio_ui;
use crate::engine::{self, Command, EngineHandle, Layout, MediaAction, Snapshot};
use rhevia_engine::Transition;
use crate::theme;

/// What the production is running at, asked of the engine rather than
/// assumed: it is the operator's choice now.
fn target_fps() -> f32 {
    engine::TARGET_FPS()
}

/// The strip under each monitor: the position bar, the buttons and the gaps.
const TRANSPORT_HEIGHT: f32 = 33.0;

/// What the production is running at, as it is written on the monitors.
///
/// Read from the engine rather than written into the interface. It said
/// "1280x720p30" on every tile no matter what the production was, which is
/// the kind of label an operator trusts and should not have to.
fn canvas_label() -> String {
    let (w, h) = engine::output_size();
    format!("{w}x{h}p{}", target_fps() as u32)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Switcher,
    Audio,
    Stream,
    Settings,
}

impl Tab {
    const ALL: [(Tab, &'static str); 4] = [
        (Tab::Switcher, "Switcher & Feeds"),
        (Tab::Audio, "Audio Mixer & DSP"),
        (Tab::Stream, "Stream & Output"),
        (Tab::Settings, "Settings"),
    ];
}

/// Filters for the input matrix.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    WithAudio,
    VideoOnly,
}

pub struct StudioApp {
    engine: EngineHandle,
    /// Each monitor's texture, with which frame is already on it.
    textures: HashMap<String, (egui::TextureHandle, usize)>,
    tab: Tab,
    filter: Filter,
    rtmp_url: String,
    stream_key: String,
    record_path: String,
    new_source_name: String,
    file_path: String,
    image_path: String,
    title_text: String,
    title_subtitle: String,
    /// The title input being edited, with its working copy of the text.
    editing_title: Option<(usize, String, String)>,
    /// The input whose settings dialog is open.
    settings_for: Option<usize>,
    /// Working copy of that input's name, so typing does not fight the engine.
    settings_name: String,
    show_add_source: bool,
    show_devices: bool,
    /// Which type of input the select dialog is showing.
    input_tab: InputTab,
    /// Devices are listed when the dialog opens and on demand, never per
    /// repaint: enumerating every window on the desktop sixty times a second
    /// costs more than the dialog is worth.
    cached_cameras: Vec<rhevia_capture::CameraTarget>,
    cached_monitors: Vec<rhevia_capture::Target>,
    cached_windows: Vec<rhevia_capture::Target>,
    cached_audio: Vec<rhevia_audio::AudioDevice>,
    cached_ndi: Vec<rhevia_ndi::NdiSource>,
    /// Installed plugins, each already checked in a separate process.
    cached_plugins: Vec<rhevia_plugin::PluginInfo>,
    /// Whether a scan has been attempted, so an empty result is not retried
    /// on every frame.
    plugins_scanned: bool,
    /// The mark in the menu bar, uploaded once.
    mark: Option<egui::TextureHandle>,
    /// The scan running in the background, if one is.
    ///
    /// Scanning loads every module and starts a process per plugin, which is
    /// far too slow to do on the interface thread.
    plugin_scan: Option<std::sync::mpsc::Receiver<Vec<rhevia_plugin::PluginInfo>>>,
    /// The name the programme is published under over NDI.
    ndi_output_name: String,
    /// Set when enumeration failed, so the dialog can say why rather than
    /// showing an empty list that looks like "no devices".
    device_error: Option<String>,
    /// A device scan running on a worker thread, if one is.
    device_scan: Option<std::sync::mpsc::Receiver<Devices>>,
    /// Which input a chosen device attaches to; None adds an audio-only input.
    attach_to: Option<usize>,
    /// Which overlay slot the next input click fills, if any.
    assigning_overlay: Option<usize>,
    /// Which overlay slot's arrangement is being chosen, if any.
    overlay_layout_for: Option<usize>,
    /// The channel whose DSP is shown on the audio tab.
    selected_channel: usize,
    /// Playback devices, listed when the Settings tab is first looked at
    /// and on demand. Enumerating them is a COM call per device, which is
    /// not something to do while painting sixty times a second.
    cached_playback: Vec<rhevia_audio::MonitorDevice>,
    /// A device picked from the combo box, applied after it closes.
    pending_monitor: Option<String>,
    /// A layer chosen from the combo box, applied after it closes.
    pending_layer: Option<(usize, usize)>,
    /// A display chosen from the combo box, applied after it closes. An
    /// empty string means off.
    pending_display: Option<String>,
    /// The always-on graphics, as the panel that drives them holds them.
    watermark_path: String,
    watermark_corner: usize,
    watermark_scale: f32,
    watermark_opacity: f32,
    ticker_text: String,
    ticker_on: bool,
    ticker_speed: f32,
    ticker_background: [u8; 3],
    /// How fast the window itself is being drawn.
    ///
    /// Separate from the engine's rate, and the one an operator actually
    /// sees: the production can be running at thirty and still look like a
    /// slideshow if the interface cannot keep up drawing it. Reported rather
    /// than assumed, because this is exactly the sort of thing that is easy
    /// to be wrong about and impossible to argue with once measured.
    drawn_at: Option<std::time::Instant>,
    /// Smoothed frames a second for the window.
    draw_fps: f32,
    /// Smoothed milliseconds spent inside one repaint.
    draw_ms: f32,
    /// What was chosen on the Settings tab, and written down.
    ///
    /// Held here rather than read back from disk while painting: the
    /// resolution only takes effect on the next start, so what is on screen
    /// is the choice, not what the engine is running.
    settings: crate::settings::Settings,
}

/// Everything the input dialog can offer, gathered in one go.
///
/// Collected on a worker thread and sent over as a whole, so the interface
/// never holds a half-updated list.
#[derive(Default)]
struct Devices {
    cameras: Vec<rhevia_capture::CameraTarget>,
    monitors: Vec<rhevia_capture::Target>,
    windows: Vec<rhevia_capture::Target>,
    audio: Vec<rhevia_audio::AudioDevice>,
    ndi: Vec<rhevia_ndi::NdiSource>,
    error: Option<String>,
}

/// Looks for every kind of input. Slow, and never called on the interface
/// thread.
///
/// Enumerating windows, opening the camera list and waiting on NDI discovery
/// together take about a second. Doing that while painting freezes the window,
/// which during a show looks exactly like a crash.
fn gather_devices() -> Devices {
    let mut found = Devices::default();

    // Asked here rather than while painting. It costs a whole process, and
    // the media tab used to pay for it on the interface thread every time the
    // dialog was opened — which is a visible stall on the one tab where the
    // operator is already waiting.
    rhevia_media::forget_availability();
    let _ = rhevia_media::available();

    match rhevia_capture::cameras() {
        Ok(list) => found.cameras = list,
        Err(e) => found.error = Some(e.to_string()),
    }
    match rhevia_capture::monitors() {
        Ok(list) => found.monitors = list,
        Err(e) => {
            found.error.get_or_insert_with(|| e.to_string());
        }
    }
    match rhevia_capture::windows() {
        Ok(list) => found.windows = list,
        Err(e) => {
            found.error.get_or_insert_with(|| e.to_string());
        }
    }
    found.audio = rhevia_audio::list_input_devices();

    // NDI finds sources by announcement, so a list taken immediately is
    // usually empty. Most of a second is the shortest wait that reliably sees
    // a machine that is already publishing — and the single biggest reason
    // this cannot happen on the interface thread.
    if rhevia_ndi::available() {
        match rhevia_ndi::find_sources(std::time::Duration::from_millis(800)) {
            Ok(sources) => found.ndi = sources,
            Err(e) => {
                found.error.get_or_insert_with(|| e.to_string());
            }
        }
    }

    found
}

/// How wide an input tile is, picture and controls alike.
///
/// One number for both, because they have to agree: the controls are laid out
/// under the picture and a row wider than the picture runs beneath the next
/// tile along, which makes the whole matrix look misaligned. Sized to hold
/// the five controls at their natural widths rather than the other way round.
const TILE_WIDTH: f32 = 206.0;

/// Ingest endpoints, so the common cases need no typing.
///
/// The address is the part that never changes; the key is the part that is
/// personal, so only the address is kept here.
const PRESETS: [(&str, &str); 6] = [
    ("YouTube", "rtmp://a.rtmp.youtube.com/live2"),
    ("Twitch", "rtmp://live.twitch.tv/app"),
    ("Facebook", "rtmps://live-api-s.facebook.com:443/rtmp"),
    ("Kick", "rtmps://fa723fc1b171.global-contribute.live-video.net"),
    ("Custom RTMP", "rtmp://"),
    ("SRT", "srt://127.0.0.1:9000?mode=caller&latency=120"),
];

/// The categories down the left of the input dialog.
///
/// Ordered by how often a category is reached for rather than alphabetically:
/// a camera is the first input on almost every show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputTab {
    Camera,
    Ndi,
    Display,
    Window,
    Audio,
    Media,
    Image,
    Title,
    Colour,
    Layers,
}

impl InputTab {
    const ALL: [InputTab; 10] = [
        InputTab::Camera,
        InputTab::Ndi,
        InputTab::Display,
        InputTab::Window,
        InputTab::Audio,
        InputTab::Media,
        InputTab::Image,
        InputTab::Title,
        InputTab::Colour,
        InputTab::Layers,
    ];

    fn label(self) -> &'static str {
        match self {
            InputTab::Camera => "Camera",
            InputTab::Ndi => "NDI",
            InputTab::Display => "Desktop Capture",
            InputTab::Window => "Window Capture",
            InputTab::Audio => "Audio Input",
            InputTab::Media => "Video / Media",
            InputTab::Image => "Image",
            InputTab::Title => "Title",
            InputTab::Colour => "Colour",
            InputTab::Layers => "Layers (group)",
        }
    }

    /// One line under the heading, saying what this kind of input is for.
    fn blurb(self) -> &'static str {
        match self {
            InputTab::Camera => "A webcam or capture card. Opened at its highest frame rate.",
            InputTab::Ndi => {
                "A source another machine on the network is publishing, with its sound."
            }
            InputTab::Display => "A whole monitor, captured live.",
            InputTab::Window => "A single application window, captured live.",
            InputTab::Audio => {
                "A microphone, a USB interface, or whatever the machine is playing."
            }
            InputTab::Media => "An H.264 file, played on a loop.",
            InputTab::Image => "A still: holding slide, sponsor board, stinger graphic.",
            InputTab::Title => "A lower third, rendered here rather than in another application.",
            InputTab::Colour => "A flat colour or a bar pattern, for testing and for backgrounds.",
            InputTab::Layers => {
                "An empty input that other inputs are stacked into — a video with a \
                 logo over it, taken to air as one thing. Add it, then open its SET \
                 dialog to build the stack."
            }
        }
    }
}

impl StudioApp {
    pub fn new(cc: &eframe::CreationContext<'_>, engine: EngineHandle) -> Self {
        theme::apply(&cc.egui_ctx);
        Self {
            engine,
            textures: HashMap::new(),
            tab: Tab::Switcher,
            filter: Filter::All,
            rtmp_url: "rtmp://a.rtmp.youtube.com/live2".into(),
            stream_key: String::new(),
            record_path: default_record_path(),
            new_source_name: String::new(),
            file_path: String::new(),
            image_path: String::new(),
            title_text: String::new(),
            title_subtitle: String::new(),
            editing_title: None,
            settings_for: None,
            settings_name: String::new(),
            show_add_source: false,
            show_devices: false,
            input_tab: InputTab::Camera,
            cached_cameras: Vec::new(),
            cached_monitors: Vec::new(),
            cached_windows: Vec::new(),
            cached_audio: Vec::new(),
            cached_ndi: Vec::new(),
            cached_plugins: Vec::new(),
            plugins_scanned: false,
            mark: None,
            plugin_scan: None,
            ndi_output_name: "Rhevia Programme".into(),
            device_error: None,
            device_scan: None,
            attach_to: None,
            assigning_overlay: None,
            overlay_layout_for: None,
            selected_channel: 0,
            drawn_at: None,
            draw_fps: 0.0,
            draw_ms: 0.0,
            cached_playback: Vec::new(),
            pending_monitor: None,
            pending_layer: None,
            pending_display: None,
            watermark_path: String::new(),
            watermark_corner: 1,
            watermark_scale: 0.12,
            watermark_opacity: 0.75,
            ticker_text: String::new(),
            ticker_on: false,
            ticker_speed: 120.0,
            ticker_background: [12, 18, 32],
            settings: crate::settings::Settings::load(),
        }
    }

    /// Puts a frame on the graphics card, and only when it is a new one.
    ///
    /// The engine shares frames rather than copying them, so two snapshots
    /// holding the same picture hold the same allocation — which makes the
    /// pointer a reliable answer to "has this changed?". Without that check
    /// the interface converted and uploaded eight megabytes per monitor per
    /// repaint, sixty times a second, for a picture arriving thirty times a
    /// second. Half that work was wasted and the other half was competing
    /// with the engine for the same memory bandwidth.
    /// Throws the programme onto another display, full screen and bare.
    ///
    /// A hall's projector is a second display with nothing on it but the
    /// programme -- no menus, no borders and nothing that can be clicked by
    /// accident. Drawn from the same picture the Program monitor shows, so
    /// what the congregation sees is what is going to air rather than a
    /// second render that could drift from it.
    fn display_output(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        let Some(wanted) = self.settings.output_display.clone() else { return };
        let Some(target) = self.cached_monitors.iter().find(|m| m.name == wanted).cloned()
        else {
            // Unplugged, or not looked for yet. Asked for once rather than
            // on every repaint.
            if self.cached_monitors.is_empty() {
                self.refresh_devices();
            }
            return;
        };

        let texture = snapshot
            .program
            .as_ref()
            .and_then(|f| self.texture(ctx, "display-out", f));
        let picture = snapshot
            .program
            .as_ref()
            .map(|f| (f.width as f32, f.height as f32))
            .unwrap_or((16.0, 9.0));

        let id = egui::ViewportId::from_hash_of("rhevia-display-out");
        let builder = egui::ViewportBuilder::default()
            .with_title("Rhevia Programme")
            .with_decorations(false)
            .with_position(egui::pos2(target.x as f32, target.y as f32))
            .with_inner_size(egui::vec2(target.width as f32, target.height as f32))
            .with_fullscreen(true)
            .with_taskbar(false);

        let mut closed = false;
        ctx.show_viewport_immediate(id, builder, |ctx, _| {
            egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(egui::Color32::BLACK))
                .show(ctx, |ui| {
                    if let Some(texture) = texture {
                        // Letterboxed, never stretched: a 16:9 programme on a
                        // 16:10 projector has to keep its shape, or every
                        // face on the screen is the wrong width.
                        let space = ui.available_size();
                        let scale = (space.x / picture.0).min(space.y / picture.1);
                        let at = Rect::from_center_size(
                            ui.available_rect_before_wrap().center(),
                            Vec2::new(picture.0 * scale, picture.1 * scale),
                        );
                        ui.painter().image(
                            texture,
                            at,
                            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                    }
                    // Escape gets out. A bare full-screen window on a
                    // projector with no way back is a trap.
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        closed = true;
                    }
                });
            if ctx.input(|i| i.viewport().close_requested()) {
                closed = true;
            }
        });

        if closed {
            self.settings.output_display = None;
            self.settings.save();
        }
    }

    /// Starts listening on a device, and remembers it for next time.
    fn choose_monitor(&mut self, device: Option<String>) {
        self.settings.monitor_device = device.clone();
        self.settings.monitor = true;
        self.settings.save();
        self.engine.send(Command::SetMonitor { device, on: true });
    }

    fn texture(
        &mut self,
        ctx: &egui::Context,
        key: &str,
        frame: &std::sync::Arc<Frame>,
    ) -> Option<egui::TextureId> {
        if frame.is_empty() {
            return None;
        }
        let identity = std::sync::Arc::as_ptr(frame) as usize;
        match self.textures.get_mut(key) {
            Some((handle, uploaded)) => {
                if *uploaded != identity {
                    handle.set(
                        egui::ColorImage::from_rgba_unmultiplied(
                            [frame.width, frame.height],
                            &frame.data,
                        ),
                        egui::TextureOptions::LINEAR,
                    );
                    *uploaded = identity;
                }
            }
            None => {
                let handle = ctx.load_texture(
                    key,
                    egui::ColorImage::from_rgba_unmultiplied(
                        [frame.width, frame.height],
                        &frame.data,
                    ),
                    egui::TextureOptions::LINEAR,
                );
                self.textures.insert(key.to_string(), (handle, identity));
            }
        }
        self.textures.get(key).map(|(h, _)| h.id())
    }
}

impl eframe::App for StudioApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let entered = std::time::Instant::now();
        if let Some(last) = self.drawn_at {
            let gap = entered.duration_since(last).as_secs_f32();
            if gap > 0.0 {
                // Smoothed over about half a second. An instantaneous figure
                // on a live readout is unreadable and a long average hides
                // exactly the stutter this is here to show.
                self.draw_fps += (1.0 / gap - self.draw_fps) * 0.08;
            }
        }
        self.drawn_at = Some(entered);

        let snapshot = self.engine.snapshot();
        ctx.request_repaint_after(std::time::Duration::from_millis(16));

        // The mixer's device buttons ask for the same thing the input
        // dialog already does, so they open it rather than a second dialog
        // that lists the same devices and can drift away from it.
        if self.show_devices {
            self.show_devices = false;
            self.attach_to = None;
            self.input_tab = InputTab::Audio;
            self.show_add_source = true;
            self.refresh_devices();
        }

        self.keyboard(ctx, &snapshot);
        self.dropped_files(ctx);
        self.menu_bar(ctx);
        self.status_strip(ctx, &snapshot);
        self.tab_bar(ctx);
        self.footer(ctx, &snapshot);

        // Finished scans, collected without waiting for either.
        self.collect_devices();

        // A finished plugin scan, collected without waiting for it.
        if let Some(receiver) = &self.plugin_scan {
            if let Ok(found) = receiver.try_recv() {
                self.cached_plugins = found;
                self.plugin_scan = None;
            }
        }
        let mut rescan = false;

        match self.tab {
            Tab::Switcher => {
                audio_ui::strip_row(
                    ctx,
                    &snapshot,
                    &self.engine,
                    &mut self.show_devices,
                    &mut self.selected_channel,
                );
                self.input_matrix(ctx, &snapshot);
                self.monitors(ctx, &snapshot);
            }
            Tab::Audio => audio_ui::full_view(
                ctx,
                &snapshot,
                &self.engine,
                &mut self.show_devices,
                &mut self.selected_channel,
                &self.cached_plugins,
                self.plugin_scan.is_some(),
                &mut rescan,
            ),
            Tab::Stream => self.stream_view(ctx, &snapshot),
            Tab::Settings => self.settings_view(ctx, &snapshot),
        }

        // Scanned once when the audio tab is first opened, and again whenever
        // the operator asks. Doing it at startup would delay the window for
        // seconds on a machine with a lot of plugins installed.
        let first_look = self.tab == Tab::Audio && !self.plugins_scanned;
        if (rescan || first_look) && self.plugin_scan.is_none() {
            self.plugins_scanned = true;
            self.start_plugin_scan();
        }

        self.dialogs(ctx, &snapshot);
        self.display_output(ctx, &snapshot);

        let spent = entered.elapsed().as_secs_f32() * 1000.0;
        self.draw_ms += (spent - self.draw_ms) * 0.08;
    }
}

impl StudioApp {
    fn keyboard(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        // Only while the switcher is in front: number keys belong to text
        // fields on the other tabs.
        if self.tab != Tab::Switcher {
            return;
        }
        ctx.input(|i| {
            if i.key_pressed(egui::Key::Space) {
                self.engine.send(Command::Cut);
            }
            if i.key_pressed(egui::Key::Enter) {
                self.engine.send(Command::Auto);
            }
            if i.key_pressed(egui::Key::Escape) {
                self.engine.send(Command::ToggleFtb);
            }
            for (index, key) in [
                egui::Key::Num1, egui::Key::Num2, egui::Key::Num3, egui::Key::Num4,
                egui::Key::Num5, egui::Key::Num6, egui::Key::Num7, egui::Key::Num8,
            ]
            .into_iter()
            .enumerate()
            {
                if i.key_pressed(key) && index < snapshot.inputs.len() {
                    if i.modifiers.ctrl {
                        self.engine.send(Command::CutTo(index));
                    } else {
                        self.engine.send(Command::SetPreview(index));
                    }
                }
            }
        });
    }

    fn menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu")
            .exact_height(26.0)
            .frame(theme::panel(theme::SURFACE_LOWEST))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add_space(9.0);
                    if let Some(mark) = self.mark(ui.ctx()) {
                        // Sized to the bar rather than to the image: the
                        // source is 128 across and would otherwise push the
                        // menus off the screen.
                        ui.add(
                            egui::Image::new((mark.id(), Vec2::splat(17.0)))
                                .fit_to_exact_size(Vec2::splat(17.0)),
                        );
                        ui.add_space(6.0);
                    }
                    ui.label(RichText::new("RHEVIA").size(12.0).strong().color(theme::ACCENT));
                    ui.add_space(12.0);

                    ui.menu_button(RichText::new("File").size(11.5), |ui| {
                        if ui.button("Add input…").clicked() {
                            self.show_add_source = true;
                            self.refresh_devices();
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Quit").clicked() {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                    ui.menu_button(RichText::new("Audio").size(11.5), |ui| {
                        if ui.button("Add capture device…").clicked() {
                            self.attach_to = None;
                            self.show_devices = true;
                            ui.close_menu();
                        }
                        if ui.button("Open mixer").clicked() {
                            self.tab = Tab::Audio;
                            ui.close_menu();
                        }
                    });
                    ui.menu_button(RichText::new("Output").size(11.5), |ui| {
                        if ui.button("Stream settings…").clicked() {
                            self.tab = Tab::Stream;
                            ui.close_menu();
                        }
                    });
                    ui.menu_button(RichText::new("Help").size(11.5), |ui| {
                        if ui.button("Shortcuts").clicked() {
                            self.tab = Tab::Settings;
                            ui.close_menu();
                        }
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                                .font(theme::mono(10.0))
                                .color(theme::TEXT_FAINT),
                        );
                    });
                });
            });
    }

    /// The telemetry strip. Every figure here is measured.
    fn status_strip(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        egui::TopBottomPanel::top("status")
            .exact_height(50.0)
            .frame(theme::panel(theme::SURFACE_LOW))
            .show(ctx, |ui| {
                // Width the right-hand block needs: timecode, GO LIVE, RECORD
                // and, while recording, the byte count.
                let right_width = if snapshot.recording { 490.0 } else { 390.0 };

                ui.horizontal_centered(|ui| {
                    ui.add_space(10.0);
                    let s = snapshot.stats;

                    let (label, fill) = if snapshot.ftb {
                        ("FADED TO BLACK", theme::WARN)
                    } else if snapshot.streaming {
                        ("● LIVE ON-AIR", theme::PROGRAM)
                    } else {
                        ("○ OFF AIR", theme::SURFACE_HIGH)
                    };
                    if theme::pill(ui, label, fill, Vec2::new(126.0, 30.0))
                        .on_hover_text("click to toggle fade to black")
                        .clicked()
                    {
                        self.engine.send(Command::ToggleFtb);
                    }
                    ui.add_space(14.0);
                    theme::divider(ui, 30.0);
                    ui.add_space(14.0);

                    if snapshot.streaming {
                        theme::readout(ui, "UPTIME", &format_duration(s.uptime_seconds), theme::TEXT);
                        let kbps = (s.bytes_sent * 8 / s.uptime_seconds.max(1)) / 1000;
                        theme::readout(ui, "BITRATE", &format!("{kbps} kb/s"), theme::PREVIEW);
                    } else {
                        theme::readout(ui, "UPTIME", "--:--", theme::TEXT_FAINT);
                        theme::readout(ui, "BITRATE", "--", theme::TEXT_FAINT);
                    }

                    // Engine load: how much of the frame budget compositing
                    // took. Above 100% the mixer cannot keep up, which is the
                    // figure that actually predicts dropped frames.
                    let budget_ms = 1000.0 / target_fps();
                    let load = (s.render_ms / budget_ms * 100.0).clamp(0.0, 999.0);
                    let load_colour = if load > 90.0 {
                        theme::PROGRAM
                    } else if load > 60.0 {
                        theme::WARN
                    } else {
                        theme::PREVIEW
                    };
                    theme::readout(ui, "ENGINE LOAD", &format!("{load:.0}%"), load_colour);

                    let fps_colour = if s.fps < target_fps() * 0.9 && s.fps > 0.0 {
                        theme::WARN
                    } else {
                        theme::PREVIEW
                    };
                    theme::readout(ui, "FPS", &format!("{:.1}", s.fps), fps_colour);

                    // What the operator is actually watching. The engine can
                    // be running at rate while the window draws at half of
                    // it, and then the production looks like it is stuttering
                    // when only the monitor of it is.
                    let drawn_colour = if self.draw_fps < target_fps() * 0.9 && self.draw_fps > 0.0 {
                        theme::WARN
                    } else {
                        theme::PREVIEW
                    };
                    theme::readout(
                        ui,
                        "WINDOW",
                        &format!("{:.0} fps · {:.1} ms", self.draw_fps, self.draw_ms),
                        drawn_colour,
                    );

                    // Eat the gap so the block that follows sits hard right,
                    // rather than trusting a nested layout to find the edge.
                    ui.add_space((ui.available_width() - right_width).max(0.0));

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(12.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(0.0, 1.0);
                            ui.add_space(7.0);
                            ui.label(RichText::new("TIMECODE").size(8.5).color(theme::TEXT_FAINT));
                            ui.label(
                                RichText::new(timecode(s.frames_rendered))
                                    .font(theme::mono(16.0))
                                    .color(theme::ACCENT),
                            );
                        });
                        ui.add_space(18.0);

                        let (stream_label, stream_fill) = if snapshot.streaming {
                            ("STOP STREAM", theme::PROGRAM)
                        } else {
                            ("GO LIVE", theme::PREVIEW)
                        };
                        if theme::button(ui, stream_label, stream_fill, Vec2::new(116.0, 30.0)).clicked() {
                            if snapshot.streaming {
                                self.engine.send(Command::StopStream);
                            } else {
                                self.tab = Tab::Stream;
                            }
                        }
                        ui.add_space(8.0);

                        let (rec_label, rec_fill) = if snapshot.recording {
                            ("■ STOP REC", theme::PROGRAM)
                        } else {
                            ("⏺ RECORD", theme::SURFACE_HIGH)
                        };
                        if theme::button(ui, rec_label, rec_fill, Vec2::new(106.0, 30.0)).clicked() {
                            if snapshot.recording {
                                self.engine.send(Command::StopRecording);
                            } else {
                                self.engine.send(Command::StartRecording {
                                    path: self.record_path.clone(),
                                });
                            }
                        }
                        ui.add_space(16.0);
                        if snapshot.recording {
                            theme::readout(
                                ui,
                                "RECORDED",
                                &format!("{:.1} MB", snapshot.recorded_bytes as f64 / 1_048_576.0),
                                theme::PROGRAM,
                            );
                        }
                    });
                });
            });
    }

    fn tab_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("tabs")
            .exact_height(32.0)
            .frame(theme::panel(theme::SURFACE_LOWEST))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(2.0, 0.0);
                    ui.add_space(14.0);
                    for (tab, label) in Tab::ALL {
                        if theme::tab(ui, label, self.tab == tab).clicked() {
                            self.tab = tab;
                        }
                    }
                });
            });
    }

    fn footer(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        egui::TopBottomPanel::bottom("footer")
            .exact_height(24.0)
            .frame(theme::panel(theme::SURFACE_LOWEST))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add_space(10.0);
                    let (dot, text) = if self.engine.is_running() {
                        (theme::PREVIEW, "engine running")
                    } else {
                        (theme::PROGRAM, "ENGINE STOPPED")
                    };
                    ui.label(RichText::new("●").size(10.0).color(dot));
                    ui.label(RichText::new(text).size(10.5).color(theme::TEXT_FAINT));
                    ui.add_space(14.0);
                    ui.label(
                        RichText::new(format!(
                            "rendered {}   ·   encoded {}   ·   render {:.1} ms",
                            snapshot.stats.frames_rendered,
                            snapshot.stats.frames_encoded,
                            snapshot.stats.render_ms
                        ))
                        .font(theme::mono(10.0))
                        .color(theme::TEXT_FAINT),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(10.0);
                        if let Some(error) = &snapshot.stream_error {
                            ui.label(RichText::new(format!("⚠ {error}")).size(10.5).color(theme::PROGRAM));
                        } else if let Some(slot) = self.assigning_overlay {
                            ui.label(
                                RichText::new(format!("click an input to fill overlay {}", slot + 1))
                                    .size(10.5)
                                    .color(theme::ACCENT),
                            );
                        } else {
                            ui.label(
                                RichText::new("1-8 preview · ctrl+1-8 cut · space CUT · enter AUTO · esc FTB")
                                    .size(10.5)
                                    .color(theme::TEXT_FAINT),
                            );
                        }
                    });
                });
            });
    }

    fn input_matrix(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        egui::TopBottomPanel::bottom("matrix")
            .exact_height(214.0)
            .frame(theme::panel(theme::SURFACE_LOW))
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new("LIVE PRODUCTION INPUT MATRIX")
                            .size(11.0)
                            .strong()
                            .color(theme::TEXT),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(format!("{} ACTIVE", snapshot.inputs.len()))
                            .font(theme::mono(9.5))
                            .color(theme::TEXT_FAINT),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(12.0);
                        if snapshot.opening > 0 {
                            ui.label(
                                RichText::new(if snapshot.opening == 1 {
                                    "opening…".to_string()
                                } else {
                                    format!("opening {}…", snapshot.opening)
                                })
                                .size(10.0)
                                .color(theme::ACCENT),
                            );
                            ui.add_space(8.0);
                        }
                        if theme::button(ui, "+ ADD INPUT", theme::ACCENT, Vec2::new(104.0, 22.0)).clicked() {
                            self.show_add_source = true;
                            self.refresh_devices();
                        }
                        ui.add_space(6.0);
                        if theme::button(ui, "AUDIO SETTINGS", theme::SURFACE_HIGH, Vec2::new(122.0, 22.0))
                            .on_hover_text(
                                "EQ, compressor, gate and delay — each channel has its own; \
                                 click a strip below, or an input's AUD button, to choose one",
                            )
                            .clicked()
                        {
                            self.tab = Tab::Audio;
                        }
                        ui.add_space(10.0);
                        for (filter, label) in [
                            (Filter::All, "ALL"),
                            (Filter::WithAudio, "WITH AUDIO"),
                            (Filter::VideoOnly, "VIDEO ONLY"),
                        ] {
                            let active = self.filter == filter;
                            if theme::chip(ui, label, active, theme::ACCENT, theme::chip_size(ui, label, 20.0)).clicked() {
                                self.filter = filter;
                            }
                            ui.add_space(3.0);
                        }
                    });
                });
                ui.add_space(6.0);

                egui::ScrollArea::horizontal().id_salt("matrix").show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        for index in 0..snapshot.inputs.len() {
                            if !self.passes_filter(snapshot, index) {
                                continue;
                            }
                            self.input_tile(ctx, ui, snapshot, index);
                            ui.add_space(8.0);
                        }
                    });
                });
            });
    }

    fn passes_filter(&self, snapshot: &Snapshot, index: usize) -> bool {
        let has_audio = snapshot.audio.get(index).map(|c| c.has_source).unwrap_or(false);
        match self.filter {
            Filter::All => true,
            Filter::WithAudio => has_audio,
            Filter::VideoOnly => !has_audio,
        }
    }

    fn input_tile(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        index: usize,
    ) {
        let info = &snapshot.inputs[index];
        let on_program = index == snapshot.program_input;
        let on_preview = index == snapshot.preview_input;
        let texture = info
            .thumbnail
            .clone()
            .and_then(|f| self.texture(ctx, &format!("input{index}"), &f));

        let detail = match snapshot.audio.get(index) {
            Some(c) if c.has_source => {
                format!("{} · AUD {:+.0}dB", canvas_label(), c.gain_db)
            }
            _ => format!("{} · no audio", canvas_label()),
        };

        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);

            let picture = theme::input_picture(
                ui,
                index + 1,
                &info.name,
                &detail,
                texture,
                on_program,
                on_preview,
                Vec2::new(TILE_WIDTH, 116.0),
            );
            if picture.clicked() {
                if let Some(slot) = self.assigning_overlay.take() {
                    self.engine.send(Command::SetOverlaySource { slot, input: index });
                } else {
                    self.engine.send(Command::SetPreview(index));
                }
            }

            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);
                ui.set_width(TILE_WIDTH);
                if theme::chip(ui, "CUT", false, theme::PROGRAM, Vec2::new(38.0, 20.0))
                    .on_hover_text("cut this input straight to air")
                    .clicked()
                {
                    self.engine.send(Command::CutTo(index));
                }
                if theme::chip(ui, "PVW", on_preview, theme::PREVIEW, Vec2::new(38.0, 20.0))
                    .on_hover_text("arm this input in Preview")
                    .clicked()
                {
                    self.engine.send(Command::SetPreview(index));
                }
                // Straight to this input's own audio settings. Every input
                // has its own channel, and without a route from the input
                // itself an operator has to guess which strip to select.
                if theme::chip(ui, "AUD", false, theme::ACCENT, Vec2::new(38.0, 20.0))
                    .on_hover_text("this input's own EQ, compressor, gate and delay")
                    .clicked()
                {
                    self.selected_channel = index;
                    self.tab = Tab::Audio;
                }
                let adjusted = !info.settings.is_default();
                if theme::chip(
                    ui,
                    if adjusted { "SET *" } else { "SET" },
                    adjusted,
                    theme::ACCENT,
                    Vec2::new(38.0, 20.0),
                )
                .on_hover_text(if adjusted {
                    "name, position, zoom and colour — this input has been adjusted"
                } else {
                    "name, position, zoom and colour"
                })
                .clicked()
                {
                    self.settings_for = Some(index);
                    self.settings_name = info.name.clone();
                }
                if theme::chip(ui, "CLOSE", false, theme::WARN, Vec2::new(46.0, 20.0))
                    .on_hover_text("remove this input")
                    .clicked()
                    && snapshot.inputs.len() > 1
                {
                    self.engine.send(Command::RemoveSource(index));
                }
            });

            // The overlay row is labelled: four bare numbers next to CUT and
            // PVW read as something else entirely.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);
                ui.label(
                    RichText::new("OVL")
                        .font(theme::mono(9.5))
                        .color(theme::TEXT_FAINT),
                );
                for slot in 0..4 {
                    let assigned = snapshot.overlay_source[slot] == Some(index);
                    let live = assigned && snapshot.overlay_on[slot];
                    let colour = if live { theme::PROGRAM } else { theme::ACCENT };
                    if theme::chip(ui, &format!("{}", slot + 1), assigned, colour, Vec2::new(32.0, 20.0))
                        .on_hover_text(if live {
                            format!("overlay {} is on air — click to take it off", slot + 1)
                        } else {
                            format!("put this input on air as overlay {}", slot + 1)
                        })
                        .clicked()
                    {
                        self.engine.send(Command::SetOverlaySource { slot, input: index });
                        self.engine.send(Command::ToggleOverlay(slot));
                    }
                }
            });

            // A lower third is edited constantly during a show, so the way in
            // sits on the tile rather than behind a settings dialog.
            if let Some((text, subtitle)) = &info.title {
                if theme::chip(ui, "EDIT TEXT", false, theme::ACCENT, Vec2::new(186.0, 19.0))
                    .on_hover_text("change this title without taking it off air")
                    .clicked()
                {
                    self.editing_title = Some((index, text.clone(), subtitle.clone()));
                }
            }
            ui.horizontal(|_ui| {
            });
        });
    }

    fn monitors(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        egui::CentralPanel::default()
            .frame(theme::panel(theme::SURFACE))
            .show(ctx, |ui| {
                let available = ui.available_size();
                let bus = 152.0;
                let gap = 8.0;
                // Room kept below each monitor for its transport, whether
                // or not the input showing has one. Giving the space back
                // when it is empty would make the pictures jump every time a
                // clip was armed, which during a show reads as a fault.
                let monitor = Vec2::new(
                    ((available.x - bus - gap * 4.0) / 2.0).max(180.0),
                    (available.y - gap * 2.0 - TRANSPORT_HEIGHT).max(150.0),
                );

                ui.add_space(gap);
                // Top aligned: the transition bus between the two pictures is
                // taller than they are, and a centring layout slides Program
                // down relative to Preview — the two things an operator most
                // needs level with each other.
                ui.horizontal_top(|ui| {
                    ui.add_space(gap);

                    let preview_texture = snapshot
                        .preview
                        .as_ref()
                        .and_then(|f| self.texture(ctx, "preview", f));
                    let preview_name = snapshot
                        .inputs
                        .get(snapshot.preview_input)
                        .map(|i| i.name.as_str())
                        .unwrap_or("—");
                    ui.vertical(|ui| {
                    theme::monitor(
                        ui,
                        "PREVIEW",
                        preview_name,
                        &format!(
                            "IN {}   ·   {}   ·   NEXT",
                            snapshot.preview_input + 1,
                            canvas_label()
                        ),
                        Some("PVW"),
                        preview_texture,
                        theme::PREVIEW,
                        monitor,
                    );
                    self.transport(ui, snapshot, snapshot.preview_input, monitor.x);
                    });

                    ui.add_space(gap);
                    self.transition_bus(ui, snapshot, bus);
                    ui.add_space(gap);

                    let program_texture = snapshot
                        .program
                        .as_ref()
                        .and_then(|f| self.texture(ctx, "program", f));
                    let program_name = snapshot
                        .inputs
                        .get(snapshot.program_input)
                        .map(|i| i.name.as_str())
                        .unwrap_or("—");
                    let overlays: Vec<String> = (0..4)
                        .filter(|&s| snapshot.overlay_on[s])
                        .map(|s| format!("OVL{}", s + 1))
                        .collect();
                    let footer = if overlays.is_empty() {
                        format!("LAYOUT {}   ·   {}", snapshot.layout.label(), canvas_label())
                    } else {
                        format!("LAYOUT {}   ·   {}", snapshot.layout.label(), overlays.join(" "))
                    };
                    ui.vertical(|ui| {
                    theme::monitor(
                        ui,
                        "PROGRAM",
                        program_name,
                        &footer,
                        Some(if snapshot.streaming { "ON AIR" } else { "PGM" }),
                        program_texture,
                        theme::PROGRAM,
                        monitor,
                    );
                    self.transport(ui, snapshot, snapshot.program_input, monitor.x);
                    });
                });
            });
    }

    /// The transport for one input, under its monitor.
    ///
    /// Only drawn for a file: a camera has nowhere to skip to. The space is
    /// still taken when there is nothing to draw, so arming a clip does not
    /// shove both pictures up the screen.
    fn transport(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot, index: usize, width: f32) {
        let Some(media) = snapshot.inputs.get(index).and_then(|i| i.media) else {
            ui.add_space(TRANSPORT_HEIGHT);
            return;
        };

        ui.add_space(4.0);
        theme::position_bar(ui, media.position_seconds, media.duration_seconds, width);
        ui.add_space(3.0);

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(4.0, 0.0);

            let press = |ui: &mut egui::Ui, label: &str, hint: &str, w: f32, on: bool| {
                theme::chip(ui, label, on, theme::ACCENT, Vec2::new(w, 20.0))
                    .on_hover_text(hint)
                    .clicked()
            };

            if press(ui, "STOP", "stop, and cue back at the beginning", 42.0, false) {
                self.engine.send(Command::MediaTransport {
                    input: index,
                    action: MediaAction::Stop,
                });
            }
            if press(ui, "\u{00AB} 5s", "back five seconds", 46.0, false) {
                self.engine.send(Command::MediaTransport {
                    input: index,
                    action: MediaAction::Back,
                });
            }
            // Wider and lit while playing: this is the one an operator finds
            // without looking, in the dark, during a service.
            let playing = !media.paused;
            if press(
                ui,
                if playing { "PAUSE" } else { "PLAY" },
                if playing { "hold the clip where it is" } else { "let the clip carry on" },
                58.0,
                playing,
            ) {
                self.engine.send(Command::MediaTransport {
                    input: index,
                    action: MediaAction::PlayPause,
                });
            }
            if press(ui, "5s \u{00BB}", "on five seconds", 46.0, false) {
                self.engine.send(Command::MediaTransport {
                    input: index,
                    action: MediaAction::Forward,
                });
            }

            ui.add_space(4.0);
            ui.label(
                RichText::new(match media.duration_seconds {
                    Some(d) => format!(
                        "{} / {}",
                        theme::clock(media.position_seconds),
                        theme::clock(d)
                    ),
                    // No length means a stream, where counting down from
                    // nothing would be a lie.
                    None => theme::clock(media.position_seconds),
                })
                .font(theme::mono(10.5))
                .color(if playing { theme::TEXT } else { theme::TEXT_DIM }),
            );
        });
    }

    fn transition_bus(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot, width: f32) {
        ui.vertical(|ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing = Vec2::new(4.0, 4.0);
            ui.add_space(4.0);

            ui.label(RichText::new("TRANSITION BUS").size(9.0).strong().color(theme::TEXT_FAINT));

            if theme::button(ui, "CUT", theme::PROGRAM, Vec2::new(width, 36.0)).clicked() {
                self.engine.send(Command::Cut);
            }
            let auto_colour = if snapshot.transition.is_some() {
                theme::ACCENT
            } else {
                theme::SURFACE_HIGH
            };
            let auto_label = format!("AUTO {}", snapshot.transition_kind.label());
            if theme::button(ui, &auto_label, auto_colour, Vec2::new(width, 36.0)).clicked() {
                self.engine.send(Command::Auto);
            }

            ui.add_space(2.0);
            ui.label(RichText::new("EFFECT").size(9.0).strong().color(theme::TEXT_FAINT));
            // Twenty-three effects will not fit as buttons beside the
            // monitors, so the bus carries the current one and opens the rest,
            // which is how every switcher presents them.
            ui.menu_button(
                RichText::new(format!("{}   [change]", snapshot.transition_kind.label())).size(11.0),
                |ui| {
                    ui.set_min_width(190.0);
                    for effect in Transition::ALL {
                        if effect == Transition::Cut {
                            // Cut has its own button; listing it here too
                            // would offer an AUTO that does not animate.
                            continue;
                        }
                        let active = snapshot.transition_kind == effect;
                        let label = RichText::new(effect.label())
                            .size(11.5)
                            .color(if active { theme::ACCENT } else { theme::TEXT });
                        if ui.selectable_label(active, label).clicked() {
                            self.engine.send(Command::SetTransition(effect));
                            ui.close_menu();
                        }
                    }
                    ui.separator();
                    ui.label(
                        RichText::new("Duration Milliseconds")
                            .size(10.5)
                            .strong()
                            .color(theme::TEXT_DIM),
                    );
                    let mut ms = snapshot.transition_seconds * 1000.0;
                    if ui
                        .add(egui::DragValue::new(&mut ms).speed(10.0).range(100.0..=10_000.0))
                        .changed()
                    {
                        self.engine.send(Command::SetTransitionMs(ms));
                    }
                },
            );

            ui.add_space(2.0);
            ui.label(RichText::new("LAYOUT").size(9.0).strong().color(theme::TEXT_FAINT));
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);
                for option in Layout::ALL {
                    let active = snapshot.layout == option;
                    if theme::chip(ui, option.label(), active, theme::ACCENT, Vec2::new(71.0, 20.0)).clicked() {
                        self.engine.send(Command::SetLayout(option));
                    }
                }
            });

            ui.add_space(2.0);
            ui.label(RichText::new("OVERLAY").size(9.0).strong().color(theme::TEXT_FAINT));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);
                for slot in 0..4 {
                    let assigned = snapshot.overlay_source[slot].is_some();
                    let on = snapshot.overlay_on[slot];
                    let colour = if on { theme::PROGRAM } else { theme::ACCENT };
                    let response =
                        theme::chip(ui, &format!("{}", slot + 1), on, colour, Vec2::new(29.0, 24.0));
                    if response.clicked() {
                        if assigned {
                            self.engine.send(Command::ToggleOverlay(slot));
                        } else {
                            self.assigning_overlay = Some(slot);
                        }
                    }
                    // Right-click opens where it sits, and closes it again, so
                    // the row is there when it is wanted and gone when it is
                    // not.
                    if response.secondary_clicked() {
                        self.overlay_layout_for =
                            if self.overlay_layout_for == Some(slot) { None } else { Some(slot) };
                    }
                    response.on_hover_text(match snapshot.overlay_source[slot] {
                        Some(input) => format!(
                            "{} — {}, {}. Click to take it on air; right-click to change where it sits.",
                            snapshot
                                .inputs
                                .get(input)
                                .map(|i| i.name.as_str())
                                .unwrap_or("?"),
                            snapshot.overlay_mode[slot].label(),
                            snapshot.overlay_mode[slot].hint(),
                        ),
                        None => "click, then pick an input".to_string(),
                    });
                }
            });

            // Where each slot draws. Shown for the slot being worked on
            // rather than all four at once: four rows of five chips in this
            // column would push the T-bar off the screen.
            if let Some(slot) = self.overlay_layout_for.filter(|s| *s < 4) {
                ui.add_space(3.0);
                ui.label(
                    RichText::new(format!("OVL {} SITS", slot + 1))
                        .size(8.5)
                        .color(theme::TEXT_FAINT),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);
                    for mode in engine::OverlayMode::ALL {
                        let chosen = snapshot.overlay_mode[slot] == mode;
                        if theme::chip(
                            ui,
                            mode.label(),
                            chosen,
                            theme::PREVIEW,
                            Vec2::new(60.0, 20.0),
                        )
                        .on_hover_text(mode.hint())
                        .clicked()
                        {
                            self.engine.send(Command::SetOverlayMode { slot, mode });
                        }
                    }
                });
                ui.add_space(3.0);
                ui.label(
                    RichText::new(format!("OVL {} MOVES", slot + 1))
                        .size(8.5)
                        .color(theme::TEXT_FAINT),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);
                    for animation in engine::OverlayAnimation::ALL {
                        let chosen = snapshot.overlay_animation[slot] == animation;
                        if theme::chip(
                            ui,
                            animation.label(),
                            chosen,
                            theme::ACCENT,
                            Vec2::new(60.0, 20.0),
                        )
                        .on_hover_text(animation.hint())
                        .clicked()
                        {
                            self.engine
                                .send(Command::SetOverlayAnimation { slot, animation });
                        }
                    }
                });
            }

            ui.add_space(6.0);
            ui.label(RichText::new("T-BAR").size(8.5).color(theme::TEXT_FAINT));
            ui.add_space(2.0);
            let bar = theme::vertical_t_bar(
                ui,
                snapshot.transition.unwrap_or(0.0),
                                // Sized to leave room for fade to black underneath. That is
                // the control that has to work when everything else has gone
                // wrong, so it does not get pushed off the bottom.
                Vec2::new(width, 112.0),
            );
            if let Some(progress) = bar.dragged {
                self.engine.send(Command::SetTransitionProgress(progress));
            }
            if bar.released {
                self.engine.send(Command::ReleaseTransition);
            }

            ui.label(
                RichText::new(format!("{:.0} ms", snapshot.transition_seconds * 1000.0))
                    .font(theme::mono(9.5))
                    .color(theme::TEXT_FAINT),
            );
            let mut seconds = snapshot.transition_seconds;
            if ui
                .add_sized(
                    Vec2::new(width, 16.0),
                    egui::Slider::new(&mut seconds, 0.2..=5.0).show_value(false),
                )
                .changed()
            {
                self.engine.send(Command::SetTransitionSeconds(seconds));
            }

            if let Some(slot) = snapshot.transition_kind.stinger_slot() {
                let ready = snapshot.overlay_source[slot].is_some();
                ui.label(
                    RichText::new(if ready {
                        format!("via overlay {}", slot + 1)
                    } else {
                        format!("set overlay {}", slot + 1)
                    })
                    .size(9.0)
                    .color(if ready { theme::PREVIEW } else { theme::WARN }),
                );
            }

            ui.add_space(4.0);
            let ftb_colour = if snapshot.ftb { theme::WARN } else { theme::SURFACE_HIGH };
            if theme::button(ui, "FADE TO BLACK", ftb_colour, Vec2::new(width, 28.0)).clicked() {
                self.engine.send(Command::ToggleFtb);
            }
        });
    }

    /// The two graphics that are simply always there.
    ///
    /// A watermark and a ticker are not sources an operator cuts to and not
    /// overlays they take on and off. They belong to the programme itself,
    /// which is why they live here rather than in the input matrix.
    fn graphics(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        ui.label(RichText::new("ON-SCREEN GRAPHICS").size(12.0).strong().color(theme::TEXT));
        ui.add_space(10.0);

        // ---- watermark ----------------------------------------------------
        ui.label(RichText::new("WATERMARK").size(10.5).strong().color(theme::TEXT_DIM));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if theme::button(ui, "Choose image", theme::ACCENT, Vec2::new(116.0, 24.0))
                .on_hover_text("a PNG with transparency is what a logo should be")
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Image", &["png", "jpg", "jpeg", "webp", "bmp", "gif"])
                    .pick_file()
                {
                    if let Some(path) = path.to_str() {
                        self.watermark_path = path.to_string();
                        self.engine
                            .send(Command::SetWatermark { path: path.to_string() });
                    }
                }
            }
            if !self.watermark_path.is_empty() {
                if theme::chip(ui, "OFF", false, theme::WARN, Vec2::new(40.0, 22.0))
                    .on_hover_text("take the watermark off the programme")
                    .clicked()
                {
                    self.watermark_path.clear();
                    self.engine.send(Command::ClearWatermark);
                }
                ui.label(
                    RichText::new(
                        std::path::Path::new(&self.watermark_path)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default(),
                    )
                    .font(theme::mono(10.0))
                    .color(theme::TEXT_DIM),
                );
            }
        });

        if !self.watermark_path.is_empty() {
            ui.add_space(6.0);
            let mut changed = false;
            ui.horizontal(|ui| {
                ui.label(RichText::new("CORNER").font(theme::mono(9.5)).color(theme::TEXT_FAINT));
                for (index, label) in
                    ["UPPER L", "UPPER R", "LOWER L", "LOWER R"].into_iter().enumerate()
                {
                    if theme::chip(
                        ui,
                        label,
                        self.watermark_corner == index,
                        theme::ACCENT,
                        Vec2::new(60.0, 20.0),
                    )
                    .clicked()
                    {
                        self.watermark_corner = index;
                        changed = true;
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("SIZE  ").font(theme::mono(9.5)).color(theme::TEXT_FAINT));
                if ui
                    .add(egui::Slider::new(&mut self.watermark_scale, 0.03..=0.35).show_value(false))
                    .changed()
                {
                    changed = true;
                }
                ui.label(
                    RichText::new(format!("{:.0}% of frame height", self.watermark_scale * 100.0))
                        .font(theme::mono(9.5))
                        .color(theme::TEXT_DIM),
                );
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("FADE  ").font(theme::mono(9.5)).color(theme::TEXT_FAINT));
                if ui
                    .add(
                        egui::Slider::new(&mut self.watermark_opacity, 0.05..=1.0)
                            .show_value(false),
                    )
                    .changed()
                {
                    changed = true;
                }
                ui.label(
                    RichText::new(format!("{:.0}%", self.watermark_opacity * 100.0))
                        .font(theme::mono(9.5))
                        .color(theme::TEXT_DIM),
                );
            });
            if changed {
                self.engine.send(Command::SetWatermarkLook {
                    corner: self.watermark_corner,
                    scale: self.watermark_scale,
                    opacity: self.watermark_opacity,
                });
            }
        }

        // ---- ticker -------------------------------------------------------
        ui.add_space(14.0);
        ui.label(RichText::new("TICKER").size(10.5).strong().color(theme::TEXT_DIM));
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(
                RichText::new(
                    "The strap that crawls along the foot of the frame. Service times, \
                     a phone number, a welcome — whatever has to be readable without \
                     covering the shot.",
                )
                .size(10.5)
                .color(theme::TEXT_DIM),
            )
            .wrap(),
        );
        ui.add_space(6.0);
        let typed = ui
            .add(
                egui::TextEdit::singleline(&mut self.ticker_text)
                    .desired_width(f32::INFINITY)
                    .hint_text("WELCOME  ·  EVERY SUNDAY 7:50 AM  ·  9952978141"),
            )
            .changed();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let on = self.ticker_on;
            if theme::chip(
                ui,
                if on { "ON AIR" } else { "OFF" },
                on,
                theme::PROGRAM,
                Vec2::new(70.0, 24.0),
            )
            .on_hover_text("put the strap up, or take it down")
            .clicked()
            {
                self.ticker_on = !on;
                self.engine.send(Command::SetTicker {
                    text: self.ticker_text.clone(),
                    on: self.ticker_on,
                });
            }
            ui.label(RichText::new("SPEED").font(theme::mono(9.5)).color(theme::TEXT_FAINT));
            if ui
                .add(egui::Slider::new(&mut self.ticker_speed, 40.0..=400.0).show_value(false))
                .changed()
            {
                self.engine.send(Command::SetTickerLook {
                    speed: self.ticker_speed,
                    background: self.ticker_background,
                    colour: [235, 238, 245],
                });
            }
            ui.label(
                RichText::new(format!("{:.0} px/s", self.ticker_speed))
                    .font(theme::mono(9.5))
                    .color(theme::TEXT_DIM),
            );
        });
        if typed && self.ticker_on {
            self.engine.send(Command::SetTicker {
                text: self.ticker_text.clone(),
                on: true,
            });
        }
        let _ = snapshot;
    }

    /// What the stream leaves at, and what that costs.
    ///
    /// The numbers are the point. "Will 1080p work on our connection" is the
    /// question every hall asks, and it cannot be answered by a dropdown that
    /// says 1080p — it needs the megabits a second written next to it, and
    /// the headroom a connection needs on top.
    fn stream_quality(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        use crate::settings::{StreamSize, AUDIO_KBPS_CHOICES};

        ui.label(RichText::new("STREAM QUALITY").size(12.0).strong().color(theme::TEXT));
        ui.label(
            RichText::new(
                "Separate from the production size. The hall can run 1080p for the \
                 projector and the recording while the internet gets 720p — which is \
                 what most people are watching on a phone anyway.",
            )
            .size(10.5)
            .color(theme::TEXT_DIM),
        );
        ui.add_space(10.0);

        let mut size = snapshot.stream_size;
        let mut kbps = snapshot.stream_kbps;
        let mut audio = snapshot.stream_audio_kbps;
        let mut changed = false;

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(4.0, 4.0);
            for choice in StreamSize::ALL {
                let (w, h) = choice.size();
                if theme::chip(ui, choice.label(), size == choice, theme::ACCENT, Vec2::new(58.0, 24.0))
                    .on_hover_text(format!(
                        "{w} x {h}, usually about {:.1} Mb/s",
                        choice.suggested_kbps() as f32 / 1000.0
                    ))
                    .clicked()
                {
                    size = choice;
                    // The bitrate follows the size unless it has been moved
                    // deliberately: a 1080p picture at a 360p bitrate looks
                    // worse than 360p did, which is the trap in offering the
                    // choice at all.
                    kbps = choice.suggested_kbps();
                    changed = true;
                }
            }
        });

        ui.add_space(8.0);
        let (low, high) = size.kbps_range();
        kbps = kbps.clamp(low, high);
        ui.horizontal(|ui| {
            ui.label(RichText::new("PICTURE").font(theme::mono(10.0)).color(theme::TEXT_FAINT));
            if ui
                .add(
                    egui::Slider::new(&mut kbps, low..=high)
                        .show_value(false)
                        .step_by(100.0),
                )
                .changed()
            {
                changed = true;
            }
            ui.label(
                RichText::new(format!("{:.1} Mb/s", kbps as f32 / 1000.0))
                    .font(theme::mono(11.0))
                    .color(theme::TEXT),
            );
            if kbps != size.suggested_kbps()
                && theme::chip(ui, "SUGGESTED", false, theme::PREVIEW, Vec2::new(76.0, 19.0))
                    .on_hover_text(format!(
                        "back to {:.1} Mb/s, which is what the platforms ask for at {}",
                        size.suggested_kbps() as f32 / 1000.0,
                        size.label()
                    ))
                    .clicked()
            {
                kbps = size.suggested_kbps();
                changed = true;
            }
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("SOUND  ").font(theme::mono(10.0)).color(theme::TEXT_FAINT));
            for choice in AUDIO_KBPS_CHOICES {
                if theme::chip(
                    ui,
                    &format!("{choice}k"),
                    audio == choice,
                    theme::ACCENT,
                    Vec2::new(46.0, 20.0),
                )
                .on_hover_text(match choice {
                    64 => "for a connection that needs every kilobit for the picture",
                    96 => "speech is fine here; music starts to thin out",
                    128 => "transparent for speech and music together — the usual choice",
                    _ => "for music that matters more than the picture does",
                })
                .clicked()
                {
                    audio = choice;
                    changed = true;
                }
            }
        });

        // ---- what it costs ------------------------------------------------
        ui.add_space(12.0);
        let total = (kbps + audio) as f32 / 1000.0;
        // Platforms and every guide say the same thing: leave headroom. A
        // connection running at exactly its limit drops frames the moment
        // anything else on the network wants a share.
        let needed = total * 1.5;
        egui::Frame::none()
            .fill(theme::SURFACE_LOWEST)
            .inner_margin(egui::Margin::symmetric(12.0, 10.0))
            .rounding(Rounding::same(5.0_f32))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{total:.1} Mb/s"))
                            .size(20.0)
                            .strong()
                            .color(theme::ACCENT),
                    );
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("leaving this machine, picture and sound together")
                                .size(10.5)
                                .color(theme::TEXT_DIM),
                        );
                        ui.label(
                            RichText::new(format!(
                                "Needs about {needed:.1} Mb/s of upload to be safe, \
                                 and about {:.0} MB for every hour on air.",
                                total * 450.0
                            ))
                            .size(10.5)
                            .color(theme::TEXT_DIM),
                        );
                    });
                });
                if snapshot.streaming {
                    ui.add_space(6.0);
                    // What is really leaving, and how much of it. A stream
                    // that connects and carries nothing is the worst failure
                    // this program can have, because every other indicator
                    // says it is working -- so the number that would have
                    // shown it is put where it cannot be missed.
                    let sent: u64 = snapshot.destinations.iter().map(|d| d.bytes_sent).sum();
                    let uptime = snapshot
                        .destinations
                        .iter()
                        .map(|d| d.uptime_seconds)
                        .max()
                        .unwrap_or(0);
                    let moving = sent > 0 && snapshot.stats.frames_encoded > 0;
                    ui.label(
                        RichText::new(if moving {
                            format!(
                                "Going out now: {:.1} Mb/s  ·  {:.1} MB sent  ·                                   {} frames encoded  ·  up {}:{:02}",
                                snapshot.stream_measured_kbps / 1000.0,
                                sent as f32 / 1_000_000.0,
                                snapshot.stats.frames_encoded,
                                uptime / 60,
                                uptime % 60,
                            )
                        } else if uptime < 3 {
                            "Connected. Waiting for the first frames to go out...".to_string()
                        } else {
                            format!(
                                "CONNECTED BUT NOTHING IS GOING OUT after {uptime}s.                                  The address was accepted and no picture has left this                                  machine -- check the stream key, and that something is                                  on Program."
                            )
                        })
                        .font(theme::mono(10.5))
                        .color(if moving {
                            theme::PREVIEW
                        } else if uptime < 3 {
                            theme::TEXT_DIM
                        } else {
                            theme::PROGRAM
                        }),
                    );
                    if snapshot.stream_dropped > 0 {
                        ui.label(
                            RichText::new(format!(
                                "The encoder could not keep up with {} pictures — the                                  stream is smoother at a smaller size.",
                                snapshot.stream_dropped
                            ))
                            .font(theme::mono(10.5))
                            .color(theme::WARN),
                        );
                    }
                    if snapshot.ftb {
                        ui.label(
                            RichText::new(
                                "FADE TO BLACK is on, so the stream is carrying black.",
                            )
                            .font(theme::mono(10.5))
                            .color(theme::WARN),
                        );
                    }
                }
            });

        if changed {
            self.engine.send(Command::SetStreamQuality { size, kbps, audio_kbps: audio });
        }
    }

    fn stream_view(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        egui::CentralPanel::default()
            .frame(theme::panel(theme::SURFACE))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().id_salt("stream-page").show(ui, |ui| {
                ui.add_space(20.0);
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    ui.vertical(|ui| {
                        ui.set_max_width(560.0);
                        self.graphics(ui, snapshot);
                        ui.add_space(18.0);
                        ui.separator();
                        ui.add_space(14.0);

                        self.stream_quality(ui, snapshot);
                        ui.add_space(18.0);
                        ui.separator();
                        ui.add_space(14.0);

                        ui.label(
                            RichText::new("STREAM DESTINATION").size(12.0).strong().color(theme::TEXT),
                        );
                        ui.label(
                            RichText::new(
                                "Add as many as the connection will carry. The picture is \
                                 encoded once and fanned out, so a second destination costs \
                                 bandwidth, not processing.",
                            )
                            .size(10.5)
                            .color(theme::TEXT_DIM),
                        );
                        ui.add_space(12.0);

                        ui.horizontal_wrapped(|ui| {
                            for (label, address) in PRESETS {
                                let selected = self.rtmp_url == address;
                                if theme::chip(ui, label, selected, theme::ACCENT, theme::chip_size(ui, label, 24.0))
                                    .clicked()
                                {
                                    self.rtmp_url = address.to_string();
                                }
                            }
                        });
                        ui.add_space(12.0);

                        let srt = engine::is_srt(&self.rtmp_url);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("ADDRESS").size(10.5).color(theme::TEXT_DIM));
                            ui.label(
                                RichText::new(if srt { "SRT" } else { "RTMP" })
                                    .font(theme::mono(9.5))
                                    .color(theme::ACCENT),
                            );
                        });
                        ui.add(
                            egui::TextEdit::singleline(&mut self.rtmp_url)
                                .desired_width(f32::INFINITY),
                        );

                        ui.add_space(10.0);
                        ui.label(
                            RichText::new(if srt { "STREAM ID" } else { "STREAM KEY" })
                                .size(10.5)
                                .color(theme::TEXT_DIM),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut self.stream_key)
                                .password(!srt)
                                .desired_width(f32::INFINITY),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(if srt {
                                "SRT identifies a stream by id rather than by key. Leave it \
                                 blank unless the receiver expects one. Latency, mode and \
                                 passphrase go in the address."
                            } else {
                                "The key may also sit inside the URL. Both forms work, because \
                                 platforms present them differently."
                            })
                            .size(10.5)
                            .color(theme::TEXT_FAINT),
                        );

                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            let label =
                                if snapshot.destinations.is_empty() { "GO LIVE" } else { "ADD DESTINATION" };
                            if theme::button(ui, label, theme::PREVIEW, Vec2::new(168.0, 34.0)).clicked()
                                && !self.rtmp_url.trim().is_empty()
                            {
                                self.engine.send(Command::StartStream {
                                    url: self.rtmp_url.clone(),
                                    key: self.stream_key.clone(),
                                });
                                if snapshot.destinations.is_empty() {
                                    self.tab = Tab::Switcher;
                                }
                            }
                            if !snapshot.destinations.is_empty() {
                                ui.add_space(8.0);
                                if theme::button(ui, "STOP ALL", theme::PROGRAM, Vec2::new(120.0, 34.0))
                                    .clicked()
                                {
                                    self.engine.send(Command::StopStream);
                                }
                            }
                        });

                        // ---- what is live now --------------------------------
                        if !snapshot.destinations.is_empty() {
                            ui.add_space(18.0);
                            ui.label(
                                RichText::new(format!("ON AIR — {}", snapshot.destinations.len()))
                                    .size(11.0)
                                    .strong()
                                    .color(theme::PROGRAM),
                            );
                            ui.add_space(6.0);
                            for (index, destination) in snapshot.destinations.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(&destination.protocol)
                                            .font(theme::mono(9.5))
                                            .color(theme::ACCENT),
                                    );
                                    ui.add_space(4.0);
                                    // Truncated rather than wrapped: a long
                                    // address would push the readouts off the
                                    // row, and the full text is on hover.
                                    let short: String =
                                        destination.address.chars().take(44).collect();
                                    ui.label(RichText::new(short).size(10.5).color(theme::TEXT))
                                        .on_hover_text(&destination.address);

                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if theme::button(
                                                ui,
                                                "STOP",
                                                theme::PROGRAM,
                                                Vec2::new(58.0, 22.0),
                                            )
                                            .clicked()
                                            {
                                                self.engine
                                                    .send(Command::StopDestination(index));
                                            }
                                            ui.add_space(10.0);
                                            ui.label(
                                                RichText::new(format!(
                                                    "{}   {}",
                                                    format_duration(destination.uptime_seconds),
                                                    format_bytes(destination.bytes_sent),
                                                ))
                                                .font(theme::mono(9.5))
                                                .color(theme::TEXT_DIM),
                                            );
                                        },
                                    );
                                });
                                ui.add_space(3.0);
                            }
                        }

                        if let Some(error) = &snapshot.stream_error {
                            ui.add_space(12.0);
                            ui.label(RichText::new(error).size(10.5).color(theme::PROGRAM));
                        }

                        // ---- NDI output --------------------------------------
                        ui.add_space(24.0);
                        ui.separator();
                        ui.add_space(14.0);
                        ui.label(
                            RichText::new("NETWORK OUTPUT (NDI)")
                                .size(12.0)
                                .strong()
                                .color(theme::TEXT),
                        );
                        ui.add_space(6.0);

                        if let Some(reason) = rhevia_ndi::unavailable_reason() {
                            ui.label(
                                RichText::new(
                                    "NDI is not available. Rhevia uses the runtime you have \
                                     installed rather than shipping its own.",
                                )
                                .size(10.5)
                                .color(theme::TEXT_FAINT),
                            );
                            ui.label(
                                RichText::new(reason)
                                    .font(theme::mono(9.0))
                                    .color(theme::TEXT_FAINT),
                            );
                        } else {
                            ui.label(
                                RichText::new(
                                    "Publishes the programme to the local network at full \
                                     quality, before encoding. Independent of streaming.",
                                )
                                .size(10.5)
                                .color(theme::TEXT_DIM),
                            );
                            ui.add_space(8.0);

                            match &snapshot.ndi_output {
                                Some(name) => {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(format!("PUBLISHING  {name}"))
                                                .font(theme::mono(10.0))
                                                .color(theme::PREVIEW),
                                        );
                                        ui.add_space(10.0);
                                        if theme::button(
                                            ui,
                                            "STOP",
                                            theme::PROGRAM,
                                            Vec2::new(80.0, 26.0),
                                        )
                                        .clicked()
                                        {
                                            self.engine.send(Command::StopNdiOutput);
                                        }
                                    });
                                }
                                None => {
                                    ui.label(
                                        RichText::new("SOURCE NAME")
                                            .size(10.0)
                                            .color(theme::TEXT_DIM),
                                    );
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.ndi_output_name)
                                            .desired_width(300.0),
                                    );
                                    ui.add_space(8.0);
                                    if theme::button(
                                        ui,
                                        "PUBLISH",
                                        theme::ACCENT,
                                        Vec2::new(140.0, 30.0),
                                    )
                                    .clicked()
                                        && !self.ndi_output_name.trim().is_empty()
                                    {
                                        self.engine.send(Command::StartNdiOutput {
                                            name: self.ndi_output_name.trim().to_string(),
                                        });
                                    }
                                }
                            }
                        }

                        // ---- recording ---------------------------------------
                        ui.add_space(24.0);
                        ui.separator();
                        ui.add_space(14.0);
                        ui.label(RichText::new("RECORDING").size(12.0).strong().color(theme::TEXT));
                        ui.add_space(10.0);
                        ui.label(RichText::new("FILE").size(10.5).color(theme::TEXT_DIM));
                        ui.horizontal(|ui| {
                            if ui.button("Browse…").clicked() {
                                if let Some(path) = rfd::FileDialog::new()
                                    .set_title("Where to record")
                                    .add_filter("H.264", &["h264"])
                                    .save_file()
                                {
                                    self.record_path = path.display().to_string();
                                }
                            }
                            ui.add(
                                egui::TextEdit::singleline(&mut self.record_path)
                                    .desired_width(f32::INFINITY),
                            );
                        });
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("Annex-B H.264.   ffmpeg -i rec.h264 -c copy rec.mp4")
                                .font(theme::mono(10.0))
                                .color(theme::TEXT_FAINT),
                        );
                        ui.add_space(20.0);
                    });
                });
                });
            });
    }

    fn settings_view(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        egui::CentralPanel::default()
            .frame(theme::panel(theme::SURFACE))
            .show(ctx, |ui| {
                ui.add_space(20.0);
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    ui.vertical(|ui| {
                        // A settings page is read, not scanned, and a line of
                        // prose the full width of a 4K display is not read.
                        ui.set_max_width(620.0);
                        ui.label(RichText::new("PRODUCTION").size(12.0).strong().color(theme::TEXT));
                        ui.add_space(4.0);
                        // Held to a column width rather than the whole window,
                        // which on a wide screen strings a sentence out across
                        // two feet of desk and makes it unreadable.
                        ui.add(
                            egui::Label::new(
                                RichText::new(
                                    "Everything is composited, encoded and streamed at this \
                                     size. An input larger than the production is scaled down \
                                     to it on the way in, so choose at least what your cameras \
                                     and files are.",
                                )
                                .size(10.5)
                                .color(theme::TEXT_DIM),
                            )
                            .wrap(),
                        );
                        ui.add_space(8.0);

                        ui.horizontal(|ui| {
                            for resolution in crate::settings::Resolution::ALL {
                                let (w, h) = resolution.size();
                                if theme::chip(
                                    ui,
                                    resolution.label(),
                                    self.settings.resolution == resolution,
                                    theme::ACCENT,
                                    Vec2::new(62.0, 24.0),
                                )
                                .on_hover_text(format!(
                                    "{w} x {h} at {:.1} Mb/s",
                                    resolution.bitrate() as f32 / 1_000_000.0
                                ))
                                .clicked()
                                {
                                    self.settings.resolution = resolution;
                                    self.settings.save();
                                }
                            }
                        });

                        // Said plainly, because a setting that looks as though
                        // it did nothing is worse than one that is not offered.
                        let running = engine::output_size();
                        if self.settings.resolution.size() != running {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(format!(
                                    "Running at {} x {} — restart Rhevia for {}.",
                                    running.0,
                                    running.1,
                                    self.settings.resolution.label()
                                ))
                                .size(10.5)
                                .color(theme::PROGRAM),
                            );
                        }

                        ui.add_space(12.0);
                        ui.label(
                            RichText::new("PICTURES A SECOND")
                                .size(10.5)
                                .strong()
                                .color(theme::TEXT_DIM),
                        );
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(4.0, 4.0);
                            for rate in crate::settings::FrameRate::ALL {
                                if theme::chip(
                                    ui,
                                    rate.label(),
                                    self.settings.frame_rate == rate,
                                    theme::ACCENT,
                                    Vec2::new(46.0, 24.0),
                                )
                                .on_hover_text(format!(
                                    "{} a second — {:.0}% of the work thirty costs",
                                    rate.label(),
                                    rate.cost() * 100.0
                                ))
                                .clicked()
                                {
                                    self.settings.frame_rate = rate;
                                    self.settings.save();
                                }
                            }
                        });
                        if (self.settings.frame_rate.fps() - target_fps()).abs() > 0.01 {
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(format!(
                                    "Running at {:.0} a second — restart Rhevia for {}.",
                                    target_fps(),
                                    self.settings.frame_rate.label()
                                ))
                                .size(10.5)
                                .color(theme::PROGRAM),
                            );
                        }

                        ui.add_space(14.0);
                        ui.label(RichText::new("MONITORING").size(12.0).strong().color(theme::TEXT));
                        ui.add_space(6.0);
                        if theme::chip(
                            ui,
                            if self.settings.monitor { "ON AT STARTUP" } else { "OFF AT STARTUP" },
                            self.settings.monitor,
                            theme::PREVIEW,
                            Vec2::new(128.0, 24.0),
                        )
                        .on_hover_text(
                            "whether you hear the programme through your speakers when \
                             Rhevia starts — the LISTEN control on the mixer changes it now",
                        )
                        .clicked()
                        {
                            self.settings.monitor = !self.settings.monitor;
                            self.settings.save();
                            // Take effect immediately as well as next time, or
                            // the switch appears not to work.
                            self.engine.send(Command::SetMonitor {
                                device: self.settings.monitor_device.clone(),
                                on: self.settings.monitor,
                            });
                        }

                        // Which device. Following the Windows default sounds
                        // obvious until the operator is on a desk and Windows
                        // is pointing at a headset — then the programme is
                        // playing perfectly into something nobody is wearing,
                        // which is indistinguishable from no sound at all.
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Play through")
                                    .size(11.0)
                                    .color(theme::TEXT_DIM),
                            );
                            let shown = snapshot
                                .monitor
                                .clone()
                                .or_else(|| self.settings.monitor_device.clone())
                                .unwrap_or_else(|| "System default".to_string());
                            egui::ComboBox::from_id_salt("monitor-device")
                                .selected_text(RichText::new(shown).size(11.0))
                                .width(280.0)
                                .show_ui(ui, |ui| {
                                    if ui
                                        .selectable_label(
                                            self.settings.monitor_device.is_none(),
                                            "System default",
                                        )
                                        .clicked()
                                    {
                                        self.choose_monitor(None);
                                    }
                                    for device in &self.cached_playback {
                                        let chosen = self.settings.monitor_device.as_deref()
                                            == Some(device.name.as_str());
                                        if ui.selectable_label(chosen, &device.name).clicked() {
                                            self.pending_monitor = Some(device.name.clone());
                                        }
                                    }
                                });
                            if ui
                                .button(RichText::new("Refresh").size(10.5))
                                .on_hover_text("look for devices plugged in since Rhevia started")
                                .clicked()
                            {
                                self.cached_playback = rhevia_audio::monitor_devices();
                            }
                        });
                        // Applied outside the combo box, which holds a borrow
                        // of self while it is open.
                        if let Some(name) = self.pending_monitor.take() {
                            self.choose_monitor(Some(name));
                        }
                        if self.cached_playback.is_empty() {
                            self.cached_playback = rhevia_audio::monitor_devices();
                        }

                        // The projector. A hall runs one, and it is the
                        // reason a second display exists on that machine.
                        ui.add_space(16.0);
                        ui.label(
                            RichText::new("PROGRAMME OUT TO A DISPLAY")
                                .size(12.0)
                                .strong()
                                .color(theme::TEXT),
                        );
                        ui.add_space(4.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(
                                    "Throws the programme full screen onto another display                                      with nothing else on it, for a projector or a foldback                                      monitor. Escape on that screen closes it again.",
                                )
                                .size(10.5)
                                .color(theme::TEXT_DIM),
                            )
                            .wrap(),
                        );
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            let shown = self
                                .settings
                                .output_display
                                .clone()
                                .unwrap_or_else(|| "Off".to_string());
                            egui::ComboBox::from_id_salt("output-display")
                                .selected_text(RichText::new(shown).size(11.0))
                                .width(300.0)
                                .show_ui(ui, |ui| {
                                    if ui
                                        .selectable_label(
                                            self.settings.output_display.is_none(),
                                            "Off",
                                        )
                                        .clicked()
                                    {
                                        self.pending_display = Some(String::new());
                                    }
                                    for display in &self.cached_monitors {
                                        let chosen = self.settings.output_display.as_deref()
                                            == Some(display.name.as_str());
                                        let label = format!(
                                            "{}  ({} x {})",
                                            display.name, display.width, display.height
                                        );
                                        if ui.selectable_label(chosen, label).clicked() {
                                            self.pending_display = Some(display.name.clone());
                                        }
                                    }
                                });
                            if ui
                                .button(RichText::new("Refresh").size(10.5))
                                .on_hover_text("look for displays plugged in since Rhevia started")
                                .clicked()
                            {
                                self.refresh_devices();
                            }
                        });
                        // Applied outside the combo box, which holds a borrow
                        // of self while it is open.
                        if let Some(name) = self.pending_display.take() {
                            self.settings.output_display =
                                Some(name).filter(|n| !n.is_empty());
                            self.settings.save();
                        }
                        if self.cached_monitors.is_empty() {
                            self.refresh_devices();
                        } else if self.cached_monitors.len() < 2 {
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(
                                    "Only one display is attached, so this would cover the                                      controls. Plug in a projector or a second screen.",
                                )
                                .size(10.5)
                                .color(theme::TEXT_FAINT),
                            );
                        }

                        // What the sound is actually doing, in numbers.
                        // "The audio is crackling" and "the audio is fine"
                        // are the same sentence until something counts the
                        // faults, and every one of these is a discontinuity
                        // in the waveform that is heard as a pop.
                        ui.add_space(16.0);
                        ui.label(RichText::new("AUDIO HEALTH").size(12.0).strong().color(theme::TEXT));
                        ui.add_space(6.0);
                        let faults = snapshot.audio_gaps
                            + snapshot.audio_dropped
                            + snapshot.audio_padded
                            + snapshot.audio_trimmed;
                        for (label, value, bad) in [
                            (
                                "Listening on",
                                snapshot
                                    .monitor
                                    .clone()
                                    .unwrap_or_else(|| "nothing".to_string()),
                                snapshot.monitor.is_none(),
                            ),
                            (
                                "Cushion",
                                format!(
                                    "{} frames · {:.0} ms",
                                    snapshot.audio_buffered,
                                    snapshot.audio_buffered as f32 / 48.0
                                ),
                                false,
                            ),
                            ("Card ran dry", snapshot.audio_gaps.to_string(), snapshot.audio_gaps > 0),
                            (
                                "Cushion spilled",
                                snapshot.audio_dropped.to_string(),
                                snapshot.audio_dropped > 0,
                            ),
                            (
                                "Clip handed short",
                                snapshot.audio_padded.to_string(),
                                snapshot.audio_padded > 2,
                            ),
                            (
                                "Clip buffer jumped",
                                snapshot.audio_trimmed.to_string(),
                                snapshot.audio_trimmed > 0,
                            ),
                        ] {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!("{label:<20}"))
                                        .font(theme::mono(11.0))
                                        .color(theme::TEXT_FAINT),
                                );
                                ui.label(
                                    RichText::new(value)
                                        .font(theme::mono(11.0))
                                        .color(if bad { theme::WARN } else { theme::TEXT }),
                                );
                            });
                        }
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(if faults == 0 {
                                "No faults since Rhevia started. A crackle heard with all \
                                 of these at zero is coming from outside Rhevia — the \
                                 device, its driver, or Windows mixing it with something else."
                            } else {
                                "Something above is not zero. Each one is a break in the sound."
                            })
                            .size(10.5)
                            .color(if faults == 0 { theme::TEXT_DIM } else { theme::WARN }),
                        );

                        ui.add_space(22.0);
                        ui.label(RichText::new("KEYBOARD").size(12.0).strong().color(theme::TEXT));
                        ui.add_space(10.0);
                        for (keys, what) in [
                            ("1 – 8", "arm that input in Preview"),
                            ("Ctrl + 1 – 8", "cut that input straight to air"),
                            ("Space", "CUT"),
                            ("Enter", "AUTO fade"),
                            ("Esc", "fade to black"),
                        ] {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!("{keys:<16}"))
                                        .font(theme::mono(11.0))
                                        .color(theme::ACCENT),
                                );
                                ui.label(RichText::new(what).size(11.0).color(theme::TEXT_DIM));
                            });
                        }

                        ui.add_space(22.0);
                        ui.label(RichText::new("ENGINE").size(12.0).strong().color(theme::TEXT));
                        ui.add_space(10.0);
                        let s = snapshot.stats;
                        for (label, value) in [
                            (
                                "Canvas",
                                format!(
                                    "{} x {} @ {} fps",
                                    engine::output_size().0,
                                    engine::output_size().1,
                                    target_fps() as u32
                                ),
                            ),
                            ("Video codec", "H.264 · OpenH264 (BSD-2-Clause)".to_string()),
                            ("Audio", format!("48 kHz stereo · {} channels", snapshot.audio.len())),
                            ("Frames rendered", s.frames_rendered.to_string()),
                            ("Frames encoded", s.frames_encoded.to_string()),
                            ("Render time", format!("{:.2} ms", s.render_ms)),
                        ] {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!("{label:<18}"))
                                        .font(theme::mono(11.0))
                                        .color(theme::TEXT_FAINT),
                                );
                                ui.label(RichText::new(value).font(theme::mono(11.0)).color(theme::TEXT));
                            });
                        }
                    });
                });
            });
    }

    fn dialogs(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        self.title_editor(ctx);
        self.input_settings(ctx, snapshot);

        self.input_select(ctx, snapshot);
    }

    /// The mark, uploaded to the graphics card the first time it is asked
    /// for and kept afterwards.
    ///
    /// Decoding and uploading it on every repaint would cost more than the
    /// rest of the interface put together.
    fn mark(&mut self, ctx: &egui::Context) -> Option<&egui::TextureHandle> {
        if self.mark.is_none() {
            let png = include_bytes!("../../../assets/mark-128.png");
            let decoded = image::load_from_memory(png).ok()?.into_rgba8();
            let size = [decoded.width() as usize, decoded.height() as usize];
            let image = egui::ColorImage::from_rgba_unmultiplied(size, decoded.as_raw());
            self.mark = Some(ctx.load_texture("rhevia-mark", image, egui::TextureOptions::LINEAR));
        }
        self.mark.as_ref()
    }

    /// Starts a plugin scan on its own thread.
    ///
    /// Scanning loads every module and then runs a process per plugin to
    /// check it. That is seconds of work, and doing it on the interface
    /// thread would freeze the window while a show is running.
    fn start_plugin_scan(&mut self) {
        let (sender, receiver) = std::sync::mpsc::channel();
        if std::thread::Builder::new()
            .name("rhevia-plugin-scan".into())
            .spawn(move || {
                let _ = sender.send(rhevia_plugin::validated_effects());
            })
            .is_ok()
        {
            self.plugin_scan = Some(receiver);
        }
    }

    /// Starts looking for devices, on a worker thread.
    ///
    /// The dialog opens immediately with whatever was found last time and
    /// fills in when the scan finishes. Doing the work here instead would
    /// freeze the window for about a second every time the dialog opens,
    /// which during a show is indistinguishable from a crash.
    fn refresh_devices(&mut self) {
        if self.device_scan.is_some() {
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        if std::thread::Builder::new()
            .name("rhevia-device-scan".into())
            .spawn(move || {
                let _ = sender.send(gather_devices());
            })
            .is_ok()
        {
            self.device_scan = Some(receiver);
        }
    }

    /// Takes the result of a finished scan, without waiting for one.
    fn collect_devices(&mut self) {
        let Some(receiver) = &self.device_scan else { return };
        let Ok(found) = receiver.try_recv() else { return };

        self.cached_cameras = found.cameras;
        self.cached_monitors = found.monitors;
        self.cached_windows = found.windows;
        self.cached_audio = found.audio;
        self.cached_ndi = found.ndi;
        self.device_error = found.error;
        self.device_scan = None;
    }

    /// The input dialog: types down the left, the chosen type on the right.
    ///
    /// One dialog for every kind of input rather than a menu that opens
    /// further dialogs, because choosing a source is a single decision and
    /// should not be spread across three windows.
    fn input_select(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        if !self.show_add_source {
            return;
        }

        let mut open = true;
        let mut close = false;

        egui::Window::new("Input select")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_size(Vec2::new(780.0, 560.0));

                ui.horizontal_top(|ui| {
                    // ---- left nav ------------------------------------------
                    ui.vertical(|ui| {
                        ui.set_width(176.0);
                        ui.spacing_mut().item_spacing = Vec2::new(0.0, 2.0);
                        ui.add_space(2.0);
                        for tab in InputTab::ALL {
                            let selected = self.input_tab == tab;
                            if theme::chip(
                                ui,
                                tab.label(),
                                selected,
                                theme::ACCENT,
                                Vec2::new(164.0, 30.0),
                            )
                            .clicked()
                            {
                                self.input_tab = tab;
                            }
                        }

                        ui.add_space(10.0);
                        if ui.button("Refresh devices").clicked() {
                            self.refresh_devices();
                        }
                        if self.device_scan.is_some() {
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new("looking…").size(9.5).color(theme::ACCENT),
                            );
                        }
                        if let Some(error) = &self.device_error {
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(error).size(9.5).color(theme::WARN),
                            );
                        }
                    });

                    ui.add_space(14.0);
                    theme::divider(ui, 528.0);
                    ui.add_space(14.0);

                    // ---- the chosen type -----------------------------------
                    ui.vertical(|ui| {
                        ui.set_width(552.0);
                        ui.add_space(2.0);
                        ui.label(RichText::new(self.input_tab.label()).size(15.0).strong());
                        ui.label(
                            RichText::new(self.input_tab.blurb())
                                .size(10.5)
                                .color(theme::TEXT_DIM),
                        );
                        ui.add_space(10.0);

                        ui.label(RichText::new("NAME").size(10.0).color(theme::TEXT_DIM));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.new_source_name)
                                .hint_text("left blank, the device names itself")
                                .desired_width(f32::INFINITY),
                        );
                        ui.add_space(12.0);

                        egui::ScrollArea::vertical()
                            .max_height(420.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                close = self.input_body(ui, snapshot);
                            });
                    });
                });
                ui.add_space(4.0);
            });

        if !open || close {
            self.show_add_source = false;
        }
    }


    /// The right-hand pane. Returns true when an input was added.
    fn input_body(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) -> bool {
        match self.input_tab {
            InputTab::Camera => {
                if self.cached_cameras.is_empty() {
                    ui.label(
                        RichText::new("no cameras found — plug one in and press Refresh")
                            .size(10.5)
                            .color(theme::TEXT_FAINT),
                    );
                    return false;
                }
                for target in self.cached_cameras.clone() {
                    let label = if target.description.is_empty() {
                        target.name.clone()
                    } else {
                        format!("{}   ·   {}", target.name, target.description)
                    };
                    if ui.button(label).clicked() {
                        self.engine.send(Command::AddCameraSource {
                            name: self.name_or(&target.name),
                            target,
                        });
                        return true;
                    }
                }
                false
            }

            InputTab::Ndi => {
                if let Some(reason) = rhevia_ndi::unavailable_reason() {
                    // Said plainly rather than showing an empty list, which
                    // would read as "there are no sources".
                    ui.label(
                        RichText::new("NDI is not available on this machine.")
                            .size(11.0)
                            .color(theme::WARN),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            "Rhevia uses the NDI runtime you have installed rather than \
                             shipping its own. Install NDI Tools from ndi.video and restart.",
                        )
                        .size(10.0)
                        .color(theme::TEXT_FAINT),
                    );
                    ui.add_space(6.0);
                    ui.label(RichText::new(reason).font(theme::mono(9.0)).color(theme::TEXT_FAINT));
                    return false;
                }

                if let Some(version) = rhevia_ndi::version() {
                    ui.label(RichText::new(version).font(theme::mono(9.0)).color(theme::TEXT_FAINT));
                    ui.add_space(6.0);
                }

                if self.cached_ndi.is_empty() {
                    ui.label(
                        RichText::new(
                            "no sources are announcing themselves — press Refresh, and \
                             check both machines are on the same network",
                        )
                        .size(10.5)
                        .color(theme::TEXT_FAINT),
                    );
                    return false;
                }

                for source in self.cached_ndi.clone() {
                    let label = source.name.clone();
                    if ui.button(label).on_hover_text(&source.address).clicked() {
                        self.engine.send(Command::AddNdiSource {
                            name: self.name_or(&short_ndi_name(&source.name)),
                            source,
                        });
                        return true;
                    }
                }
                false
            }

            InputTab::Display => {
                if self.cached_monitors.is_empty() {
                    ui.label(
                        RichText::new("no displays found")
                            .size(10.5)
                            .color(theme::TEXT_FAINT),
                    );
                    return false;
                }
                for target in self.cached_monitors.clone() {
                    let label = format!("{}   {}x{}", target.name, target.width, target.height);
                    if ui.button(label).clicked() {
                        self.engine.send(Command::AddScreenSource {
                            name: self.name_or(&target.name),
                            target,
                        });
                        return true;
                    }
                }
                false
            }

            InputTab::Window => {
                if self.cached_windows.is_empty() {
                    ui.label(
                        RichText::new("no capturable windows — press Refresh")
                            .size(10.5)
                            .color(theme::TEXT_FAINT),
                    );
                    return false;
                }
                for target in self.cached_windows.clone() {
                    // Window titles run long; the list stays readable and the
                    // full title is on hover.
                    let short: String = target.name.chars().take(62).collect();
                    if ui.button(short).on_hover_text(&target.name).clicked() {
                        self.engine.send(Command::AddScreenSource {
                            name: self.name_or(&target.name),
                            target,
                        });
                        return true;
                    }
                }
                false
            }

            InputTab::Audio => {
                ui.label(RichText::new("ATTACH TO").size(10.0).color(theme::TEXT_DIM));
                ui.horizontal_wrapped(|ui| {
                    let standalone = self.attach_to.is_none();
                    if theme::chip(
                        ui,
                        "New audio input",
                        standalone,
                        theme::ACCENT,
                        Vec2::new(134.0, 24.0),
                    )
                    .clicked()
                    {
                        self.attach_to = None;
                    }
                    for (index, input) in snapshot.inputs.iter().enumerate() {
                        let selected = self.attach_to == Some(index);
                        let label = format!("{} {}", index + 1, input.name);
                        if theme::chip(ui, &label, selected, theme::ACCENT, theme::chip_size(ui, &label, 24.0))
                            .clicked()
                        {
                            self.attach_to = Some(index);
                        }
                    }
                });
                ui.add_space(10.0);
                ui.label(RichText::new("DEVICE").size(10.0).color(theme::TEXT_DIM));
                ui.add_space(4.0);

                if self.cached_audio.is_empty() {
                    ui.label(
                        RichText::new("no capture devices found")
                            .size(10.5)
                            .color(theme::TEXT_FAINT),
                    );
                    return false;
                }

                // Hardware and system audio are listed apart, because they
                // answer different questions: what is the microphone, and
                // what is the computer playing.
                let devices = self.cached_audio.clone();
                let mut chosen: Option<rhevia_audio::AudioDevice> = None;

                for (heading, kind) in [
                    ("HARDWARE", rhevia_audio::DeviceKind::Input),
                    ("SYSTEM AUDIO", rhevia_audio::DeviceKind::SystemAudio),
                ] {
                    let group: Vec<&rhevia_audio::AudioDevice> =
                        devices.iter().filter(|d| d.kind == kind).collect();
                    if group.is_empty() {
                        continue;
                    }

                    ui.add_space(6.0);
                    ui.label(RichText::new(heading).size(9.5).color(theme::TEXT_FAINT));
                    ui.add_space(3.0);

                    if kind == rhevia_audio::DeviceKind::SystemAudio {
                        ui.label(
                            RichText::new(
                                "Captures what is already playing. Start the music first: \
                                 an idle output produces nothing to capture.",
                            )
                            .size(9.5)
                            .color(theme::TEXT_FAINT),
                        );
                        ui.add_space(3.0);
                    }

                    for device in group {
                        // The prefix has done its job in the list; showing it
                        // on every row under its own heading is noise.
                        let shown = rhevia_audio::system_audio_endpoint(&device.name)
                            .unwrap_or(&device.name);
                        let label = if device.is_default {
                            format!("{shown}   (default)")
                        } else {
                            shown.to_string()
                        };
                        if ui.button(label).clicked() {
                            chosen = Some(device.clone());
                        }
                    }
                }

                if let Some(device) = chosen {
                    match self.attach_to {
                        Some(input) => self.engine.send(Command::AttachAudio {
                            input,
                            device: Some(device.name.clone()),
                        }),
                        None => self.engine.send(Command::AddAudioSource {
                            name: shorten(
                                rhevia_audio::system_audio_endpoint(&device.name)
                                    .unwrap_or(&device.name),
                            ),
                            device: Some(device.name.clone()),
                        }),
                    }
                    return true;
                }
                false
            }

            InputTab::Media => {
                ui.horizontal(|ui| {
                    if ui.button("Browse…").clicked() {
                        if let Some(path) = pick_file(
                            "Choose a video or audio file",
                            &[
                                (
                                    "Media",
                                    &[
                                        "mp4", "mov", "mkv", "webm", "avi", "flv", "wmv", "m4v",
                                        "mpg", "mpeg", "ts", "m2ts", "3gp", "ogv", "mp3", "wav",
                                        "flac", "aac", "m4a", "ogg", "opus", "wma", "aiff",
                                        "h264",
                                    ],
                                ),
                                ("Every file", &["*"]),
                            ],
                        ) {
                            self.file_path = path;
                        }
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut self.file_path)
                            .hint_text(r"or type a path")
                            .desired_width(f32::INFINITY),
                    );
                });
                ui.add_space(6.0);

                if rhevia_media::available() {
                    ui.label(
                        RichText::new(
                            "Video or audio, any container: MP4, MOV, MKV, WebM, AVI, MP3, \
                             WAV, FLAC and the rest. Clips loop. Sound arrives on its own \
                             mixer channel.",
                        )
                        .size(10.0)
                        .color(theme::TEXT_FAINT),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("Or drop a file anywhere on the window.")
                            .size(10.0)
                            .color(theme::TEXT_DIM),
                    );
                } else {
                    // Said plainly rather than failing later with a confusing
                    // error when the operator presses the button.
                    ui.label(
                        RichText::new(
                            "ffmpeg is not installed, so media files cannot be played. \
                             Everything else in Rhevia works without it.",
                        )
                        .size(10.5)
                        .color(theme::WARN),
                    );
                }

                ui.add_space(10.0);
                if ui.button("Add media").clicked() && !self.file_path.trim().is_empty() {
                    let path = self.file_path.trim().to_string();
                    let name = self.name_or(&file_stem(&path));
                    self.engine.send(command_for(name, path));
                    return true;
                }
                false
            }

            InputTab::Image => {
                ui.horizontal(|ui| {
                    if ui.button("Browse…").clicked() {
                        if let Some(path) = pick_file(
                            "Choose an image",
                            &[
                                ("Images", &["png", "jpg", "jpeg", "bmp", "gif", "webp", "tif", "tiff"]),
                                ("Every file", &["*"]),
                            ],
                        ) {
                            self.image_path = path;
                        }
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut self.image_path)
                            .hint_text(r"or type a path")
                            .desired_width(f32::INFINITY),
                    );
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "PNG, JPEG, BMP or GIF. Transparency is kept, so a PNG works as an overlay.",
                    )
                    .size(10.0)
                    .color(theme::TEXT_FAINT),
                );
                ui.add_space(10.0);
                if ui.button("Add image").clicked() && !self.image_path.trim().is_empty() {
                    self.engine.send(Command::AddImageSource {
                        name: self.name_or("Image"),
                        path: self.image_path.trim().to_string(),
                    });
                    return true;
                }
                false
            }

            InputTab::Title => {
                ui.label(RichText::new("HEADLINE").size(10.0).color(theme::TEXT_DIM));
                ui.add(
                    egui::TextEdit::singleline(&mut self.title_text)
                        .hint_text("ALEX CARTER")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(6.0);
                ui.label(RichText::new("SUBTITLE").size(10.0).color(theme::TEXT_DIM));
                ui.add(
                    egui::TextEdit::singleline(&mut self.title_subtitle)
                        .hint_text("LEAD ANALYST")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(10.0);
                if ui.button("Add title").clicked() && !self.title_text.trim().is_empty() {
                    self.engine.send(Command::AddTitleSource {
                        name: self.name_or("Title"),
                        text: self.title_text.trim().to_string(),
                        subtitle: self.title_subtitle.trim().to_string(),
                    });
                    return true;
                }
                false
            }

            InputTab::Layers => {
                ui.label(
                    RichText::new(
                        "Made empty and filled afterwards. Which inputs go into it, \
                         where each one sits and in what order are all chosen in the \
                         stack's own SET dialog, where they can be seen against the \
                         picture they make.",
                    )
                    .size(10.5)
                    .color(theme::TEXT_DIM),
                );
                ui.add_space(10.0);
                if theme::button(ui, "Add layered input", theme::ACCENT, Vec2::new(150.0, 26.0))
                    .clicked()
                {
                    self.engine
                        .send(Command::AddLayeredSource { name: self.name_or("Layers") });
                    return true;
                }
                false
            }
            InputTab::Colour => {
                // A handful of useful flats rather than a colour picker: these
                // are the ones a show actually reaches for.
                for (label, rgb) in [
                    ("Colour bars", None),
                    ("Black", Some([0u8, 0, 0])),
                    ("White", Some([235u8, 235, 235])),
                    ("Chroma green", Some([0u8, 177, 64])),
                    ("Chroma blue", Some([0u8, 71, 187])),
                    ("Studio magenta", Some([160u8, 40, 90])),
                ] {
                    if ui.button(label).clicked() {
                        match rgb {
                            None => self
                                .engine
                                .send(Command::AddBarsSource { name: self.name_or("Bars") }),
                            Some(rgb) => self.engine.send(Command::AddColourSource {
                                name: self.name_or(label),
                                rgb,
                            }),
                        }
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Builds one input out of several, bottom of the stack first.
    ///
    /// Listed in drawing order rather than in the order they were added, and
    /// labelled as such: "which one is on top" is the only question that
    /// matters here, and a list that does not answer it by being looked at is
    /// a list an operator has to experiment with during a service.
    fn stack_editor(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
        layers: &[engine::StackLayer],
        snapshot: &Snapshot,
    ) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("LAYERS").size(11.0).strong().color(theme::TEXT));
            ui.label(
                RichText::new(format!("{} of {}", layers.len(), engine::MAX_LAYERS))
                    .font(theme::mono(9.5))
                    .color(theme::TEXT_FAINT),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if layers.len() < engine::MAX_LAYERS {
                    egui::ComboBox::from_id_salt(("add-layer", index))
                        .selected_text(RichText::new("+ add layer").size(10.5))
                        .width(140.0)
                        .show_ui(ui, |ui| {
                            for (other, info) in snapshot.inputs.iter().enumerate() {
                                // A stack cannot be built out of itself.
                                if other == index {
                                    continue;
                                }
                                let label = format!("{} {}", other + 1, info.name);
                                if ui.selectable_label(false, label).clicked() {
                                    self.pending_layer = Some((index, other));
                                }
                            }
                        });
                }
            });
        });
        // Applied outside the combo box, which holds a borrow while it is open.
        if let Some((input, layer)) = self.pending_layer.take() {
            self.engine.send(Command::AddLayer { input, layer });
        }

        if layers.is_empty() {
            ui.add_space(4.0);
            ui.label(
                RichText::new("Empty. Add an input and it fills the frame; add another and it sits on top.")
                    .size(10.5)
                    .color(theme::TEXT_FAINT),
            );
            return;
        }

        ui.add_space(6.0);
        // Top of the stack first, because that is what is seen first.
        for at in (0..layers.len()).rev() {
            let mut layer = layers[at];
            let mut changed = false;
            let name = snapshot
                .inputs
                .get(layer.input)
                .map(|i| i.name.as_str())
                .unwrap_or("missing");

            egui::Frame::none()
                .fill(theme::SURFACE_LOWEST)
                .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                .rounding(Rounding::same(4.0_f32))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(if at + 1 == layers.len() { "TOP" } else { "   " })
                                .font(theme::mono(9.0))
                                .color(theme::ACCENT),
                        );
                        ui.label(
                            RichText::new(format!("{} {name}", layer.input + 1))
                                .size(11.0)
                                .color(theme::TEXT),
                        );

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::chip(ui, "X", false, theme::WARN, Vec2::new(24.0, 19.0))
                                .on_hover_text("take this layer out of the stack")
                                .clicked()
                            {
                                self.engine.send(Command::RemoveLayer { input: index, at });
                            }
                            if theme::chip(ui, "\u{25BC}", false, theme::ACCENT, Vec2::new(24.0, 19.0))
                                .on_hover_text("move down, behind the layer below")
                                .clicked()
                            {
                                self.engine.send(Command::MoveLayer { input: index, at, up: false });
                            }
                            if theme::chip(ui, "\u{25B2}", false, theme::ACCENT, Vec2::new(24.0, 19.0))
                                .on_hover_text("move up, in front of the layer above")
                                .clicked()
                            {
                                self.engine.send(Command::MoveLayer { input: index, at, up: true });
                            }
                            if theme::chip(
                                ui,
                                if layer.visible { "ON" } else { "OFF" },
                                layer.visible,
                                theme::PREVIEW,
                                Vec2::new(34.0, 19.0),
                            )
                            .on_hover_text("hide without removing")
                            .clicked()
                            {
                                layer.visible = !layer.visible;
                                changed = true;
                            }
                        });
                    });

                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);
                        for (label, x, y, w, h) in engine::StackLayer::PLACES {
                            let here = (layer.x - x).abs() < 0.005
                                && (layer.y - y).abs() < 0.005
                                && (layer.width - w).abs() < 0.005
                                && (layer.height - h).abs() < 0.005;
                            if theme::chip(ui, label, here, theme::ACCENT, Vec2::new(54.0, 19.0))
                                .clicked()
                            {
                                layer.x = x;
                                layer.y = y;
                                layer.width = w;
                                layer.height = h;
                                changed = true;
                            }
                        }
                    });

                    ui.add_space(4.0);
                    // The presets cover most of it; these are for the rest.
                    // As fractions of the frame, so a stack built at 1080p
                    // still lines up if the production is run at another size.
                    ui.horizontal(|ui| {
                        for (label, value, range) in [
                            ("X", &mut layer.x, -1.0..=1.0),
                            ("Y", &mut layer.y, -1.0..=1.0),
                            ("W", &mut layer.width, 0.05..=2.0),
                            ("H", &mut layer.height, 0.05..=2.0),
                        ] {
                            ui.label(RichText::new(label).font(theme::mono(9.5)).color(theme::TEXT_FAINT));
                            if ui
                                .add(
                                    egui::DragValue::new(value)
                                        .speed(0.004)
                                        .range(range)
                                        .fixed_decimals(3),
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.label(RichText::new("OPACITY").font(theme::mono(9.5)).color(theme::TEXT_FAINT));
                        if ui
                            .add(egui::Slider::new(&mut layer.opacity, 0.0..=1.0).show_value(false))
                            .changed()
                        {
                            changed = true;
                        }
                        ui.label(
                            RichText::new(format!("{:.0}%", layer.opacity * 100.0))
                                .font(theme::mono(9.5))
                                .color(theme::TEXT_DIM),
                        );
                        if theme::chip(
                            ui,
                            "KEEP SHAPE",
                            layer.preserve_aspect,
                            theme::PREVIEW,
                            Vec2::new(82.0, 19.0),
                        )
                        .on_hover_text("letterbox inside the rectangle rather than stretching to fill it")
                        .clicked()
                        {
                            layer.preserve_aspect = !layer.preserve_aspect;
                            changed = true;
                        }
                    });
                });
            ui.add_space(4.0);

            if changed {
                self.engine.send(Command::SetLayer { input: index, at, layer });
            }
        }
    }

    /// Per-input settings: name, position, zoom and colour.
    fn input_settings(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        let Some(index) = self.settings_for else { return };
        let Some(info) = snapshot.inputs.get(index) else {
            self.settings_for = None;
            return;
        };

        let mut open = true;
        let settings = info.settings;
        egui::Window::new(format!("Input {} settings", index + 1))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_width(440.0);
                ui.add_space(4.0);

                ui.horizontal(|ui| {
                    ui.label(RichText::new("TYPE").size(10.5).color(theme::TEXT_DIM));
                    ui.label(RichText::new(info.kind).font(theme::mono(11.0)).color(theme::ACCENT));
                });
                ui.add_space(8.0);

                ui.label(RichText::new("NAME").size(10.5).color(theme::TEXT_DIM));
                if ui
                    .add(egui::TextEdit::singleline(&mut self.settings_name).desired_width(f32::INFINITY))
                    .changed()
                {
                    self.engine.send(Command::RenameInput {
                        input: index,
                        name: self.settings_name.clone(),
                    });
                }

                if let Some(layers) = &info.layers {
                    ui.add_space(14.0);
                    ui.separator();
                    ui.add_space(8.0);
                    self.stack_editor(ui, index, layers, snapshot);
                }

                ui.add_space(14.0);
                ui.separator();
                ui.add_space(8.0);
                ui.label(RichText::new("POSITION").size(11.0).strong().color(theme::TEXT));
                ui.add_space(6.0);

                let mut zoom = settings.zoom;
                let mut offset_x = settings.offset_x;
                let mut offset_y = settings.offset_y;
                let mut moved = false;

                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{:<8}", "ZOOM")).font(theme::mono(10.5)).color(theme::TEXT_DIM));
                    moved |= ui
                        .add_sized(Vec2::new(260.0, 18.0), egui::Slider::new(&mut zoom, 0.25..=4.0).fixed_decimals(2))
                        .changed();
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{:<8}", "PAN X")).font(theme::mono(10.5)).color(theme::TEXT_DIM));
                    moved |= ui
                        .add_sized(Vec2::new(260.0, 18.0), egui::Slider::new(&mut offset_x, -1.0..=1.0).fixed_decimals(2))
                        .changed();
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{:<8}", "PAN Y")).font(theme::mono(10.5)).color(theme::TEXT_DIM));
                    moved |= ui
                        .add_sized(Vec2::new(260.0, 18.0), egui::Slider::new(&mut offset_y, -1.0..=1.0).fixed_decimals(2))
                        .changed();
                });
                if moved {
                    self.engine.send(Command::SetInputTransform { input: index, zoom, offset_x, offset_y });
                }

                ui.add_space(14.0);
                ui.separator();
                ui.add_space(8.0);
                ui.label(RichText::new("COLOUR ADJUST").size(11.0).strong().color(theme::TEXT));
                ui.add_space(6.0);

                let mut colour = settings.colour;
                let mut graded = false;
                for (label, value, range) in [
                    ("BRIGHT", &mut colour.brightness, -1.0..=1.0),
                    ("CONTRAST", &mut colour.contrast, 0.0..=2.0),
                    ("SAT", &mut colour.saturation, 0.0..=2.0),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{label:<9}")).font(theme::mono(10.5)).color(theme::TEXT_DIM));
                        graded |= ui
                            .add_sized(Vec2::new(260.0, 18.0), egui::Slider::new(value, range).fixed_decimals(2))
                            .changed();
                    });
                }
                if graded {
                    self.engine.send(Command::SetInputColour { input: index, colour });
                }

                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if theme::button(ui, "RESET", theme::SURFACE_HIGH, Vec2::new(90.0, 28.0)).clicked() {
                        self.engine.send(Command::ResetInputSettings(index));
                    }
                    if theme::button(ui, "AUDIO", theme::ACCENT, Vec2::new(90.0, 28.0))
                        .on_hover_text("open this input's channel in the mixer")
                        .clicked()
                    {
                        self.tab = Tab::Audio;
                        self.selected_channel = index;
                        self.settings_for = None;
                    }
                    if theme::button(ui, "CLOSE INPUT", theme::WARN, Vec2::new(110.0, 28.0)).clicked()
                        && snapshot.inputs.len() > 1
                    {
                        self.engine.send(Command::RemoveSource(index));
                        self.settings_for = None;
                    }
                });
                ui.add_space(4.0);
            });

        if !open {
            self.settings_for = None;
        }
    }

    /// Edits a title in place. Applying while it is on air is the point.
    fn title_editor(&mut self, ctx: &egui::Context) {
        let Some((index, mut text, mut subtitle)) = self.editing_title.clone() else {
            return;
        };
        let mut open = true;
        let mut apply = false;

        egui::Window::new("Title text")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_width(420.0);
                ui.add_space(4.0);
                ui.label(RichText::new("HEADLINE").size(10.5).color(theme::TEXT_DIM));
                ui.add(egui::TextEdit::singleline(&mut text).desired_width(f32::INFINITY));
                ui.add_space(8.0);
                ui.label(RichText::new("SUBTITLE").size(10.5).color(theme::TEXT_DIM));
                ui.add(egui::TextEdit::singleline(&mut subtitle).desired_width(f32::INFINITY));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if theme::button(ui, "APPLY", theme::PREVIEW, Vec2::new(100.0, 28.0)).clicked() {
                        apply = true;
                    }
                    if ui.button("Close").clicked() {
                        apply = false;
                    }
                });
                ui.add_space(4.0);
            });

        if apply {
            self.engine.send(Command::SetTitleText {
                input: index,
                text: text.clone(),
                subtitle: subtitle.clone(),
            });
        }
        if open {
            self.editing_title = Some((index, text, subtitle));
        } else {
            self.editing_title = None;
        }
    }

    /// Accepts files dropped on the window.
    ///
    /// Dropping a file is the fastest way to get something on air, so it is
    /// worth getting right: the file decides what kind of input it becomes,
    /// rather than the operator having to say.
    fn dropped_files(&mut self, ctx: &egui::Context) {
        // What is hovering, so the window can say what will happen before the
        // operator lets go.
        let hovering = ctx.input(|i| i.raw.hovered_files.len());
        if hovering > 0 {
            self.paint_drop_hint(ctx, hovering);
        }

        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect()
        });

        for path in dropped {
            let Some(text) = path.to_str() else { continue };
            let name = file_stem(text);

            self.engine.send(command_for(name, text.to_string()));
        }
    }

    /// An overlay saying what dropping will do.
    fn paint_drop_hint(&self, ctx: &egui::Context, count: usize) {
        let screen = ctx.screen_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("drop-hint"),
        ));

        painter.rect_filled(screen, Rounding::ZERO, theme::SURFACE.gamma_multiply(0.82));

        let box_size = Vec2::new(420.0, 120.0);
        let rect = Rect::from_center_size(screen.center(), box_size);
        painter.rect_filled(rect, Rounding::same(8.0), theme::SURFACE_CONTAINER);
        painter.rect_stroke(rect, Rounding::same(8.0), Stroke::new(2.0_f32, theme::ACCENT));

        painter.text(
            rect.center() - Vec2::new(0.0, 14.0),
            egui::Align2::CENTER_CENTER,
            if count == 1 { "Drop to add as an input".to_string() } else { format!("Drop to add {count} inputs") },
            egui::FontId::proportional(15.0),
            theme::TEXT,
        );
        painter.text(
            rect.center() + Vec2::new(0.0, 14.0),
            egui::Align2::CENTER_CENTER,
            "video · audio · images",
            egui::FontId::proportional(11.0),
            theme::TEXT_DIM,
        );
    }

    fn name_or(&self, fallback: &str) -> String {
        let name = self.new_source_name.trim();
        if name.is_empty() {
            fallback.to_string()
        } else {
            name.to_string()
        }
    }
}

/// Device names are long and repetitive; a channel strip is 62 px wide.
fn shorten(name: &str) -> String {
    let trimmed = name.split('(').next().unwrap_or(name).trim();
    if trimmed.is_empty() {
        name.chars().take(18).collect()
    } else {
        trimmed.chars().take(18).collect()
    }
}

/// Timecode from the engine frame count, so it counts production time rather
/// than wall clock and stops when the engine does.
fn timecode(frames: u64) -> String {
    let fps = target_fps() as u64;
    let seconds = frames / fps;
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60,
        frames % fps
    )
}

fn default_record_path() -> String {
    let dir = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".into());
    format!("{dir}\\rhevia-recording.h264")
}

/// Bytes at the scale a person reads them, which changes as a stream runs.
///
/// A show that has sent 4 GB should not be reported in megabytes.
fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let value = bytes as f64;
    if value >= GB {
        format!("{:.2} GB", value / GB)
    } else if value >= MB {
        format!("{:.1} MB", value / MB)
    } else if value >= KB {
        format!("{:.0} kB", value / KB)
    } else {
        format!("{bytes} B")
    }
}

/// What kind of input a file should become.
///
/// Stills are loaded directly: sending a single picture through the media
/// decoder would start a process to loop one frame forever.
///
/// Everything else goes to the media decoder, which reads far more than any
/// extension list could enumerate — so a file is never turned away on the
/// strength of its name. The exception is raw Annex-B with no ffmpeg
/// installed: that is the one format Rhevia plays natively, and falling back
/// to it is more useful than reporting that ffmpeg is missing.
fn command_for(name: String, path: String) -> Command {
    if is_image(&path) {
        return Command::AddImageSource { name, path };
    }
    let annexb = std::path::Path::new(&path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("h264") || e.eq_ignore_ascii_case("264"))
        .unwrap_or(false);

    if annexb && !rhevia_media::available() {
        Command::AddFileSource { name, path }
    } else {
        Command::AddMediaSource { name, path }
    }
}

/// The source part of an NDI name, for naming an input.
///
/// NDI names a source "MACHINE (Source name)". The machine is useful in the
/// picker, where several machines are listed together, and noise on a tile
/// sixty pixels wide.
fn short_ndi_name(full: &str) -> String {
    match (full.find('('), full.rfind(')')) {
        (Some(open), Some(close)) if close > open + 1 => full[open + 1..close].to_string(),
        _ => full.to_string(),
    }
}

/// Asks for a file with the system's own picker.
///
/// Typing a full path into a text field is not a reasonable way to add a
/// clip. It is also how a path gets a typo in it, and the error that follows
/// says the file does not exist, which is true and unhelpful.
///
/// Returns None when the dialog was cancelled.
fn pick_file(title: &str, filters: &[(&str, &[&str])]) -> Option<String> {
    let mut dialog = rfd::FileDialog::new().set_title(title);
    for (name, extensions) in filters {
        dialog = dialog.add_filter(*name, extensions);
    }
    dialog.pick_file().map(|path| path.display().to_string())
}

/// The file name without its directory or extension, for naming an input.
///
/// "opener" reads better on a tile than "D:\\clips\\opener.mp4", and a tile is
/// about sixty pixels wide.
fn file_stem(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Media".to_string())
}

/// Whether a dropped file is a still rather than something to decode.
///
/// Stills are loaded directly: sending them through the media decoder would
/// start a process to loop a single frame forever.
fn is_image(path: &str) -> bool {
    const EXTENSIONS: [&str; 8] = ["png", "jpg", "jpeg", "bmp", "gif", "webp", "tif", "tiff"];
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn format_duration(seconds: u64) -> String {
    let (h, m, s) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a dropped file becomes. Getting this wrong is visible and
    /// annoying: a song that opens as a still picture, or a photo that starts
    /// a decoder process to loop one frame forever.
    mod routing {
        use super::*;

        fn kind(path: &str) -> &'static str {
            match command_for("x".into(), path.into()) {
                Command::AddImageSource { .. } => "image",
                Command::AddMediaSource { .. } => "media",
                Command::AddFileSource { .. } => "annexb",
                _ => "other",
            }
        }

        #[test]
        fn stills_are_loaded_directly_rather_than_decoded() {
            for path in ["slide.png", "logo.PNG", "photo.jpg", "sponsor.webp", "scan.tiff"] {
                assert_eq!(kind(path), "image", "{path} should be a still");
            }
        }

        #[test]
        fn video_and_audio_go_to_the_media_decoder() {
            for path in ["opener.mp4", "show.MKV", "walk-in.mp3", "bed.flac", "sting.mov"] {
                assert_eq!(kind(path), "media", "{path} should be decoded");
            }
        }

        #[test]
        fn an_unknown_extension_is_still_attempted() {
            // ffmpeg reads far more than any list here could name. Turning a
            // file away because of its extension would refuse working files.
            assert_eq!(kind("recording.mxf"), "media");
            assert_eq!(kind("no-extension"), "media");
        }

        #[test]
        fn the_name_and_path_survive_routing() {
            let command = command_for("Opener".into(), r"D:\clips\opener.mp4".into());
            match command {
                Command::AddMediaSource { name, path } => {
                    assert_eq!(name, "Opener");
                    assert_eq!(path, r"D:\clips\opener.mp4");
                }
                other => panic!("expected media, got {other:?}"),
            }
        }
    }

    mod naming {
        use super::*;

        #[test]
        fn an_input_is_named_after_the_file_not_its_path() {
            // A tile is about sixty pixels wide; a full path is unreadable.
            assert_eq!(file_stem(r"D:\clips\opener.mp4"), "opener");
            assert_eq!(file_stem("/home/user/walk-in music.mp3"), "walk-in music");
            assert_eq!(file_stem("sting.mov"), "sting");
        }

        #[test]
        fn something_with_no_usable_name_still_gets_one() {
            assert_eq!(file_stem(""), "Media");
            assert_eq!(file_stem("   "), "Media");
        }

        #[test]
        fn a_file_with_no_extension_keeps_its_whole_name() {
            assert_eq!(file_stem("README"), "README");
        }
    }

    mod byte_sizes {
        use super::*;

        #[test]
        fn a_stream_is_reported_at_a_scale_a_person_reads() {
            // A show that has sent four gigabytes should not be reported in
            // megabytes, and a show that has just started should not read as
            // 0.00 GB.
            assert_eq!(format_bytes(0), "0 B");
            assert_eq!(format_bytes(512), "512 B");
            assert_eq!(format_bytes(2048), "2 kB");
            assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
            assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.00 GB");
        }

        #[test]
        fn the_scale_changes_at_the_boundary_not_past_it() {
            assert!(format_bytes(1023).ends_with('B'));
            assert!(format_bytes(1024).ends_with("kB"));
        }
    }
}

#[cfg(test)]
mod ndi_naming {
    use super::*;

    #[test]
    fn an_input_is_named_after_the_source_not_the_machine() {
        // NDI names a source "MACHINE (Source name)". The machine matters in
        // the picker, where several are listed together, and is noise on a
        // tile sixty pixels wide.
        assert_eq!(short_ndi_name("STUDIO-PC (Camera 1)"), "Camera 1");
        assert_eq!(short_ndi_name("BK (Rhevia Programme)"), "Rhevia Programme");
    }

    #[test]
    fn a_name_in_an_unexpected_shape_is_kept_whole() {
        // Better a long name than an empty one.
        assert_eq!(short_ndi_name("Plain Name"), "Plain Name");
        assert_eq!(short_ndi_name("MACHINE ()"), "MACHINE ()");
        assert_eq!(short_ndi_name(""), "");
    }

    #[test]
    fn a_source_name_containing_brackets_survives() {
        assert_eq!(short_ndi_name("PC (Camera (left))"), "Camera (left)");
    }
}
