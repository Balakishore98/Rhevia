//! Visual language, from `stitch_live_stream_studio_alternative/obsidian_broadcast/DESIGN.md`.
//!
//! Broadcast convention governs the two colours that matter: **red is on air,
//! green is next**. Operators read those faster than any label, and an
//! interface that spends red on anything else costs someone a mistake during a
//! show. Cyan is the third signal — active, selected, armed — and never means
//! "live".
//!
//! Everything else recedes into layered near-black surfaces so the two
//! monitors are the brightest things on screen.

use eframe::egui::{
    self, Color32, FontId, Rect, Response, RichText, Rounding, Sense, Stroke, TextureId, Ui, Vec2,
};

/* -- surfaces, darkest to lightest ------------------------------------- */
pub const SURFACE_LOWEST: Color32 = Color32::from_rgb(0x0a, 0x0e, 0x17);
pub const SURFACE: Color32 = Color32::from_rgb(0x0f, 0x13, 0x1c);
pub const SURFACE_LOW: Color32 = Color32::from_rgb(0x18, 0x1c, 0x25);
pub const SURFACE_CONTAINER: Color32 = Color32::from_rgb(0x1c, 0x20, 0x29);
pub const SURFACE_HIGH: Color32 = Color32::from_rgb(0x26, 0x2a, 0x33);
pub const SURFACE_HIGHEST: Color32 = Color32::from_rgb(0x31, 0x35, 0x3e);

/* -- signal colours ------------------------------------------------------ */
/// On air. Nothing else may use this.
pub const PROGRAM: Color32 = Color32::from_rgb(0xff, 0x54, 0x51);
/// The softer on-air tint, for headers and fills behind text.
pub const PROGRAM_DIM: Color32 = Color32::from_rgb(0x93, 0x00, 0x13);
/// Armed next.
pub const PREVIEW: Color32 = Color32::from_rgb(0x4a, 0xe1, 0x76);
pub const PREVIEW_DIM: Color32 = Color32::from_rgb(0x00, 0x53, 0x21);
/// Active, selected, armed — never "live".
pub const ACCENT: Color32 = Color32::from_rgb(0x4c, 0xd7, 0xf6);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x00, 0x4e, 0x5c);
/// Warnings and approaching-limit states.
pub const WARN: Color32 = Color32::from_rgb(0xff, 0xb3, 0xad);

/* -- text ---------------------------------------------------------------- */
pub const TEXT: Color32 = Color32::from_rgb(0xdf, 0xe2, 0xef);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8a, 0x91, 0xa4);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x5b, 0x62, 0x74);
/// For text sitting on a bright fill.
pub const ON_BRIGHT: Color32 = Color32::from_rgb(0x10, 0x14, 0x1c);

pub fn apply(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();

    style.visuals.dark_mode = true;
    style.visuals.panel_fill = SURFACE;
    style.visuals.window_fill = SURFACE_CONTAINER;
    style.visuals.extreme_bg_color = SURFACE_LOWEST;
    style.visuals.override_text_color = Some(TEXT);
    style.visuals.window_rounding = Rounding::same(8.0_f32);
    style.visuals.window_stroke = Stroke::new(1.0_f32, SURFACE_HIGHEST);
    style.visuals.selection.bg_fill = ACCENT_DIM;

    let w = &mut style.visuals.widgets;
    w.noninteractive.bg_fill = SURFACE_CONTAINER;
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT_DIM);
    w.inactive.bg_fill = SURFACE_HIGH;
    w.inactive.rounding = Rounding::same(4.0_f32);
    w.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    w.hovered.bg_fill = SURFACE_HIGHEST;
    w.hovered.rounding = Rounding::same(4.0_f32);
    w.active.bg_fill = ACCENT_DIM;
    w.active.rounding = Rounding::same(4.0_f32);

    style.spacing.item_spacing = Vec2::new(6.0, 5.0);
    style.spacing.button_padding = Vec2::new(10.0, 5.0);

    ctx.set_style(style);
}

pub fn panel(fill: Color32) -> egui::Frame {
    egui::Frame::none().fill(fill)
}

/// Monospace, for anything an operator reads as a number.
///
/// Timecode, bitrate and dB values change constantly; a proportional font
/// makes them jitter sideways and become unreadable at a glance.
pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

/// A primary action button.
pub fn button(ui: &mut Ui, label: &str, colour: Color32, size: Vec2) -> Response {
    let font = FontId::proportional((size.y * 0.36).clamp(10.0, 16.0));
    let needed = text_width(ui, label, &font) + 20.0;
    let size = Vec2::new(size.x.max(needed), size.y);

    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter_at(rect);

    let fill = if response.is_pointer_button_down_on() {
        colour.gamma_multiply(0.7)
    } else if response.hovered() {
        colour.gamma_multiply(1.2)
    } else {
        colour
    };
    painter.rect_filled(rect, Rounding::same(4.0_f32), fill);

    let luminance = 0.299 * fill.r() as f32 + 0.587 * fill.g() as f32 + 0.114 * fill.b() as f32;
    let text_colour = if luminance > 130.0 { ON_BRIGHT } else { TEXT };

    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        font,
        text_colour,
    );

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// A small outlined button, for dense rows of secondary actions.
pub fn chip(ui: &mut Ui, label: &str, active: bool, colour: Color32, size: Vec2) -> Response {
    let font = FontId::proportional((size.y * 0.46).clamp(9.0, 12.0));
    // The requested size is a minimum, not a promise. A label that does not
    // fit grows its own box rather than being painted over its neighbour.
    let needed = text_width(ui, label, &font) + 14.0;
    let size = Vec2::new(size.x.max(needed), size.y);

    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    // Clipped as well as grown: belt and braces, so nothing can ever bleed
    // out of a chip however the text is measured.
    let painter = ui.painter_at(rect);

    let (fill, border, text_colour) = if active {
        (colour, colour, ON_BRIGHT)
    } else if response.hovered() {
        (SURFACE_HIGHEST, colour, TEXT)
    } else {
        (SURFACE_HIGH, SURFACE_HIGHEST, TEXT_DIM)
    };

    painter.rect_filled(rect, Rounding::same(3.0_f32), fill);
    painter.rect_stroke(rect, Rounding::same(3.0_f32), Stroke::new(1.0_f32, border));
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        font,
        text_colour,
    );

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// A labelled telemetry reading for the status strip.
pub fn readout(ui: &mut Ui, label: &str, value: &str, colour: Color32) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(0.0, 1.0);
        ui.add_space(9.0);
        ui.label(RichText::new(label).size(8.5).color(TEXT_FAINT));
        ui.label(RichText::new(value).font(mono(12.0)).color(colour));
    });
    ui.add_space(16.0);
}

/// Width of `text` in `font`, as it will actually be drawn.
///
/// Guessing this from the character count is what made buttons overlap: a
/// painter does not clip to the box it is handed, so a label wider than its
/// box is drawn straight over whatever sits beside it. Measuring costs
/// nothing and removes the whole class of bug.
pub fn text_width(ui: &Ui, text: &str, font: &FontId) -> f32 {
    ui.fonts(|f| f.layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE).size().x)
}

/// Shortens `text` with an ellipsis until it fits `max_width`.
///
/// For the places where the space is genuinely fixed — a tile header with a
/// badge at the other end — and growing the box is not an option.
pub fn elide(ui: &Ui, text: &str, font: &FontId, max_width: f32) -> String {
    if max_width <= 0.0 {
        return String::new();
    }
    if text_width(ui, text, font) <= max_width {
        return text.to_string();
    }

    let chars: Vec<char> = text.chars().collect();
    // Binary search rather than shrinking one character at a time: a long
    // window title would otherwise cost dozens of layout passes per frame.
    let (mut low, mut high) = (0usize, chars.len());
    while low < high {
        let mid = (low + high + 1) / 2;
        let candidate: String = chars[..mid].iter().collect::<String>() + "…";
        if text_width(ui, &candidate, font) <= max_width {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    if low == 0 {
        return String::new();
    }
    chars[..low].iter().collect::<String>() + "…"
}

/// The size a chip needs for `label` at `height`.
///
/// Callers used to guess this as a multiple of the character count, which is
/// wrong for anything but a monospaced font and produced exactly the overlaps
/// it was meant to avoid.
pub fn chip_size(ui: &Ui, label: &str, height: f32) -> Vec2 {
    let font = FontId::proportional((height * 0.46).clamp(9.0, 12.0));
    Vec2::new(text_width(ui, label, &font) + 18.0, height)
}

/// A status pill, e.g. LIVE ON-AIR.
pub fn pill(ui: &mut Ui, label: &str, fill: Color32, size: Vec2) -> Response {
    let font = FontId::proportional(11.5);
    let size = Vec2::new(size.x.max(text_width(ui, label, &font) + 18.0), size.y);

    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, Rounding::same(4.0_f32), fill);

    let luminance = 0.299 * fill.r() as f32 + 0.587 * fill.g() as f32 + 0.114 * fill.b() as f32;
    let text_colour = if luminance > 130.0 { ON_BRIGHT } else { TEXT };
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        font,
        text_colour,
    );
    response
}

/// A vertical rule.
pub fn divider(ui: &mut Ui, height: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, height), Sense::hover());
    ui.painter().rect_filled(rect, Rounding::ZERO, SURFACE_HIGHEST);
}

/// Preview or Program, with a metadata header and footer.
///
/// The strips carry the things an operator checks without looking away from
/// the picture: what the source is, what format it is in, and whether it is
/// live.
pub fn monitor(
    ui: &mut Ui,
    label: &str,
    source_name: &str,
    footer: &str,
    badge: Option<&str>,
    texture: Option<TextureId>,
    accent: Color32,
    size: Vec2,
) {
    ui.allocate_ui(size, |ui| {
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let painter = ui.painter();

        painter.rect_filled(rect, Rounding::same(5.0_f32), SURFACE_LOWEST);

        let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 24.0));
        painter.rect_filled(
            header,
            Rounding { nw: 5.0_f32, ne: 5.0_f32, sw: 0.0_f32, se: 0.0_f32 },
            SURFACE_CONTAINER,
        );
        // A dot rather than a filled bar: the picture stays the brightest
        // thing, and the colour still reads at a glance.
        painter.circle_filled(header.left_center() + Vec2::new(12.0, 0.0), 4.0, accent);
        // The badge is measured first, because the title has to fit in what
        // is left over. Drawing the title at its natural length ran it
        // straight through the badge on any source with a long name.
        let badge_font = FontId::proportional(9.0);
        let badge_width = badge
            .map(|b| text_width(ui, b, &badge_font) + 14.0)
            .unwrap_or(0.0);

        let title_font = FontId::proportional(11.5);
        let title_start = 24.0;
        let title_room = header.width() - title_start - badge_width - 16.0;
        painter.text(
            header.left_center() + Vec2::new(title_start, 0.0),
            egui::Align2::LEFT_CENTER,
            elide(ui, &format!("{label} — {source_name}"), &title_font, title_room),
            title_font,
            accent,
        );
        if let Some(badge) = badge {
            let width = badge_width;
            let chip_rect = Rect::from_min_size(
                egui::pos2(header.max.x - width - 8.0, header.min.y + 5.0),
                Vec2::new(width, 14.0),
            );
            painter.rect_filled(chip_rect, Rounding::same(2.0_f32), accent);
            painter.text(
                chip_rect.center(),
                egui::Align2::CENTER_CENTER,
                badge,
                badge_font,
                ON_BRIGHT,
            );
        }

        let footer_height = 20.0;
        let body = Rect::from_min_max(
            rect.min + Vec2::new(0.0, 24.0),
            rect.max - Vec2::new(0.0, footer_height),
        );
        painter.rect_filled(body, Rounding::ZERO, Color32::BLACK);

        if let Some(texture) = texture {
            painter.image(
                texture,
                fit_16x9(body),
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            painter.text(
                body.center(),
                egui::Align2::CENTER_CENTER,
                "NO SIGNAL",
                mono(12.0),
                TEXT_FAINT,
            );
        }

        let foot = Rect::from_min_max(egui::pos2(rect.min.x, body.max.y), rect.max);
        painter.rect_filled(
            foot,
            Rounding { nw: 0.0_f32, ne: 0.0_f32, sw: 5.0_f32, se: 5.0_f32 },
            SURFACE_CONTAINER,
        );
        let footer_font = mono(9.5);
        painter.text(
            foot.left_center() + Vec2::new(12.0, 0.0),
            egui::Align2::LEFT_CENTER,
            elide(ui, footer, &footer_font, foot.width() - 20.0),
            footer_font,
            TEXT_FAINT,
        );

        painter.rect_stroke(rect, Rounding::same(5.0_f32), Stroke::new(1.0_f32, accent));
    });
}

/// The picture area of an input tile, with its tally header.
pub fn input_picture(
    ui: &mut Ui,
    index: usize,
    name: &str,
    detail: &str,
    texture: Option<TextureId>,
    on_program: bool,
    on_preview: bool,
    size: Vec2,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter();

    // Program wins when a source is on both: what matters is that it is live.
    let (tally, badge) = if on_program {
        (PROGRAM, "PGM")
    } else if on_preview {
        (PREVIEW, "PVW")
    } else {
        (SURFACE_HIGHEST, "IDLE")
    };

    painter.rect_filled(rect, Rounding::same(4.0_f32), SURFACE_LOWEST);

    let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 19.0));
    let header_fill = if on_program {
        PROGRAM_DIM
    } else if on_preview {
        PREVIEW_DIM
    } else {
        SURFACE_CONTAINER
    };
    painter.rect_filled(
        header,
        Rounding { nw: 4.0_f32, ne: 4.0_f32, sw: 0.0_f32, se: 0.0_f32 },
        header_fill,
    );

    // The index, boxed, so "cut to three" maps to something visible.
    let number = Rect::from_min_size(header.min + Vec2::new(4.0, 3.5), Vec2::new(13.0, 12.0));
    painter.rect_filled(number, Rounding::same(2.0_f32), tally);
    painter.text(
        number.center(),
        egui::Align2::CENTER_CENTER,
        format!("{index}"),
        FontId::proportional(9.0),
        ON_BRIGHT,
    );
    // As in the monitor header: the badge is fixed at the right, so the name
    // gets whatever is left and is shortened to fit rather than running over.
    let badge_font = FontId::proportional(8.5);
    let badge_width = text_width(ui, badge, &badge_font);
    let name_font = FontId::proportional(10.5);
    let name_start = 22.0;
    let name_room = header.width() - name_start - badge_width - 12.0;

    painter.text(
        header.left_center() + Vec2::new(name_start, 0.0),
        egui::Align2::LEFT_CENTER,
        elide(ui, name, &name_font, name_room),
        name_font,
        if on_program || on_preview { TEXT } else { TEXT_DIM },
    );
    painter.text(
        header.right_center() - Vec2::new(6.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        badge,
        badge_font,
        if on_program || on_preview { TEXT } else { TEXT_FAINT },
    );

    let picture = Rect::from_min_max(
        rect.min + Vec2::new(0.0, 19.0),
        rect.max - Vec2::new(0.0, 14.0),
    );
    if let Some(texture) = texture {
        painter.image(
            texture,
            fit_16x9(picture),
            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        painter.text(
            picture.center(),
            egui::Align2::CENTER_CENTER,
            "—",
            mono(11.0),
            TEXT_FAINT,
        );
    }

    // Format line, as on a real multiview.
    let foot = Rect::from_min_max(egui::pos2(rect.min.x, picture.max.y), rect.max);
    painter.rect_filled(foot, Rounding::ZERO, SURFACE_LOWEST);
    let detail_font = mono(8.5);
    painter.text(
        foot.left_center() + Vec2::new(5.0, 0.0),
        egui::Align2::LEFT_CENTER,
        elide(ui, detail, &detail_font, foot.width() - 9.0),
        detail_font,
        TEXT_FAINT,
    );

    painter.rect_stroke(rect, Rounding::same(4.0_f32), Stroke::new(1.5_f32, tally));

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// The T-bar, travelling downward as the transition completes.
pub fn vertical_t_bar(ui: &mut Ui, progress: f32, size: Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();

    let track = Rect::from_center_size(rect.center(), Vec2::new(28.0, rect.height()));
    painter.rect_filled(track, Rounding::same(4.0_f32), SURFACE_LOWEST);
    painter.rect_stroke(track, Rounding::same(4.0_f32), Stroke::new(1.0_f32, SURFACE_HIGHEST));

    let p = progress.clamp(0.0, 1.0);
    if p > 0.0 {
        let filled = Rect::from_min_size(track.min, Vec2::new(track.width(), track.height() * p));
        painter.rect_filled(filled, Rounding::same(4.0_f32), ACCENT_DIM);
    }

    for step in 1..4 {
        let y = track.min.y + track.height() * (step as f32 / 4.0);
        painter.line_segment(
            [egui::pos2(track.min.x + 4.0, y), egui::pos2(track.max.x - 4.0, y)],
            Stroke::new(1.0_f32, SURFACE_HIGHEST),
        );
    }

    let handle = Rect::from_center_size(
        egui::pos2(track.center().x, track.min.y + track.height() * p),
        Vec2::new(track.width() + 8.0, 9.0),
    );
    painter.rect_filled(handle, Rounding::same(2.0_f32), if p > 0.0 { ACCENT } else { TEXT_DIM });
}

/// Largest 16:9 rectangle centred inside `area`.
fn fit_16x9(area: Rect) -> Rect {
    let target = 16.0 / 9.0;
    let actual = area.width() / area.height().max(1.0);
    let (w, h) = if actual > target {
        (area.height() * target, area.height())
    } else {
        (area.width(), area.width() / target)
    };
    Rect::from_center_size(area.center(), Vec2::new(w, h))
}
