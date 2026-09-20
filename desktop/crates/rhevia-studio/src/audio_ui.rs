//! The audio mixer panel.
//!
//! Meters are the part that has to be right. An operator reads level from
//! colour and position long before they read a number, so the zones here match
//! broadcast convention: green is fine, amber is approaching, red is trouble,
//! and a clip latches until acknowledged rather than flashing past unseen.

use eframe::egui::{self, Color32, Rect, RichText, Rounding, Sense, Stroke, Ui, Vec2};

use crate::engine::{ChannelState, Command, EngineHandle, MasterState, Snapshot};
use crate::theme;

/// Meter range. Below this a signal is inaudible anyway, and showing more just
/// compresses the region that matters.
const METER_MIN_DB: f32 = -60.0;
const METER_MAX_DB: f32 = 6.0;

/// Where a dB value sits on the meter, 0.0 at the bottom.
fn position(db: f32) -> f32 {
    ((db - METER_MIN_DB) / (METER_MAX_DB - METER_MIN_DB)).clamp(0.0, 1.0)
}

/// Colour for a level, by broadcast convention.
fn zone_colour(db: f32) -> Color32 {
    if db >= -3.0 {
        theme::PROGRAM
    } else if db >= -12.0 {
        theme::ACCENT
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

    painter.rect_filled(rect, Rounding::same(2.0_f32), Color32::from_rgb(10, 11, 14));

    // Graticule at the levels an operator actually aims for.
    for &mark in &[0.0_f32, -6.0, -12.0, -20.0, -40.0] {
        let y = rect.max.y - rect.height() * position(mark);
        painter.line_segment(
            [egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)],
            Stroke::new(1.0_f32, Color32::from_rgb(38, 41, 48)),
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

    let peak_y = rect.max.y - rect.height() * position(peak_db);
    if peak_db > METER_MIN_DB {
        painter.line_segment(
            [egui::pos2(rect.min.x + 1.0, peak_y), egui::pos2(rect.max.x - 1.0, peak_y)],
            Stroke::new(2.0_f32, zone_colour(peak_db)),
        );
    }

    // Clip light at the top, latched. Clicking clears it.
    let light = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 5.0));
    painter.rect_filled(
        light,
        Rounding::same(2.0_f32),
        if clipped { theme::PROGRAM } else { Color32::from_rgb(30, 32, 38) },
    );

    painter.rect_stroke(rect, Rounding::same(2.0_f32), Stroke::new(1.0_f32, theme::BG_RAISED));
    response
}

/// A vertical fader. Returns the new value when dragged.
pub fn fader(ui: &mut Ui, db: f32, size: Vec2) -> (egui::Response, Option<f32>) {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let painter = ui.painter();

    let track = Rect::from_center_size(rect.center(), Vec2::new(5.0, rect.height()));
    painter.rect_filled(track, Rounding::same(2.0_f32), Color32::from_rgb(12, 13, 16));

    // Unity mark, so 0 dB can be found without reading the number.
    let unity_y = rect.max.y - rect.height() * position(0.0);
    painter.line_segment(
        [egui::pos2(rect.min.x, unity_y), egui::pos2(rect.max.x, unity_y)],
        Stroke::new(1.0_f32, theme::TEXT_DIM),
    );

    let knob_y = rect.max.y - rect.height() * position(db);
    let knob = Rect::from_center_size(
        egui::pos2(rect.center().x, knob_y),
        Vec2::new(rect.width() - 4.0, 13.0),
    );
    let knob_colour = if response.dragged() {
        theme::ACCENT
    } else {
        Color32::from_rgb(186, 190, 198)
    };
    painter.rect_filled(knob, Rounding::same(3.0_f32), knob_colour);
    painter.line_segment(
        [egui::pos2(knob.min.x + 2.0, knob.center().y), egui::pos2(knob.max.x - 2.0, knob.center().y)],
        Stroke::new(1.0_f32, Color32::from_rgb(40, 42, 48)),
    );

    let changed = if response.dragged() || response.clicked() {
        response.interact_pointer_pos().map(|pos| {
            let fraction = ((rect.max.y - pos.y) / rect.height()).clamp(0.0, 1.0);
            let value = METER_MIN_DB + fraction * (METER_MAX_DB - METER_MIN_DB);
            // Snap near unity: it is the value operators return to constantly
            // and a fader that will not settle on it is maddening.
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
///
/// Small, because pan is set once and rarely touched, but present, because a
/// mixer that cannot place a source in the stereo field is not a mixer.
fn pan_control(ui: &mut Ui, pan: f32, width: f32) -> (egui::Response, Option<f32>) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 14.0), Sense::click_and_drag());
    let painter = ui.painter();

    painter.rect_filled(rect, Rounding::same(3.0_f32), Color32::from_rgb(12, 13, 16));

    // Centre detent, so straight-ahead is findable without looking.
    painter.line_segment(
        [
            egui::pos2(rect.center().x, rect.min.y + 2.0),
            egui::pos2(rect.center().x, rect.max.y - 2.0),
        ],
        Stroke::new(1.0_f32, theme::TEXT_DIM),
    );

    let x = rect.min.x + rect.width() * ((pan.clamp(-1.0, 1.0) + 1.0) / 2.0);
    let knob = Rect::from_center_size(egui::pos2(x, rect.center().y), Vec2::new(7.0, 11.0));
    painter.rect_filled(knob, Rounding::same(2.0_f32), Color32::from_rgb(186, 190, 198));
    painter.rect_stroke(rect, Rounding::same(3.0_f32), Stroke::new(1.0_f32, theme::BG_RAISED));

    let changed = if response.dragged() || response.clicked() {
        response.interact_pointer_pos().map(|pos| {
            let value = ((pos.x - rect.min.x) / rect.width() * 2.0 - 1.0).clamp(-1.0, 1.0);
            // Snap to centre, which is where most sources belong.
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

/// A small square toggle, for mute and solo.
fn toggle(ui: &mut Ui, label: &str, on: bool, on_colour: Color32, width: f32) -> egui::Response {
    let colour = if on { on_colour } else { theme::BG_RAISED };
    theme::button(ui, label, colour, Vec2::new(width, 20.0))
}

/// One channel strip.
fn strip(ui: &mut Ui, index: usize, channel: &ChannelState, engine: &EngineHandle) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(3.0, 4.0);
        ui.set_width(62.0);

        let name: String = channel.name.chars().take(9).collect();
        ui.label(
            RichText::new(name)
                .size(10.0)
                .color(if channel.has_source { theme::TEXT } else { theme::TEXT_DIM }),
        );

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);
            let (response, changed) = fader(ui, channel.gain_db, Vec2::new(26.0, 116.0));
            if let Some(db) = changed {
                engine.send(Command::SetChannelGain { channel: index, db });
            }
            response.on_hover_text("drag to set level · unity snaps at 0 dB");

            if meter(ui, channel.peak_db, channel.rms_db, channel.clipped, Vec2::new(14.0, 116.0))
                .on_hover_text(if channel.clipped { "clipped — click to clear" } else { "level" })
                .clicked()
            {
                engine.send(Command::ClearClip(index));
            }
        });

        ui.label(
            RichText::new(format!("{:+.0}", channel.gain_db))
                .size(9.5)
                .color(theme::TEXT_DIM),
        );

        let (pan_response, pan_changed) = pan_control(ui, channel.pan, 59.0);
        if let Some(pan) = pan_changed {
            engine.send(Command::SetPan { channel: index, pan });
        }
        pan_response.on_hover_text(match channel.pan {
            p if p < -0.05 => format!("pan {:.0}% left", -p * 100.0),
            p if p > 0.05 => format!("pan {:.0}% right", p * 100.0),
            _ => "centre".to_string(),
        });

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);
            if toggle(ui, "M", channel.muted, theme::PROGRAM, 28.0)
                .on_hover_text("mute")
                .clicked()
            {
                engine.send(Command::ToggleMute(index));
            }
            if toggle(ui, "S", channel.solo, theme::ACCENT, 28.0)
                .on_hover_text("solo")
                .clicked()
            {
                engine.send(Command::ToggleSolo(index));
            }
        });

        // Following Program is the difference between a camera microphone and
        // a presenter microphone, so it belongs on the strip, not in a dialog.
        if toggle(ui, "FOLLOW", channel.follow_program, theme::PREVIEW, 59.0)
            .on_hover_text("audio comes up only while this input is on Program")
            .clicked()
        {
            engine.send(Command::ToggleFollowProgram(index));
        }
    });
}

/// The master strip.
fn master_strip(ui: &mut Ui, master: &MasterState, engine: &EngineHandle) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(3.0, 4.0);
        ui.set_width(68.0);

        ui.label(RichText::new("MASTER").size(10.0).strong().color(theme::ACCENT));

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(3.0, 3.0);
            let (_, changed) = fader(ui, master.gain_db, Vec2::new(28.0, 116.0));
            if let Some(db) = changed {
                engine.send(Command::SetMasterGain(db));
            }
            if meter(ui, master.peak_db, master.rms_db, master.clipped, Vec2::new(18.0, 116.0))
                .clicked()
            {
                engine.send(Command::ClearClip(usize::MAX));
            }
        });

        ui.label(
            RichText::new(format!("{:+.0} dB", master.gain_db))
                .size(9.5)
                .color(theme::TEXT_DIM),
        );

        if toggle(ui, "MUTE", master.muted, theme::PROGRAM, 65.0).clicked() {
            engine.send(Command::ToggleMasterMute);
        }

        ui.label(
            RichText::new(format!("{:.0} dBFS", master.peak_db.max(-99.0)))
                .size(9.0)
                .color(if master.clipped { theme::PROGRAM } else { theme::TEXT_DIM }),
        );
    });
}

/// The whole panel.
pub fn panel(ctx: &egui::Context, snapshot: &Snapshot, engine: &EngineHandle, show_devices: &mut bool) {
    egui::SidePanel::right("audio")
        .exact_width(300.0)
        .resizable(false)
        .frame(theme::panel(theme::BG_PANEL))
        .show(ctx, |ui| {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(RichText::new("AUDIO MIXER").size(11.0).strong().color(theme::TEXT_DIM));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(10.0);
                    if theme::button(ui, "+ DEVICE", theme::BG_RAISED, Vec2::new(78.0, 20.0))
                        .on_hover_text("add a microphone or line input")
                        .clicked()
                    {
                        *show_devices = true;
                    }
                });
            });
            ui.add_space(8.0);

            if snapshot.audio.is_empty() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("no audio channels").size(11.0).color(theme::TEXT_DIM));
                });
                return;
            }

            ui.horizontal(|ui| {
                ui.add_space(8.0);
                master_strip(ui, &snapshot.master, engine);
                ui.add_space(6.0);
                theme::divider(ui, 190.0);
                ui.add_space(6.0);

                egui::ScrollArea::horizontal()
                    .id_salt("audio-strips")
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for (index, channel) in snapshot.audio.iter().enumerate() {
                                strip(ui, index, channel, engine);
                                ui.add_space(4.0);
                            }
                        });
                    });
            });

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                let live = snapshot.audio.iter().filter(|c| c.has_source).count();
                ui.label(
                    RichText::new(format!(
                        "{live} of {} channels have a device",
                        snapshot.audio.len()
                    ))
                    .size(10.0)
                    .color(theme::TEXT_DIM),
                );
            });
        });
}
