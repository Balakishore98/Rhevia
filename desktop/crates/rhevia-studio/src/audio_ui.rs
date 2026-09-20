//! The audio mixer.
//!
//! Two presentations of the same state: a compact strip row along the bottom
//! of the switcher, always visible because level is something you watch rather
//! than visit, and a full view on its own tab with room for pan and routing.
//!
//! Meters are the part that has to be right. An operator reads level from
//! colour and position long before they read a number, so the zones follow
//! broadcast convention: green is fine, amber is approaching, red is trouble,
//! and a clip latches until acknowledged rather than flashing past unseen.

use eframe::egui::{self, Color32, Rect, RichText, Rounding, Sense, Stroke, Ui, Vec2};

use crate::engine::{ChannelState, Command, EngineHandle, MasterState, Snapshot};
use crate::theme;

/// Meter range. Below this a signal is inaudible anyway, and showing more only
/// compresses the region that matters.
const METER_MIN_DB: f32 = -60.0;
const METER_MAX_DB: f32 = 6.0;

fn position(db: f32) -> f32 {
    ((db - METER_MIN_DB) / (METER_MAX_DB - METER_MIN_DB)).clamp(0.0, 1.0)
}

fn zone_colour(db: f32) -> Color32 {
    if db >= -3.0 {
        theme::PROGRAM
    } else if db >= -12.0 {
        theme::WARN
    } else {
        theme::PREVIEW
    }
}

/// A vertical peak-and-RMS meter.
///
/// RMS is the solid bar because it tracks what you hear; peak is a line above
/// it because it tells you how close you are to clipping. Showing only one
/// leaves you either deaf to transients or unable to judge loudness.
pub fn meter(ui: &mut Ui, peak_db: f32, rms_db: f32, clipped: bool, size: Vec2) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter();

    painter.rect_filled(rect, Rounding::same(2.0_f32), theme::SURFACE_LOWEST);

    for &mark in &[0.0_f32, -6.0, -12.0, -20.0, -40.0] {
        let y = rect.max.y - rect.height() * position(mark);
        painter.line_segment(
            [egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)],
            Stroke::new(1.0_f32, theme::SURFACE_HIGH),
        );
    }

    let rms_height = rect.height() * position(rms_db);
    if rms_height > 0.5 {
        let bar = Rect::from_min_max(
            egui::pos2(rect.min.x + 1.0, rect.max.y - rms_height),
            egui::pos2(rect.max.x - 1.0, rect.max.y),
        );
        painter.rect_filled(bar, Rounding::ZERO, zone_colour(rms_db));
    }

    if peak_db > METER_MIN_DB {
        let peak_y = rect.max.y - rect.height() * position(peak_db);
        painter.line_segment(
            [egui::pos2(rect.min.x + 1.0, peak_y), egui::pos2(rect.max.x - 1.0, peak_y)],
            Stroke::new(2.0_f32, zone_colour(peak_db)),
        );
    }

    // Clip light at the top, latched. Clicking clears it.
    let light = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 4.0));
    painter.rect_filled(
        light,
        Rounding::same(2.0_f32),
        if clipped { theme::PROGRAM } else { theme::SURFACE_HIGH },
    );

    painter.rect_stroke(rect, Rounding::same(2.0_f32), Stroke::new(1.0_f32, theme::SURFACE_HIGH));
    response
}

/// A vertical fader. Returns the new value while being dragged.
pub fn fader(ui: &mut Ui, db: f32, size: Vec2) -> (egui::Response, Option<f32>) {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let painter = ui.painter();

    let track = Rect::from_center_size(rect.center(), Vec2::new(4.0, rect.height()));
    painter.rect_filled(track, Rounding::same(2.0_f32), theme::SURFACE_LOWEST);

    // Unity mark, so 0 dB can be found without reading the number.
    let unity_y = rect.max.y - rect.height() * position(0.0);
    painter.line_segment(
        [egui::pos2(rect.min.x, unity_y), egui::pos2(rect.max.x, unity_y)],
        Stroke::new(1.0_f32, theme::TEXT_FAINT),
    );

    let knob_y = rect.max.y - rect.height() * position(db);
    let knob = Rect::from_center_size(
        egui::pos2(rect.center().x, knob_y),
        Vec2::new(rect.width() - 2.0, 12.0),
    );
    painter.rect_filled(
        knob,
        Rounding::same(2.0_f32),
        if response.dragged() { theme::ACCENT } else { Color32::from_rgb(180, 186, 198) },
    );
    painter.line_segment(
        [
            egui::pos2(knob.min.x + 2.0, knob.center().y),
            egui::pos2(knob.max.x - 2.0, knob.center().y),
        ],
        Stroke::new(1.0_f32, theme::SURFACE_LOWEST),
    );

    let changed = if response.dragged() || response.clicked() {
        response.interact_pointer_pos().map(|pos| {
            let fraction = ((rect.max.y - pos.y) / rect.height()).clamp(0.0, 1.0);
            let value = METER_MIN_DB + fraction * (METER_MAX_DB - METER_MIN_DB);
            // Snap near unity: operators return to it constantly, and a fader
            // that will not settle there is maddening.
            if value.abs() < 1.0 {
                0.0
            } else {
                value
            }
        })
    } else {
        None
    };

    (response, changed)
}

/// A horizontal pan control.
fn pan_control(ui: &mut Ui, pan: f32, width: f32) -> (egui::Response, Option<f32>) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 13.0), Sense::click_and_drag());
    let painter = ui.painter();

    painter.rect_filled(rect, Rounding::same(3.0_f32), theme::SURFACE_LOWEST);
    // Centre detent, so straight-ahead is findable without looking.
    painter.line_segment(
        [
            egui::pos2(rect.center().x, rect.min.y + 2.0),
            egui::pos2(rect.center().x, rect.max.y - 2.0),
        ],
        Stroke::new(1.0_f32, theme::TEXT_FAINT),
    );

    let x = rect.min.x + rect.width() * ((pan.clamp(-1.0, 1.0) + 1.0) / 2.0);
    let knob = Rect::from_center_size(egui::pos2(x, rect.center().y), Vec2::new(6.0, 10.0));
    painter.rect_filled(knob, Rounding::same(2.0_f32), Color32::from_rgb(180, 186, 198));
    painter.rect_stroke(rect, Rounding::same(3.0_f32), Stroke::new(1.0_f32, theme::SURFACE_HIGH));

    let changed = if response.dragged() || response.clicked() {
        response.interact_pointer_pos().map(|pos| {
            let value = ((pos.x - rect.min.x) / rect.width() * 2.0 - 1.0).clamp(-1.0, 1.0);
            if value.abs() < 0.08 {
                0.0
            } else {
                value
            }
        })
    } else {
        None
    };

    (response, changed)
}

/// One channel strip. `tall` gives the full view more fader travel and a pan
/// control; the compact row omits pan for space.
fn strip(
    ui: &mut Ui,
    index: usize,
    channel: &ChannelState,
    engine: &EngineHandle,
    tall: bool,
    selected: Option<&mut usize>,
) {
    let fader_height = if tall { 150.0 } else { 74.0 };
    let is_selected = selected.as_ref().map(|s| **s == index).unwrap_or(false);

    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);
        ui.set_width(66.0);

        let name: String = channel.name.chars().take(10).collect();
        if let Some(selected) = selected {
            // On the full view the name selects which channel the DSP panel
            // below is editing.
            if theme::chip(ui, &name, is_selected, theme::ACCENT, Vec2::new(63.0, 18.0)).clicked() {
                *selected = index;
            }
        } else {
            ui.label(
                RichText::new(name)
                    .size(9.5)
                    .color(if channel.has_source { theme::TEXT } else { theme::TEXT_FAINT }),
            );
        }
        ui.label(
            RichText::new(format!("{:+.1} dB", channel.gain_db))
                .font(theme::mono(9.5))
                .color(if channel.muted { theme::PROGRAM } else { theme::PREVIEW }),
        );

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);
            let (response, changed) = fader(ui, channel.gain_db, Vec2::new(26.0, fader_height));
            if let Some(db) = changed {
                engine.send(Command::SetChannelGain { channel: index, db });
            }
            response.on_hover_text("drag to set level · snaps at 0 dB");

            if meter(
                ui,
                channel.peak_db,
                channel.rms_db,
                channel.clipped,
                Vec2::new(13.0, fader_height),
            )
            .on_hover_text(if channel.clipped { "clipped — click to clear" } else { "level" })
            .clicked()
            {
                engine.send(Command::ClearClip(index));
            }
        });

        if tall {
            let (pan_response, pan_changed) = pan_control(ui, channel.pan, 63.0);
            if let Some(pan) = pan_changed {
                engine.send(Command::SetPan { channel: index, pan });
            }
            pan_response.on_hover_text(match channel.pan {
                p if p < -0.05 => format!("pan {:.0}% left", -p * 100.0),
                p if p > 0.05 => format!("pan {:.0}% right", p * 100.0),
                _ => "centre".to_string(),
            });
        }

        if tall {
            // A one-line summary of the chain, so its state is visible without
            // selecting the channel.
            let mut active: Vec<&str> = Vec::new();
            if channel.gate.enabled {
                active.push("G");
            }
            if channel.eq.enabled {
                active.push("EQ");
            }
            if channel.compressor.enabled {
                active.push("C");
            }
            if channel.delay_ms > 0.0 {
                active.push("D");
            }
            ui.label(
                RichText::new(if active.is_empty() { "—".to_string() } else { active.join(" ") })
                    .font(theme::mono(9.0))
                    .color(if active.is_empty() { theme::TEXT_FAINT } else { theme::ACCENT }),
            );
        }

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);
            if theme::chip(ui, "M", channel.muted, theme::PROGRAM, Vec2::new(19.0, 19.0))
                .on_hover_text("mute")
                .clicked()
            {
                engine.send(Command::ToggleMute(index));
            }
            if theme::chip(ui, "S", channel.solo, theme::WARN, Vec2::new(19.0, 19.0))
                .on_hover_text("solo")
                .clicked()
            {
                engine.send(Command::ToggleSolo(index));
            }
            // Following Program is the difference between a camera microphone
            // and a presenter microphone, so it belongs on the strip.
            if theme::chip(ui, "F", channel.follow_program, theme::PREVIEW, Vec2::new(19.0, 19.0))
                .on_hover_text("follow Program: audio only while this input is on air")
                .clicked()
            {
                engine.send(Command::ToggleFollowProgram(index));
            }
        });
    });
}

fn master_strip(ui: &mut Ui, master: &MasterState, engine: &EngineHandle, tall: bool) {
    let fader_height = if tall { 160.0 } else { 74.0 };

    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);
        ui.set_width(72.0);

        ui.label(RichText::new("MASTER L/R").size(9.5).strong().color(theme::ACCENT));
        ui.label(
            RichText::new(format!("{:+.1} dB", master.gain_db))
                .font(theme::mono(9.5))
                .color(if master.muted { theme::PROGRAM } else { theme::PREVIEW }),
        );

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);
            let (_, changed) = fader(ui, master.gain_db, Vec2::new(28.0, fader_height));
            if let Some(db) = changed {
                engine.send(Command::SetMasterGain(db));
            }
            if meter(
                ui,
                master.peak_db,
                master.rms_db,
                master.clipped,
                Vec2::new(17.0, fader_height),
            )
            .on_hover_text(if master.clipped { "clipped — click to clear" } else { "master level" })
            .clicked()
            {
                engine.send(Command::ClearClip(usize::MAX));
            }
        });

        if tall {
            ui.add_space(13.0);
        }

        if theme::chip(ui, "MUTE", master.muted, theme::PROGRAM, Vec2::new(69.0, 19.0)).clicked() {
            engine.send(Command::ToggleMasterMute);
        }
    });
}

/// The compact row along the bottom of the switcher.
pub fn strip_row(
    ctx: &egui::Context,
    snapshot: &Snapshot,
    engine: &EngineHandle,
    show_devices: &mut bool,
) {
    egui::TopBottomPanel::bottom("audio-row")
        .exact_height(188.0)
        .frame(theme::panel(theme::SURFACE_CONTAINER))
        .show(ctx, |ui| {
            ui.add_space(7.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(RichText::new("AUDIO").size(11.0).strong().color(theme::TEXT));
                ui.add_space(8.0);
                let live = snapshot.audio.iter().filter(|c| c.has_source).count();
                ui.label(
                    RichText::new(format!("{live} WITH DEVICE"))
                        .font(theme::mono(9.5))
                        .color(theme::TEXT_FAINT),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(12.0);
                    if theme::button(ui, "+ DEVICE", theme::SURFACE_HIGH, Vec2::new(84.0, 20.0))
                        .on_hover_text("add a microphone or line input")
                        .clicked()
                    {
                        *show_devices = true;
                    }
                });
            });
            ui.add_space(5.0);

            ui.horizontal(|ui| {
                ui.add_space(12.0);
                master_strip(ui, &snapshot.master, engine, false);
                ui.add_space(6.0);
                theme::divider(ui, 140.0);
                ui.add_space(6.0);

                egui::ScrollArea::horizontal()
                    .id_salt("audio-row-strips")
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for (index, channel) in snapshot.audio.iter().enumerate() {
                                strip(ui, index, channel, engine, false, None);
                                ui.add_space(5.0);
                            }
                        });
                    });
            });
        });
}

/// A labelled slider that reports only when the value actually moves.
fn control(ui: &mut Ui, label: &str, value: f32, range: std::ops::RangeInclusive<f32>, suffix: &str) -> Option<f32> {
    let mut edited = value;
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("{label:<10}"))
                .font(theme::mono(10.0))
                .color(theme::TEXT_DIM),
        );
        changed = ui
            .add_sized(
                Vec2::new(180.0, 16.0),
                egui::Slider::new(&mut edited, range).suffix(suffix).fixed_decimals(1),
            )
            .changed();
    });
    changed.then_some(edited)
}

/// EQ, compressor, gate and delay for one channel.
fn dsp_panel(ui: &mut Ui, index: usize, channel: &ChannelState, engine: &EngineHandle) {
    ui.horizontal(|ui| {
        ui.add_space(20.0);

        // ---- EQ ----------------------------------------------------------
        ui.vertical(|ui| {
            ui.set_width(300.0);
            ui.horizontal(|ui| {
                if theme::chip(ui, "EQ", channel.eq.enabled, theme::ACCENT, Vec2::new(44.0, 20.0)).clicked() {
                    let mut s = channel.eq;
                    s.enabled = !s.enabled;
                    engine.send(Command::SetEq { channel: index, settings: s });
                }
                ui.label(
                    RichText::new("100 Hz · 400 Hz · 2.5 kHz · 8 kHz")
                        .font(theme::mono(9.0))
                        .color(theme::TEXT_FAINT),
                );
            });
            ui.add_space(4.0);
            for (label, value, apply) in [
                ("BASS", channel.eq.bass_db, 0usize),
                ("LO-MID", channel.eq.low_mid_db, 1),
                ("HI-MID", channel.eq.high_mid_db, 2),
                ("PRESENCE", channel.eq.presence_db, 3),
            ] {
                if let Some(db) = control(ui, label, value, -15.0..=15.0, " dB") {
                    let mut s = channel.eq;
                    match apply {
                        0 => s.bass_db = db,
                        1 => s.low_mid_db = db,
                        2 => s.high_mid_db = db,
                        _ => s.presence_db = db,
                    }
                    s.enabled = true;
                    engine.send(Command::SetEq { channel: index, settings: s });
                }
            }
        });

        ui.add_space(16.0);
        theme::divider(ui, 130.0);
        ui.add_space(16.0);

        // ---- compressor --------------------------------------------------
        ui.vertical(|ui| {
            ui.set_width(300.0);
            ui.horizontal(|ui| {
                if theme::chip(ui, "COMP", channel.compressor.enabled, theme::ACCENT, Vec2::new(52.0, 20.0)).clicked() {
                    let mut s = channel.compressor;
                    s.enabled = !s.enabled;
                    engine.send(Command::SetCompressor { channel: index, settings: s });
                }
                // Gain reduction, so the effect of the settings is visible
                // rather than guessed at.
                ui.label(
                    RichText::new(format!("GR {:.1} dB", channel.gain_reduction_db))
                        .font(theme::mono(9.5))
                        .color(if channel.gain_reduction_db < -0.5 { theme::WARN } else { theme::TEXT_FAINT }),
                );
            });
            ui.add_space(4.0);
            if let Some(v) = control(ui, "THRESH", channel.compressor.threshold_db, -60.0..=0.0, " dB") {
                let mut s = channel.compressor;
                s.threshold_db = v;
                s.enabled = true;
                engine.send(Command::SetCompressor { channel: index, settings: s });
            }
            if let Some(v) = control(ui, "RATIO", channel.compressor.ratio, 1.0..=20.0, ":1") {
                let mut s = channel.compressor;
                s.ratio = v;
                s.enabled = true;
                engine.send(Command::SetCompressor { channel: index, settings: s });
            }
            if let Some(v) = control(ui, "ATTACK", channel.compressor.attack_ms, 0.1..=100.0, " ms") {
                let mut s = channel.compressor;
                s.attack_ms = v;
                engine.send(Command::SetCompressor { channel: index, settings: s });
            }
            if let Some(v) = control(ui, "RELEASE", channel.compressor.release_ms, 10.0..=1000.0, " ms") {
                let mut s = channel.compressor;
                s.release_ms = v;
                engine.send(Command::SetCompressor { channel: index, settings: s });
            }
            if let Some(v) = control(ui, "MAKEUP", channel.compressor.makeup_db, 0.0..=24.0, " dB") {
                let mut s = channel.compressor;
                s.makeup_db = v;
                engine.send(Command::SetCompressor { channel: index, settings: s });
            }
        });

        ui.add_space(16.0);
        theme::divider(ui, 130.0);
        ui.add_space(16.0);

        // ---- gate and delay ----------------------------------------------
        ui.vertical(|ui| {
            ui.set_width(300.0);
            ui.horizontal(|ui| {
                if theme::chip(ui, "GATE", channel.gate.enabled, theme::ACCENT, Vec2::new(52.0, 20.0)).clicked() {
                    let mut s = channel.gate;
                    s.enabled = !s.enabled;
                    engine.send(Command::SetGate { channel: index, settings: s });
                }
                ui.label(
                    RichText::new(if channel.gate_open { "OPEN" } else { "CLOSED" })
                        .font(theme::mono(9.5))
                        .color(if channel.gate_open { theme::PREVIEW } else { theme::TEXT_FAINT }),
                );
            });
            ui.add_space(4.0);
            if let Some(v) = control(ui, "THRESH", channel.gate.threshold_db, -80.0..=0.0, " dB") {
                let mut s = channel.gate;
                s.threshold_db = v;
                s.enabled = true;
                engine.send(Command::SetGate { channel: index, settings: s });
            }
            if let Some(v) = control(ui, "HOLD", channel.gate.hold_ms, 0.0..=1000.0, " ms") {
                let mut s = channel.gate;
                s.hold_ms = v;
                engine.send(Command::SetGate { channel: index, settings: s });
            }
            if let Some(v) = control(ui, "RELEASE", channel.gate.release_ms, 10.0..=2000.0, " ms") {
                let mut s = channel.gate;
                s.release_ms = v;
                engine.send(Command::SetGate { channel: index, settings: s });
            }

            ui.add_space(10.0);
            ui.label(RichText::new("DELAY").size(10.0).strong().color(theme::TEXT_DIM));
            // Lip-sync trim, for when a source arrives ahead of its picture.
            if let Some(v) = control(ui, "OFFSET", channel.delay_ms, 0.0..=500.0, " ms") {
                engine.send(Command::SetAudioDelay { channel: index, ms: v });
            }
        });
    });
}

/// The crosspoint routing matrix: every channel against every bus.
///
/// A grid rather than a per-channel list, because the question an operator
/// actually asks is "what is feeding the interpreter bus", and only a grid
/// answers that by being looked at.
fn routing_matrix(ui: &mut Ui, snapshot: &Snapshot, engine: &EngineHandle) {
    const NAME_WIDTH: f32 = 150.0;
    const CELL: f32 = 62.0;

    ui.horizontal(|ui| {
        ui.add_space(20.0);
        ui.label(
            RichText::new("VISUAL CROSSPOINT ROUTING MATRIX")
                .size(11.5)
                .strong()
                .color(theme::TEXT),
        );
        ui.add_space(8.0);
        ui.label(
            RichText::new(format!(
                "{} IN x {} BUS OUT",
                snapshot.audio.len(),
                rhevia_audio::BUS_COUNT
            ))
            .font(theme::mono(9.5))
            .color(theme::TEXT_FAINT),
        );
    });
    ui.add_space(8.0);

    egui::ScrollArea::vertical()
        .id_salt("routing-matrix")
        .max_height(230.0)
        .show(ui, |ui| {
            // Bus header, with each bus carrying its own level so a silent
            // feed is visible here rather than only on its own meter.
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                ui.allocate_exact_size(Vec2::new(NAME_WIDTH, 26.0), Sense::hover());
                for (bus, name) in rhevia_audio::BUS_NAMES.iter().enumerate() {
                    let (peak, clipped) = snapshot
                        .bus_levels
                        .get(bus)
                        .copied()
                        .unwrap_or((METER_MIN_DB, false));
                    let colour = if clipped {
                        theme::PROGRAM
                    } else if peak > METER_MIN_DB + 1.0 {
                        theme::PREVIEW
                    } else {
                        theme::TEXT_FAINT
                    };
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(0.0, 1.0);
                        ui.set_width(CELL);
                        ui.label(RichText::new(*name).size(10.0).strong().color(colour));
                        ui.label(
                            RichText::new(if peak > METER_MIN_DB + 1.0 {
                                format!("{peak:.0}")
                            } else {
                                "--".into()
                            })
                            .font(theme::mono(8.5))
                            .color(theme::TEXT_FAINT),
                        );
                    });
                }
            });
            ui.add_space(4.0);

            for (index, channel) in snapshot.audio.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.add_space(20.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(0.0, 0.0);
                        ui.set_width(NAME_WIDTH);
                        ui.label(
                            RichText::new(channel.name.chars().take(20).collect::<String>())
                                .size(10.5)
                                .color(if channel.has_source { theme::TEXT } else { theme::TEXT_DIM }),
                        );
                        ui.label(
                            RichText::new(if channel.has_source { "device" } else { "no device" })
                                .font(theme::mono(8.5))
                                .color(theme::TEXT_FAINT),
                        );
                    });

                    for bus in 0..rhevia_audio::BUS_COUNT {
                        let on = channel.buses.get(bus).copied().unwrap_or(false);
                        // Master is red because it is what goes to air; the
                        // auxiliaries are cyan because they are routing, not
                        // programme.
                        let colour = if bus == 0 { theme::PROGRAM } else { theme::ACCENT };
                        ui.allocate_ui(Vec2::new(CELL, 30.0), |ui| {
                            if theme::chip(ui, if on { "ON" } else { "" }, on, colour, Vec2::new(CELL - 6.0, 26.0))
                                .on_hover_text(format!(
                                    "{} to {}",
                                    channel.name,
                                    rhevia_audio::BUS_NAMES[bus]
                                ))
                                .clicked()
                            {
                                engine.send(Command::SetChannelBus { channel: index, bus, on: !on });
                            }
                        });
                    }
                });
                ui.add_space(2.0);
            }
        });
}

/// The full mixer, on its own tab.
pub fn full_view(
    ctx: &egui::Context,
    snapshot: &Snapshot,
    engine: &EngineHandle,
    show_devices: &mut bool,
    selected: &mut usize,
) {
    egui::CentralPanel::default()
        .frame(theme::panel(theme::SURFACE))
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().id_salt("audio-page").show(ui, |ui| {
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                ui.label(RichText::new("AUDIO MIXER").size(12.0).strong().color(theme::TEXT));
                ui.add_space(10.0);
                ui.label(
                    RichText::new("48 kHz · stereo · constant-power pan")
                        .font(theme::mono(10.0))
                        .color(theme::TEXT_FAINT),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(20.0);
                    if theme::button(ui, "+ ADD DEVICE", theme::ACCENT, Vec2::new(116.0, 24.0)).clicked() {
                        *show_devices = true;
                    }
                });
            });
            ui.add_space(14.0);

            if snapshot.audio.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(40.0);
                    ui.label(RichText::new("no audio channels").size(12.0).color(theme::TEXT_FAINT));
                });
                return;
            }

            ui.horizontal(|ui| {
                ui.add_space(20.0);
                master_strip(ui, &snapshot.master, engine, true);
                ui.add_space(10.0);
                theme::divider(ui, 240.0);
                ui.add_space(10.0);

                egui::ScrollArea::horizontal()
                    .id_salt("audio-full-strips")
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for (index, channel) in snapshot.audio.iter().enumerate() {
                                strip(ui, index, channel, engine, true, Some(selected));
                                ui.add_space(6.0);
                            }
                        });
                    });
            });

            ui.add_space(14.0);
            ui.separator();
            ui.add_space(10.0);
            routing_matrix(ui, snapshot, engine);

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(10.0);

            if *selected >= snapshot.audio.len() {
                *selected = 0;
            }
            if let Some(channel) = snapshot.audio.get(*selected) {
                ui.horizontal(|ui| {
                    ui.add_space(20.0);
                    ui.label(
                        RichText::new(format!("CHANNEL DSP — {}", channel.name))
                            .size(11.5)
                            .strong()
                            .color(theme::TEXT),
                    );
                    ui.label(
                        RichText::new("gate → EQ → compressor → gain → delay")
                            .font(theme::mono(9.5))
                            .color(theme::TEXT_FAINT),
                    );
                });
                ui.add_space(8.0);
                dsp_panel(ui, *selected, channel, engine);
            }

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                ui.label(
                    RichText::new(
                        "M mute · S solo · F follow Program.  Click a meter to clear a latched clip.",
                    )
                    .size(10.5)
                    .color(theme::TEXT_FAINT),
                );
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                ui.label(
                    RichText::new(
                        "⚠ Audio is mixed and metered but not yet encoded into the outgoing stream.",
                    )
                    .size(10.5)
                    .color(theme::WARN),
                );
            });
            ui.add_space(16.0);
            });
        });
}
