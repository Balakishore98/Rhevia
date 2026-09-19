//! Visual language.
//!
//! Broadcast convention, not decoration: **red is on air, green is next**.
//! Operators read those two colours faster than any label, and an interface
//! that uses red for anything else costs someone a mistake during a show.
//!
//! Everything else recedes — dark neutral surfaces, dim labels, one amber
//! accent — so the two monitors are the brightest things on screen.

use eframe::egui::{
    self, Color32, FontId, Rect, Response, RichText, Rounding, Sense, Stroke, TextureId, Ui, Vec2,
};

pub const BG_DARK: Color32 = Color32::from_rgb(14, 15, 18);
pub const BG_PANEL: Color32 = Color32::from_rgb(20, 22, 26);
pub const BG_CHROME: Color32 = Color32::from_rgb(26, 28, 33);
pub const BG_RAISED: Color32 = Color32::from_rgb(38, 41, 48);

/// On air.
pub const PROGRAM: Color32 = Color32::from_rgb(226, 62, 62);
/// Armed next.
pub const PREVIEW: Color32 = Color32::from_rgb(64, 196, 118);
/// Safe-to-act green, for Go Live.
pub const GO: Color32 = Color32::from_rgb(46, 160, 92);
/// The one accent, used sparingly so it still means something.
pub const ACCENT: Color32 = Color32::from_rgb(232, 168, 56);

pub const TEXT: Color32 = Color32::from_rgb(232, 234, 238);
pub const TEXT_DIM: Color32 = Color32::from_rgb(138, 144, 156);

pub fn apply(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();

    style.visuals.dark_mode = true;
    style.visuals.panel_fill = BG_DARK;
    style.visuals.window_fill = BG_PANEL;
    style.visuals.extreme_bg_color = BG_DARK;
    style.visuals.override_text_color = Some(TEXT);
    style.visuals.window_rounding = Rounding::same(8.0_f32);
    style.visuals.window_stroke = Stroke::new(1.0_f32, BG_RAISED);

    let widgets = &mut style.visuals.widgets;
    widgets.noninteractive.bg_fill = BG_PANEL;
    widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT_DIM);
    widgets.inactive.bg_fill = BG_RAISED;
    widgets.inactive.rounding = Rounding::same(5.0_f32);
    widgets.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    widgets.hovered.bg_fill = Color32::from_rgb(52, 56, 64);
    widgets.hovered.rounding = Rounding::same(5.0_f32);
    widgets.active.bg_fill = ACCENT;
    widgets.active.rounding = Rounding::same(5.0_f32);

    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    style.spacing.button_padding = Vec2::new(12.0, 6.0);

    ctx.set_style(style);
}

pub fn panel(fill: Color32) -> egui::Frame {
    egui::Frame::none().fill(fill)
}

/// A chunky, unambiguous action button. Sized deliberately: CUT and AUTO get
/// hit under pressure and must not need aiming.
pub fn button(ui: &mut Ui, label: &str, colour: Color32, size: Vec2) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter();

    let fill = if response.is_pointer_button_down_on() {
        colour.gamma_multiply(0.75)
    } else if response.hovered() {
        colour.gamma_multiply(1.15)
    } else {
        colour
    };

    painter.rect_filled(rect, Rounding::same(6.0_f32), fill);

    // Dark text on bright fills, light text on dark ones — readable either way.
    let luminance = 0.299 * fill.r() as f32 + 0.587 * fill.g() as f32 + 0.114 * fill.b() as f32;
    let text_colour = if luminance > 140.0 {
        Color32::from_rgb(18, 18, 20)
    } else {
        TEXT
    };

    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        FontId::proportional((size.y * 0.32).clamp(12.0, 18.0)),
        text_colour,
    );

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// A small labelled number for the top bar.
pub fn metric(ui: &mut Ui, label: &str, value: &str) {
    ui.vertical(|ui| {
        ui.add_space(6.0);
        ui.label(RichText::new(value).size(15.0).strong().color(TEXT));
        ui.label(RichText::new(label).size(9.0).color(TEXT_DIM));
    });
    ui.add_space(18.0);
}

/// Preview or Program, with its picture and a coloured identity bar.
pub fn monitor(
    ui: &mut Ui,
    label: &str,
    source_name: &str,
    texture: Option<TextureId>,
    accent: Color32,
    size: Vec2,
) {
    ui.allocate_ui(size, |ui| {
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let painter = ui.painter();

        painter.rect_filled(rect, Rounding::same(8.0_f32), BG_PANEL);

        // Header strip carries the colour, so the identity is visible even
        // when the picture is dark.
        let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 30.0));
        painter.rect_filled(
            header,
            Rounding {
                nw: 8.0_f32,
                ne: 8.0_f32,
                sw: 0.0_f32,
                se: 0.0_f32,
            },
            accent,
        );
        painter.text(
            header.left_center() + Vec2::new(12.0, 0.0),
            egui::Align2::LEFT_CENTER,
            label,
            FontId::proportional(13.0),
            Color32::from_rgb(16, 16, 18),
        );
        painter.text(
            header.right_center() - Vec2::new(12.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            source_name,
            FontId::proportional(12.0),
            Color32::from_rgb(28, 28, 30),
        );

        // The picture area, letterboxed to 16:9 so the aspect never lies.
        let body = Rect::from_min_max(
            rect.min + Vec2::new(0.0, 30.0),
            rect.max,
        );
        painter.rect_filled(body, Rounding::ZERO, Color32::BLACK);

        if let Some(texture) = texture {
            let video = fit_16x9(body);
            painter.image(
                texture,
                video,
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            painter.text(
                body.center(),
                egui::Align2::CENTER_CENTER,
                "no signal",
                FontId::proportional(13.0),
                TEXT_DIM,
            );
        }

        painter.rect_stroke(rect, Rounding::same(8.0_f32), Stroke::new(1.5_f32, accent));
    });
}

/// A vertical rule, for grouping the toolbar.
pub fn divider(ui: &mut Ui, height: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, height), Sense::hover());
    ui.painter().rect_filled(rect, Rounding::ZERO, BG_RAISED);
}

/// An input's picture with its tally header. Returns the click response; the
/// per-input buttons are drawn by the caller beneath it.
pub fn input_picture(
    ui: &mut Ui,
    label: &str,
    texture: Option<TextureId>,
    on_program: bool,
    on_preview: bool,
) -> Response {
    let size = Vec2::new(178.0, 118.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter();

    // Program wins when a source is on both: what matters is that it is live.
    let tally = if on_program {
        PROGRAM
    } else if on_preview {
        PREVIEW
    } else {
        Color32::from_rgb(52, 55, 62)
    };

    painter.rect_filled(rect, Rounding::same(4.0_f32), Color32::BLACK);

    let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 20.0));
    painter.rect_filled(
        header,
        Rounding { nw: 4.0_f32, ne: 4.0_f32, sw: 0.0_f32, se: 0.0_f32 },
        tally,
    );
    let header_text = if on_program || on_preview {
        Color32::from_rgb(16, 16, 18)
    } else {
        TEXT
    };
    painter.text(
        header.left_center() + Vec2::new(7.0, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::proportional(11.5),
        header_text,
    );

    let picture = Rect::from_min_max(rect.min + Vec2::new(0.0, 20.0), rect.max);
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
            "no signal",
            FontId::proportional(11.0),
            TEXT_DIM,
        );
    }

    painter.rect_stroke(rect, Rounding::same(4.0_f32), Stroke::new(2.0_f32, tally));

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// The T-bar, drawn vertically as on a hardware panel: the fader travels down
/// as the transition completes.
pub fn vertical_t_bar(ui: &mut Ui, progress: f32, size: Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();

    let track = Rect::from_center_size(rect.center(), Vec2::new(34.0, rect.height()));
    painter.rect_filled(track, Rounding::same(5.0_f32), BG_DARK);
    painter.rect_stroke(track, Rounding::same(5.0_f32), Stroke::new(1.0_f32, BG_RAISED));

    let p = progress.clamp(0.0, 1.0);
    if p > 0.0 {
        let filled = Rect::from_min_size(track.min, Vec2::new(track.width(), track.height() * p));
        painter.rect_filled(filled, Rounding::same(5.0_f32), ACCENT);
    }

    // Detents, so partial travel is readable at a glance.
    for step in 1..4 {
        let y = track.min.y + track.height() * (step as f32 / 4.0);
        painter.line_segment(
            [egui::pos2(track.min.x + 5.0, y), egui::pos2(track.max.x - 5.0, y)],
            Stroke::new(1.0_f32, Color32::from_rgb(60, 63, 70)),
        );
    }

    let handle = Rect::from_center_size(
        egui::pos2(track.center().x, track.min.y + track.height() * p),
        Vec2::new(track.width() + 10.0, 10.0),
    );
    painter.rect_filled(handle, Rounding::same(3.0_f32), TEXT);
    painter.rect_stroke(handle, Rounding::same(3.0_f32), Stroke::new(1.0_f32, BG_CHROME));
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
