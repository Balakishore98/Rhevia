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

/// Height the compact row along the bottom of the switcher reserves for its
/// strips, and the panel height that follows from it.
///
/// The strips were being clipped: a horizontal layout centres its children,
/// so the master and the channels sat at different heights and the taller of
/// them ran off the bottom of the panel. Stated here so the panel and its
/// contents cannot disagree.
const COMPACT_STRIP_HEIGHT: f32 = 132.0;
const COMPACT_ROW_HEIGHT: f32 = COMPACT_STRIP_HEIGHT + 56.0;

/// Height the mixer row reserves on the full view.
///
/// Stated rather than inferred: a horizontal scroll area does not report the
/// height of what it contains, so without this the row measures short and the
/// routing matrix below is drawn over the channel strips.
const MIXER_ROW_HEIGHT: f32 = 296.0;

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
/// The range the curve is drawn over, which is also the range the bands
/// allow. Wider would compress the part of the picture that is actually used.
const EQ_RANGE_DB: f32 = 15.0;
const EQ_MIN_HZ: f32 = 20.0;
const EQ_MAX_HZ: f32 = 20_000.0;

/// Position of a frequency across the curve, on a log scale — which is how
/// hearing works, and the only way 100 Hz and 10 kHz both get usable room.
fn eq_x(freq: f32) -> f32 {
    (freq.log10() - EQ_MIN_HZ.log10()) / (EQ_MAX_HZ.log10() - EQ_MIN_HZ.log10())
}

fn eq_freq_at(fraction: f32) -> f32 {
    10f32.powf(EQ_MIN_HZ.log10() + fraction * (EQ_MAX_HZ.log10() - EQ_MIN_HZ.log10()))
}

/// The gain a pointer at `y` is asking for, given the curve's vertical centre
/// and usable half-height.
///
/// Pulled out of the widget so the mapping can be checked: an inverted or
/// mis-scaled axis here means dragging up cuts, which would be found only by
/// someone doing it live.
fn eq_gain_at(pointer_y: f32, centre_y: f32, half_height: f32) -> f32 {
    if half_height <= 0.0 {
        return 0.0;
    }
    (((centre_y - pointer_y) / half_height) * EQ_RANGE_DB).clamp(-EQ_RANGE_DB, EQ_RANGE_DB)
}

/// Draws the EQ response and lets the bands be dragged on it.
///
/// The curve is the transfer function of the filters themselves rather than a
/// sketch of the slider positions, so what is drawn is exactly what the audio
/// is having done to it. Dragging a handle is the fastest way to work: you aim
/// at the shape you want rather than translating it into four numbers.
fn eq_curve(
    ui: &mut Ui,
    settings: rhevia_audio::EqSettings,
    size: Vec2,
) -> (egui::Response, Option<rhevia_audio::EqSettings>) {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let painter = ui.painter_at(rect);

    painter.rect_filled(rect, Rounding::same(3.0), theme::SURFACE_LOWEST);
    painter.rect_stroke(rect, Rounding::same(3.0), Stroke::new(1.0_f32, theme::SURFACE_HIGH));

    let to_y = |db: f32| {
        let clamped = db.clamp(-EQ_RANGE_DB, EQ_RANGE_DB);
        rect.center().y - (clamped / EQ_RANGE_DB) * (rect.height() / 2.0 - 4.0)
    };

    // ---- grid -----------------------------------------------------------
    for db in [-12.0, -6.0, 6.0, 12.0] {
        let y = to_y(db);
        painter.line_segment(
            [egui::pos2(rect.left() + 1.0, y), egui::pos2(rect.right() - 1.0, y)],
            Stroke::new(1.0_f32, theme::SURFACE_HIGH.gamma_multiply(0.55)),
        );
    }
    // Unity is drawn brighter: it is the reference the whole curve is read
    // against.
    let zero_y = to_y(0.0);
    painter.line_segment(
        [egui::pos2(rect.left() + 1.0, zero_y), egui::pos2(rect.right() - 1.0, zero_y)],
        Stroke::new(1.0_f32, theme::TEXT_FAINT),
    );

    for (freq, label) in [(100.0, "100"), (1000.0, "1k"), (10_000.0, "10k")] {
        let x = rect.left() + eq_x(freq) * rect.width();
        painter.line_segment(
            [egui::pos2(x, rect.top() + 1.0), egui::pos2(x, rect.bottom() - 1.0)],
            Stroke::new(1.0_f32, theme::SURFACE_HIGH.gamma_multiply(0.55)),
        );
        painter.text(
            egui::pos2(x + 3.0, rect.bottom() - 2.0),
            egui::Align2::LEFT_BOTTOM,
            label,
            theme::mono(8.5),
            theme::TEXT_FAINT,
        );
    }

    // ---- the curve ------------------------------------------------------
    let colour = if settings.enabled { theme::ACCENT } else { theme::TEXT_FAINT };
    let steps = 96;
    let mut points = Vec::with_capacity(steps + 1);
    for step in 0..=steps {
        let fraction = step as f32 / steps as f32;
        let db = settings.response_db(eq_freq_at(fraction));
        points.push(egui::pos2(rect.left() + fraction * rect.width(), to_y(db)));
    }

    // A soft fill under the curve, so a boost and a cut read differently at a
    // glance, before the shape itself is read.
    for window in points.windows(2) {
        let (a, b) = (window[0], window[1]);
        painter.add(egui::Shape::convex_polygon(
            vec![
                a,
                b,
                egui::pos2(b.x, zero_y),
                egui::pos2(a.x, zero_y),
            ],
            colour.gamma_multiply(0.14),
            Stroke::NONE,
        ));
    }
    painter.add(egui::Shape::line(points, Stroke::new(1.6_f32, colour)));

    // ---- band handles ---------------------------------------------------
    let gains = settings.gains();
    let mut handles = Vec::with_capacity(4);
    for (band, (_, freq)) in rhevia_audio::EqSettings::BANDS.iter().enumerate() {
        let centre = egui::pos2(rect.left() + eq_x(*freq) * rect.width(), to_y(gains[band]));
        handles.push(centre);
        painter.circle_filled(centre, 4.5, theme::SURFACE_LOWEST);
        painter.circle_stroke(centre, 4.5, Stroke::new(1.6_f32, colour));
    }

    // ---- dragging -------------------------------------------------------
    // The band being dragged is remembered for the length of the drag, so a
    // steep move past another band frequency does not hand the drag over to
    // that band halfway through.
    let held_id = response.id.with("held");
    let mut changed = None;

    let nearest_to = |pos: egui::Pos2| -> usize {
        handles
            .iter()
            .enumerate()
            .min_by(|a, b| {
                a.1.distance(pos)
                    .partial_cmp(&b.1.distance(pos))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(index, _)| index)
            .unwrap_or(0)
    };

    if response.drag_started() {
        if let Some(pos) = response.interact_pointer_pos() {
            let band = nearest_to(pos);
            ui.data_mut(|d| d.insert_temp(held_id, band));
        }
    }

    if response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            let band: usize = ui.data(|d| d.get_temp(held_id)).unwrap_or(0);
            let db = eq_gain_at(pos.y, rect.center().y, rect.height() / 2.0 - 4.0);

            let mut next = settings;
            next.set_gain(band, db);
            // Touching the curve means you want to hear it.
            next.enabled = true;
            changed = Some(next);
        }
    }

    // A double click flattens the band under the pointer, which is the usual
    // way out of an edit that went wrong.
    if response.double_clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let mut next = settings;
            next.set_gain(nearest_to(pos), 0.0);
            changed = Some(next);
        }
    }

    (response, changed)
}

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

        // Given the same height as a channel's name, which is a chip rather
        // than a label. Without this the master sits six pixels higher than
        // the channels beside it and every fader is on a different line — the
        // one thing an operator scans straight across.
        ui.allocate_ui(Vec2::new(69.0, 18.0), |ui| {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("MASTER L/R").size(9.5).strong().color(theme::ACCENT));
            });
        });
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

        // Loudness, which is the figure a platform judges the programme by.
        // Peak says whether it will distort; this says whether it will arrive
        // at the same level as everything else on the service.
        if tall {
            ui.add_space(6.0);
            loudness_readout(ui, master);
        }
    });
}

/// What most streaming services normalise to, and how far a reading may sit
/// from it before it is worth calling out. Broadcast practice is tighter than
/// this; streaming is not.
const LOUDNESS_TARGET: f32 = -16.0;
const LOUDNESS_TOLERANCE: f32 = 1.5;

fn loudness_colour(lufs: f32) -> Color32 {
    if lufs <= -60.0 {
        theme::TEXT_FAINT
    } else if (lufs - LOUDNESS_TARGET).abs() <= LOUDNESS_TOLERANCE {
        theme::PREVIEW
    } else if lufs > LOUDNESS_TARGET {
        theme::PROGRAM
    } else {
        theme::WARN
    }
}

fn loudness_text(lufs: f32) -> String {
    // Below the gate there is nothing to report, and a figure like -70.0
    // reads as a measurement rather than as silence.
    if lufs <= -60.0 {
        "  --  ".to_string()
    } else {
        format!("{lufs:+.1}")
    }
}

/// Momentary, short-term and integrated loudness, stacked.
fn loudness_readout(ui: &mut Ui, master: &MasterState) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(2.0, 2.0);
        ui.label(RichText::new("LOUDNESS  LUFS").size(8.5).color(theme::TEXT_FAINT));
        for (label, value, hint) in [
            ("M", master.momentary_lufs, "momentary — the last 400 ms"),
            ("S", master.short_term_lufs, "short term — the last 3 seconds"),
            ("I", master.integrated_lufs, "integrated — the whole programme, gated"),
        ] {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(4.0, 0.0);
                ui.label(RichText::new(label).font(theme::mono(9.0)).color(theme::TEXT_FAINT));
                ui.label(
                    RichText::new(loudness_text(value))
                        .font(theme::mono(10.0))
                        .color(loudness_colour(value)),
                )
                .on_hover_text(hint);
            });
        }
        ui.label(
            RichText::new(format!("target {LOUDNESS_TARGET:.0}"))
                .size(8.0)
                .color(theme::TEXT_FAINT),
        )
        .on_hover_text("most streaming platforms normalise to about -16 LUFS");
    });
}

/// The compact row along the bottom of the switcher.
pub fn strip_row(
    ctx: &egui::Context,
    snapshot: &Snapshot,
    engine: &EngineHandle,
    show_devices: &mut bool,
    selected: &mut usize,
) {
    egui::TopBottomPanel::bottom("audio-row")
        .exact_height(COMPACT_ROW_HEIGHT)
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

            // Top aligned, as on the full view: centring puts the master
            // fader and the channel faders on different lines, which is
            // exactly what an operator scans across.
            ui.horizontal_top(|ui| {
                ui.set_min_height(COMPACT_STRIP_HEIGHT);
                ui.add_space(12.0);
                master_strip(ui, &snapshot.master, engine, false);
                ui.add_space(6.0);
                theme::divider(ui, COMPACT_STRIP_HEIGHT - 8.0);
                ui.add_space(6.0);

                egui::ScrollArea::horizontal()
                    .id_salt("audio-row-strips")
                    .show(ui, |ui| {
                        ui.set_height(COMPACT_STRIP_HEIGHT);
                        ui.horizontal_top(|ui| {
                            for (index, channel) in snapshot.audio.iter().enumerate() {
                                // Selectable here as well as on the audio tab.
                                // Without this, every route to the settings
                                // arrived at whichever channel happened to be
                                // selected — which made per-channel settings
                                // look like one shared set.
                                strip(ui, index, channel, engine, false, Some(selected));
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
                    RichText::new("drag the curve")
                        .font(theme::mono(9.0))
                        .color(theme::TEXT_FAINT),
                );
            });
            ui.add_space(5.0);

            let (curve, dragged) = eq_curve(ui, channel.eq, Vec2::new(300.0, 104.0));
            curve.on_hover_text("drag a band to shape it, double click to flatten it");
            if let Some(settings) = dragged {
                engine.send(Command::SetEq { channel: index, settings });
            }

            ui.add_space(5.0);
            // The numbers stay: a curve is faster to shape with, but a show
            // that has to be matched to another desk needs exact figures.
            let gains = channel.eq.gains();
            for (band, (label, _)) in rhevia_audio::EqSettings::BANDS.iter().enumerate() {
                if let Some(db) = control(ui, label, gains[band], -15.0..=15.0, " dB") {
                    let mut s = channel.eq;
                    s.set_gain(band, db);
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
/// The plugin chain on one channel, and the list to add from.
///
/// Only plugins that survived validation can be added. A plugin that faults
/// takes down whatever hosts it, and that must never be the process carrying
/// the show — so the list says plainly which ones cannot be used, rather than
/// hiding them and leaving an operator hunting for a plugin they installed.
fn plugin_panel(
    ui: &mut Ui,
    index: usize,
    channel: &ChannelState,
    engine: &EngineHandle,
    available: &[rhevia_plugin::PluginInfo],
    scanning: bool,
    rescan: &mut bool,
) {
    ui.horizontal(|ui| {
        ui.add_space(20.0);
        ui.label(RichText::new("PLUGINS").size(11.5).strong().color(theme::TEXT));
        ui.label(
            RichText::new("after the built-in chain, in order")
                .font(theme::mono(9.5))
                .color(theme::TEXT_FAINT),
        );
        ui.add_space(10.0);
        if theme::button(ui, "RESCAN", theme::SURFACE_HIGHEST, Vec2::new(78.0, 20.0)).clicked() {
            *rescan = true;
        }
    });
    ui.add_space(6.0);

    ui.horizontal_top(|ui| {
        ui.add_space(20.0);

        // ---- what is on this channel -----------------------------------
        ui.vertical(|ui| {
            ui.set_width(300.0);
            ui.label(RichText::new("ON THIS CHANNEL").size(9.5).color(theme::TEXT_FAINT));
            ui.add_space(4.0);

            if channel.plugins.is_empty() {
                ui.label(RichText::new("none").size(10.5).color(theme::TEXT_FAINT));
            }
            for (slot, name) in channel.plugins.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{}.", slot + 1))
                            .font(theme::mono(9.5))
                            .color(theme::TEXT_FAINT),
                    );
                    ui.label(RichText::new(name).size(10.5).color(theme::TEXT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::chip(ui, "REMOVE", false, theme::PROGRAM, Vec2::new(62.0, 18.0))
                            .clicked()
                        {
                            engine.send(Command::RemovePlugin { channel: index, index: slot });
                        }
                    });
                });
            }
        });

        ui.add_space(16.0);
        theme::divider(ui, 150.0);
        ui.add_space(16.0);

        // ---- what can be added -----------------------------------------
        ui.vertical(|ui| {
            ui.set_width(360.0);
            ui.label(RichText::new("INSTALLED").size(9.5).color(theme::TEXT_FAINT));
            ui.add_space(4.0);

            if scanning {
                ui.label(
                    RichText::new("checking each plugin in its own process…")
                        .size(10.5)
                        .color(theme::TEXT_DIM),
                );
                return;
            }
            if available.is_empty() {
                ui.label(
                    RichText::new("no VST3 effects found — press Rescan after installing some")
                        .size(10.5)
                        .color(theme::TEXT_FAINT),
                );
                return;
            }

            egui::ScrollArea::vertical().max_height(140.0).id_salt("plugin-list").show(ui, |ui| {
                for plugin in available {
                    match plugin.usable {
                        Some(true) => {
                            let label = if plugin.vendor.is_empty() {
                                plugin.name.clone()
                            } else {
                                format!("{}   ·   {}", plugin.name, plugin.vendor)
                            };
                            if ui.button(label).on_hover_text(&plugin.path).clicked() {
                                engine.send(Command::AddPlugin {
                                    channel: index,
                                    path: plugin.path.clone(),
                                    cid: plugin.cid.clone(),
                                    name: plugin.name.clone(),
                                });
                            }
                        }
                        _ => {
                            // Listed but not offered, with the reason. An
                            // operator who installed it deserves to know why
                            // it is not there rather than to think Rhevia
                            // missed it.
                            ui.label(
                                RichText::new(format!("{}  — crashes when loaded", plugin.name))
                                    .size(10.0)
                                    .color(theme::TEXT_FAINT),
                            )
                            .on_hover_text(
                                "This plugin failed when opened in a separate process, so it \
                                 is not offered — it would take the show down with it.",
                            );
                        }
                    }
                }
            });
        });
    });
}

pub fn full_view(
    ctx: &egui::Context,
    snapshot: &Snapshot,
    engine: &EngineHandle,
    show_devices: &mut bool,
    selected: &mut usize,
    plugins: &[rhevia_plugin::PluginInfo],
    scanning: bool,
    rescan: &mut bool,
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

            // Top aligned, not centred: the master strip and the channel
            // strips are different heights, and centring them puts the faders
            // on different lines, which is exactly what an operator scans
            // across.
            ui.horizontal_top(|ui| {
                ui.set_min_height(MIXER_ROW_HEIGHT);
                ui.add_space(20.0);
                master_strip(ui, &snapshot.master, engine, true);
                ui.add_space(10.0);
                theme::divider(ui, MIXER_ROW_HEIGHT - 16.0);
                ui.add_space(10.0);

                egui::ScrollArea::horizontal()
                    .id_salt("audio-full-strips")
                    .show(ui, |ui| {
                        ui.set_height(MIXER_ROW_HEIGHT);
                        ui.horizontal_top(|ui| {
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
                            .color(theme::ACCENT),
                    );
                    ui.label(
                        RichText::new("gate → EQ → compressor → gain → delay")
                            .font(theme::mono(9.5))
                            .color(theme::TEXT_FAINT),
                    );
                });
                ui.add_space(5.0);

                // Every channel has its own settings, and which one is being
                // edited has to be unmistakable. Picking from a row here means
                // an operator never has to work out which strip was selected.
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(20.0);
                    ui.label(
                        RichText::new("EDITING").size(9.5).color(theme::TEXT_FAINT),
                    );
                    for (index, other) in snapshot.audio.iter().enumerate() {
                        let name: String = other.name.chars().take(12).collect();
                        let label = format!("{} {name}", index + 1);
                        if theme::chip(
                            ui,
                            &label,
                            index == *selected,
                            theme::ACCENT,
                            theme::chip_size(ui, &label, 20.0),
                        )
                        .on_hover_text("each channel keeps its own EQ, compressor, gate and delay")
                        .clicked()
                        {
                            *selected = index;
                        }
                    }
                });
                ui.add_space(8.0);
                dsp_panel(ui, *selected, channel, engine);

                ui.add_space(16.0);
                ui.separator();
                ui.add_space(12.0);
                plugin_panel(ui, *selected, channel, engine, plugins, scanning, rescan);
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


#[cfg(test)]
mod tests {
    use super::*;

    /// The frequency axis. A curve that puts 1 kHz in the wrong place is
    /// worse than no curve, because it is read rather than measured.
    mod frequency_axis {
        use super::*;

        #[test]
        fn the_ends_of_the_axis_are_the_ends_of_the_range() {
            assert!((eq_x(EQ_MIN_HZ) - 0.0).abs() < 1e-5);
            assert!((eq_x(EQ_MAX_HZ) - 1.0).abs() < 1e-5);
        }

        #[test]
        fn the_axis_is_logarithmic_not_linear() {
            // On a log axis the midpoint of 20 Hz to 20 kHz is about 630 Hz.
            // On a linear one it would be 10 kHz, which would leave every
            // useful band crushed into the left edge.
            let middle = eq_freq_at(0.5);
            assert!(
                (middle - 632.0).abs() < 20.0,
                "expected about 632 Hz at the midpoint, got {middle:.0}"
            );
        }

        #[test]
        fn position_and_frequency_round_trip() {
            for freq in [20.0, 100.0, 440.0, 2500.0, 8000.0, 20_000.0] {
                let back = eq_freq_at(eq_x(freq));
                assert!(
                    (back / freq - 1.0).abs() < 0.001,
                    "{freq} Hz came back as {back}"
                );
            }
        }

        #[test]
        fn every_band_falls_inside_the_drawn_range() {
            // A band whose handle sits off the edge could never be dragged.
            for (label, freq) in rhevia_audio::EqSettings::BANDS {
                let x = eq_x(freq);
                assert!(x > 0.0 && x < 1.0, "{label} at {freq} Hz sits at {x}");
            }
        }
    }

    /// What a drag lands on.
    mod drag_mapping {
        use super::*;

        // A curve 104 px tall, as the panel draws it.
        const CENTRE: f32 = 100.0;
        const HALF: f32 = 48.0;

        #[test]
        fn the_centre_line_is_unity() {
            assert!(eq_gain_at(CENTRE, CENTRE, HALF).abs() < 1e-5);
        }

        #[test]
        fn dragging_up_boosts_and_dragging_down_cuts() {
            // Screen y grows downward, so this is the sign error that would
            // otherwise make the control work backwards.
            assert!(eq_gain_at(CENTRE - 24.0, CENTRE, HALF) > 0.0, "up should boost");
            assert!(eq_gain_at(CENTRE + 24.0, CENTRE, HALF) < 0.0, "down should cut");
        }

        #[test]
        fn the_top_and_bottom_are_the_ends_of_the_range() {
            assert!((eq_gain_at(CENTRE - HALF, CENTRE, HALF) - EQ_RANGE_DB).abs() < 1e-4);
            assert!((eq_gain_at(CENTRE + HALF, CENTRE, HALF) + EQ_RANGE_DB).abs() < 1e-4);
        }

        #[test]
        fn dragging_past_the_edge_is_clamped_not_extrapolated() {
            assert_eq!(eq_gain_at(-500.0, CENTRE, HALF), EQ_RANGE_DB);
            assert_eq!(eq_gain_at(5000.0, CENTRE, HALF), -EQ_RANGE_DB);
        }

        #[test]
        fn halfway_up_is_half_the_range() {
            let db = eq_gain_at(CENTRE - HALF / 2.0, CENTRE, HALF);
            assert!((db - EQ_RANGE_DB / 2.0).abs() < 1e-4, "got {db}");
        }

        #[test]
        fn a_collapsed_curve_does_not_divide_by_zero() {
            // The panel can be laid out with no height for a frame while the
            // window is being resized.
            assert_eq!(eq_gain_at(50.0, 100.0, 0.0), 0.0);
        }
    }

    /// The loudness readout.
    mod loudness {
        use super::*;

        #[test]
        fn a_reading_at_target_is_shown_as_good() {
            assert_eq!(loudness_colour(LOUDNESS_TARGET), theme::PREVIEW);
        }

        #[test]
        fn too_loud_and_too_quiet_are_told_apart() {
            // Over target risks being turned down by the platform; under it
            // means the show arrives quieter than everything around it. They
            // are different problems and must not look the same.
            assert_eq!(loudness_colour(LOUDNESS_TARGET + 6.0), theme::PROGRAM);
            assert_eq!(loudness_colour(LOUDNESS_TARGET - 6.0), theme::WARN);
            assert_ne!(
                loudness_colour(LOUDNESS_TARGET + 6.0),
                loudness_colour(LOUDNESS_TARGET - 6.0)
            );
        }

        #[test]
        fn silence_is_shown_as_nothing_rather_than_as_a_number() {
            assert_eq!(loudness_text(-70.0).trim(), "--");
            assert_eq!(loudness_colour(-70.0), theme::TEXT_FAINT);
        }

        #[test]
        fn a_real_reading_carries_its_sign() {
            assert_eq!(loudness_text(-16.0), "-16.0");
            assert_eq!(loudness_text(-23.4), "-23.4");
        }
    }
}
