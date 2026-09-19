//! Rhevia Studio's interface.
//!
//! Laid out the way a switcher is actually operated. Preview and Program at the
//! top with the transition controls between them, because that is the axis your
//! eyes and hands work along. Every input carries its own controls rather than
//! requiring a selection first — a director calls "cut to two" and the button
//! for two must already be under your finger.
//!
//! The UI holds no production state. It renders a snapshot and sends commands,
//! so everything here is equally reachable from a script or a remote control.

use std::collections::HashMap;

use eframe::egui::{self, RichText, Vec2};
use rhevia_engine::Frame;

use crate::engine::{Command, EngineHandle, Layout, Snapshot};
use crate::theme;

pub struct StudioApp {
    engine: EngineHandle,
    textures: HashMap<String, egui::TextureHandle>,
    rtmp_url: String,
    stream_key: String,
    record_path: String,
    new_source_name: String,
    file_path: String,
    show_add_source: bool,
    show_stream_settings: bool,
    /// Which overlay slot the next input click assigns to, if any.
    assigning_overlay: Option<usize>,
}

impl StudioApp {
    pub fn new(cc: &eframe::CreationContext<'_>, engine: EngineHandle) -> Self {
        theme::apply(&cc.egui_ctx);
        Self {
            engine,
            textures: HashMap::new(),
            rtmp_url: "rtmp://a.rtmp.youtube.com/live2".into(),
            stream_key: String::new(),
            record_path: default_record_path(),
            new_source_name: String::new(),
            file_path: String::new(),
            show_add_source: false,
            show_stream_settings: false,
            assigning_overlay: None,
        }
    }

    fn texture(&mut self, ctx: &egui::Context, key: &str, frame: &Frame) -> Option<egui::TextureId> {
        if frame.is_empty() {
            return None;
        }
        let image =
            egui::ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.data);
        match self.textures.get_mut(key) {
            Some(handle) => handle.set(image, egui::TextureOptions::LINEAR),
            None => {
                let handle = ctx.load_texture(key, image, egui::TextureOptions::LINEAR);
                self.textures.insert(key.to_string(), handle);
            }
        }
        self.textures.get(key).map(|h| h.id())
    }
}

impl eframe::App for StudioApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let snapshot = self.engine.snapshot();
        ctx.request_repaint_after(std::time::Duration::from_millis(16));

        self.keyboard(ctx, &snapshot);
        self.top_bar(ctx, &snapshot);
        self.status_bar(ctx, &snapshot);
        self.toolbar(ctx, &snapshot);
        self.inputs(ctx, &snapshot);
        self.monitors(ctx, &snapshot);
        self.dialogs(ctx);
    }
}

impl StudioApp {
    /// Shortcuts an operator's hands already know. Numbers preview, Ctrl+number
    /// cuts straight to air, space is CUT, Enter is AUTO — the same muscle
    /// memory every switcher trains.
    fn keyboard(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
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
                egui::Key::Num1,
                egui::Key::Num2,
                egui::Key::Num3,
                egui::Key::Num4,
                egui::Key::Num5,
                egui::Key::Num6,
                egui::Key::Num7,
                egui::Key::Num8,
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

    fn top_bar(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        egui::TopBottomPanel::top("top")
            .exact_height(54.0)
            .frame(theme::panel(theme::BG_CHROME))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add_space(16.0);
                    ui.label(RichText::new("RHEVIA").size(19.0).strong().color(theme::ACCENT));
                    ui.label(RichText::new("STUDIO").size(19.0).color(theme::TEXT_DIM));
                    ui.add_space(20.0);

                    if snapshot.ftb {
                        ui.label(RichText::new("■ FADED TO BLACK").size(15.0).strong().color(theme::ACCENT));
                    } else if snapshot.streaming {
                        let pulse = (ui.input(|i| i.time) * 2.0).sin() as f32 * 0.25 + 0.75;
                        ui.label(
                            RichText::new("● ON AIR")
                                .size(15.0)
                                .strong()
                                .color(theme::PROGRAM.gamma_multiply(pulse)),
                        );
                    } else {
                        ui.label(RichText::new("○ OFF AIR").size(15.0).color(theme::TEXT_DIM));
                    }

                    if snapshot.recording {
                        ui.add_space(14.0);
                        ui.label(
                            RichText::new(format!(
                                "⏺ REC  {:.1} MB",
                                snapshot.recorded_bytes as f64 / 1_048_576.0
                            ))
                            .size(14.0)
                            .strong()
                            .color(theme::PROGRAM),
                        );
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(16.0);
                        let (label, colour) = if snapshot.streaming {
                            ("STOP STREAM", theme::PROGRAM)
                        } else {
                            ("START STREAM", theme::GO)
                        };
                        if theme::button(ui, label, colour, Vec2::new(142.0, 32.0)).clicked() {
                            if snapshot.streaming {
                                self.engine.send(Command::StopStream);
                            } else {
                                self.show_stream_settings = true;
                            }
                        }
                        ui.add_space(18.0);

                        let s = snapshot.stats;
                        if snapshot.streaming {
                            let kbps = (s.bytes_sent * 8 / s.uptime_seconds.max(1)) / 1000;
                            theme::metric(ui, "UPTIME", &format_duration(s.uptime_seconds));
                            theme::metric(ui, "BITRATE", &format!("{kbps} kb/s"));
                        }
                        theme::metric(ui, "FPS", &format!("{:.0}", s.fps));
                    });
                });
            });
    }

    fn status_bar(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        egui::TopBottomPanel::bottom("status")
            .exact_height(26.0)
            .frame(theme::panel(theme::BG_CHROME))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add_space(14.0);
                    // Engine health first: if this thread has died, every
                    // number to the right of it is stale and the show is off
                    // air whatever else the screen says.
                    if self.engine.is_running() {
                        ui.label(RichText::new("●").size(11.5).color(theme::GO));
                    } else {
                        ui.label(
                            RichText::new("● ENGINE STOPPED")
                                .size(11.5)
                                .strong()
                                .color(theme::PROGRAM),
                        );
                    }
                    ui.add_space(8.0);

                    let s = snapshot.stats;
                    // The numbers vMix puts here, because they are the ones
                    // that tell you whether the machine is coping.
                    ui.label(
                        RichText::new(format!(
                            "FPS {:.0}   ·   Render {:.1} ms   ·   Inputs {}   ·   Rendered {}   ·   Encoded {}",
                            s.fps, s.render_ms, snapshot.inputs.len(), s.frames_rendered, s.frames_encoded
                        ))
                        .size(11.5)
                        .color(theme::TEXT_DIM),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(14.0);
                        if let Some(error) = &snapshot.stream_error {
                            ui.label(RichText::new(format!("⚠ {error}")).size(11.5).color(theme::PROGRAM));
                        } else if let Some(assigning) = self.assigning_overlay {
                            ui.label(
                                RichText::new(format!("click an input to assign overlay {}", assigning + 1))
                                    .size(11.5)
                                    .color(theme::ACCENT),
                            );
                        } else {
                            ui.label(
                                RichText::new("1-8 preview  ·  ctrl+1-8 cut  ·  space CUT  ·  enter AUTO  ·  esc FTB")
                                    .size(11.5)
                                    .color(theme::TEXT_DIM),
                            );
                        }
                    });
                });
            });
    }

    /// The bottom toolbar: the actions that are not about a single input.
    fn toolbar(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        egui::TopBottomPanel::bottom("toolbar")
            .exact_height(56.0)
            .frame(theme::panel(theme::BG_CHROME))
            .show(ctx, |ui| {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.add_space(14.0);

                    if theme::button(ui, "+ ADD INPUT", theme::BG_RAISED, Vec2::new(116.0, 34.0)).clicked() {
                        self.show_add_source = true;
                    }
                    ui.add_space(8.0);

                    let (rec_label, rec_colour) = if snapshot.recording {
                        ("■ STOP REC", theme::PROGRAM)
                    } else {
                        ("⏺ RECORD", theme::BG_RAISED)
                    };
                    if theme::button(ui, rec_label, rec_colour, Vec2::new(104.0, 34.0)).clicked() {
                        if snapshot.recording {
                            self.engine.send(Command::StopRecording);
                        } else {
                            self.engine.send(Command::StartRecording {
                                path: self.record_path.clone(),
                            });
                        }
                    }

                    ui.add_space(18.0);
                    theme::divider(ui, 34.0);
                    ui.add_space(18.0);

                    ui.label(RichText::new("LAYOUT").size(10.0).strong().color(theme::TEXT_DIM));
                    ui.add_space(6.0);
                    for option in Layout::ALL {
                        let active = snapshot.layout == option;
                        let colour = if active { theme::ACCENT } else { theme::BG_RAISED };
                        if theme::button(ui, option.label(), colour, Vec2::new(64.0, 34.0)).clicked() {
                            self.engine.send(Command::SetLayout(option));
                        }
                        ui.add_space(4.0);
                    }

                    ui.add_space(14.0);
                    theme::divider(ui, 34.0);
                    ui.add_space(14.0);

                    ui.label(RichText::new("OVERLAY").size(10.0).strong().color(theme::TEXT_DIM));
                    ui.add_space(6.0);
                    for slot in 0..4 {
                        let assigned = snapshot.overlay_source[slot].is_some();
                        let on = snapshot.overlay_on[slot];
                        let colour = if on {
                            theme::PROGRAM
                        } else if assigned {
                            theme::BG_RAISED
                        } else {
                            theme::BG_PANEL
                        };
                        let response =
                            theme::button(ui, &format!("{}", slot + 1), colour, Vec2::new(40.0, 34.0));
                        if response.clicked() {
                            if assigned {
                                self.engine.send(Command::ToggleOverlay(slot));
                            } else {
                                // Nothing assigned yet, so arm assignment
                                // rather than doing nothing and looking broken.
                                self.assigning_overlay = Some(slot);
                            }
                        }
                        if response.secondary_clicked() {
                            self.assigning_overlay = Some(slot);
                        }
                        response.on_hover_text(if assigned {
                            "click to toggle on air · right-click to reassign"
                        } else {
                            "click to assign a source"
                        });
                        ui.add_space(4.0);
                    }
                });
            });
    }

    /// The input row: every source with its own controls.
    fn inputs(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        egui::TopBottomPanel::bottom("inputs")
            .exact_height(216.0)
            .frame(theme::panel(theme::BG_PANEL))
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    ui.label(RichText::new("INPUTS").size(10.0).strong().color(theme::TEXT_DIM));
                });
                ui.add_space(4.0);

                egui::ScrollArea::horizontal().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        for index in 0..snapshot.inputs.len() {
                            self.input_tile(ctx, ui, snapshot, index);
                            ui.add_space(10.0);
                        }
                    });
                });
            });
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

        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);

            let picture = theme::input_picture(
                ui,
                &format!("{}  {}", index + 1, info.name),
                texture,
                on_program,
                on_preview,
            );
            if picture.clicked() {
                if let Some(slot) = self.assigning_overlay.take() {
                    self.engine.send(Command::SetOverlaySource { slot, input: index });
                } else {
                    self.engine.send(Command::SetPreview(index));
                }
            }

            // Per-input actions, as on a real switcher: the button for input
            // three is always in the same place under input three.
            ui.horizontal(|ui| {
                if theme::button(ui, "CUT", theme::PROGRAM, Vec2::new(56.0, 22.0)).clicked() {
                    self.engine.send(Command::CutTo(index));
                }
                if theme::button(ui, "PVW", theme::PREVIEW, Vec2::new(56.0, 22.0)).clicked() {
                    self.engine.send(Command::SetPreview(index));
                }
                let removable = snapshot.inputs.len() > 1;
                let colour = if removable { theme::BG_RAISED } else { theme::BG_PANEL };
                if theme::button(ui, "✕", colour, Vec2::new(28.0, 22.0)).clicked() && removable {
                    self.engine.send(Command::RemoveSource(index));
                }
            });

            ui.horizontal(|ui| {
                for slot in 0..4 {
                    let assigned = snapshot.overlay_source[slot] == Some(index);
                    let live = assigned && snapshot.overlay_on[slot];
                    let colour = if live {
                        theme::PROGRAM
                    } else if assigned {
                        theme::ACCENT
                    } else {
                        theme::BG_RAISED
                    };
                    if theme::button(ui, &format!("{}", slot + 1), colour, Vec2::new(34.0, 20.0))
                        .on_hover_text(format!("overlay {} with this input", slot + 1))
                        .clicked()
                    {
                        self.engine.send(Command::SetOverlaySource { slot, input: index });
                        self.engine.send(Command::ToggleOverlay(slot));
                    }
                    ui.add_space(2.0);
                }
            });
        });
    }

    fn monitors(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        egui::CentralPanel::default()
            .frame(theme::panel(theme::BG_DARK))
            .show(ctx, |ui| {
                let available = ui.available_size();
                let column = 104.0;
                let gap = 10.0;
                let monitor = Vec2::new(
                    ((available.x - column - gap * 4.0) / 2.0).max(160.0),
                    (available.y - gap * 2.0).max(120.0),
                );

                ui.add_space(gap);
                ui.horizontal(|ui| {
                    ui.add_space(gap);

                    let preview_frame = snapshot
                        .inputs
                        .get(snapshot.preview_input)
                        .and_then(|i| i.thumbnail.clone());
                    let preview_texture = preview_frame
                        .as_ref()
                        .and_then(|f| self.texture(ctx, "preview", f));
                    let preview_name = snapshot
                        .inputs
                        .get(snapshot.preview_input)
                        .map(|i| i.name.as_str())
                        .unwrap_or("—");
                    theme::monitor(ui, "PREVIEW", preview_name, preview_texture, theme::PREVIEW, monitor);

                    ui.add_space(gap);

                    // The transition column, between the two monitors where a
                    // hardware panel puts it.
                    ui.vertical(|ui| {
                        ui.set_width(column);
                        ui.add_space(18.0);

                        if theme::button(ui, "CUT", theme::PROGRAM, Vec2::new(column, 46.0)).clicked() {
                            self.engine.send(Command::Cut);
                        }
                        ui.add_space(6.0);
                        let auto_colour = if snapshot.transition.is_some() {
                            theme::ACCENT
                        } else {
                            theme::BG_RAISED
                        };
                        if theme::button(ui, "AUTO", auto_colour, Vec2::new(column, 46.0)).clicked() {
                            self.engine.send(Command::Auto);
                        }
                        ui.add_space(10.0);

                        theme::vertical_t_bar(
                            ui,
                            snapshot.transition.unwrap_or(0.0),
                            Vec2::new(column, 150.0),
                        );

                        ui.add_space(10.0);
                        ui.label(RichText::new("DURATION").size(9.0).color(theme::TEXT_DIM));
                        let mut seconds = snapshot.transition_seconds;
                        if ui
                            .add_sized(
                                Vec2::new(column, 18.0),
                                egui::Slider::new(&mut seconds, 0.2..=5.0)
                                    .show_value(true)
                                    .fixed_decimals(1),
                            )
                            .changed()
                        {
                            self.engine.send(Command::SetTransitionSeconds(seconds));
                        }

                        ui.add_space(12.0);
                        let ftb_colour = if snapshot.ftb { theme::ACCENT } else { theme::BG_RAISED };
                        if theme::button(ui, "FTB", ftb_colour, Vec2::new(column, 38.0)).clicked() {
                            self.engine.send(Command::ToggleFtb);
                        }
                    });

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
                    theme::monitor(ui, "PROGRAM", program_name, program_texture, theme::PROGRAM, monitor);
                });
            });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if self.show_stream_settings {
            let mut open = true;
            egui::Window::new("Stream destination")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.set_min_width(460.0);
                    ui.add_space(4.0);
                    ui.label(RichText::new("RTMP URL").size(11.0).color(theme::TEXT_DIM));
                    ui.add(egui::TextEdit::singleline(&mut self.rtmp_url).desired_width(f32::INFINITY));
                    ui.add_space(8.0);
                    ui.label(RichText::new("STREAM KEY").size(11.0).color(theme::TEXT_DIM));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.stream_key)
                            .password(true)
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            "The key may also be part of the URL. Both forms work, because \
                             platforms present them differently.",
                        )
                        .size(11.0)
                        .color(theme::TEXT_DIM),
                    );
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if theme::button(ui, "GO LIVE", theme::GO, Vec2::new(120.0, 32.0)).clicked() {
                            self.engine.send(Command::StartStream {
                                url: self.rtmp_url.clone(),
                                key: self.stream_key.clone(),
                            });
                            self.show_stream_settings = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.show_stream_settings = false;
                        }
                    });
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(8.0);
                    ui.label(RichText::new("RECORD TO").size(11.0).color(theme::TEXT_DIM));
                    ui.add(egui::TextEdit::singleline(&mut self.record_path).desired_width(f32::INFINITY));
                    ui.label(
                        RichText::new("Annex-B H.264. Remux with: ffmpeg -i rec.h264 -c copy rec.mp4")
                            .size(10.0)
                            .monospace()
                            .color(theme::TEXT_DIM),
                    );
                    ui.add_space(4.0);
                });
            if !open {
                self.show_stream_settings = false;
            }
        }

        if self.show_add_source {
            let mut open = true;
            egui::Window::new("Add input")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.set_min_width(460.0);
                    ui.add_space(4.0);
                    ui.label(RichText::new("NAME").size(11.0).color(theme::TEXT_DIM));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.new_source_name)
                            .hint_text("Camera 2")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("Colour bars").clicked() {
                            self.engine.send(Command::AddBarsSource { name: self.name_or("Bars") });
                            self.show_add_source = false;
                        }
                        if ui.button("Solid colour").clicked() {
                            self.engine.send(Command::AddColourSource {
                                name: self.name_or("Colour"),
                                rgb: [160, 40, 90],
                            });
                            self.show_add_source = false;
                        }
                    });

                    ui.add_space(14.0);
                    ui.separator();
                    ui.add_space(8.0);
                    ui.label(RichText::new("H.264 FILE (Annex-B, loops)").size(11.0).color(theme::TEXT_DIM));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.file_path)
                            .hint_text(r"C:\clips\opener.h264")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("ffmpeg -i in.mp4 -c:v libx264 -bsf:v h264_mp4toannexb -f h264 out.h264")
                            .size(10.0)
                            .monospace()
                            .color(theme::TEXT_DIM),
                    );
                    ui.add_space(10.0);
                    if ui.button("Add file").clicked() && !self.file_path.trim().is_empty() {
                        self.engine.send(Command::AddFileSource {
                            name: self.name_or("Clip"),
                            path: self.file_path.trim().to_string(),
                        });
                        self.show_add_source = false;
                    }
                    ui.add_space(4.0);
                });
            if !open {
                self.show_add_source = false;
            }
        }
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

fn default_record_path() -> String {
    let dir = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".into());
    format!("{dir}\\rhevia-recording.h264")
}

fn format_duration(seconds: u64) -> String {
    let (h, m, s) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}
