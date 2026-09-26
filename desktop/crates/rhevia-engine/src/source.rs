//! Sources that produce a picture without a codec: stills and titles.
//!
//! Both matter more than they look. A still is how a holding slide, a sponsor
//! board or a stinger graphic gets on air, and a title is the lower third that
//! every production needs and no switcher should make you leave to build.

use ab_glyph::{Font, PxScale, ScaleFont};

/// Re-exported so a caller can hold a font without depending on ab_glyph
/// directly.
pub use ab_glyph::FontVec;

use crate::frame::Frame;

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("could not read {0}: {1}")]
    Read(String, String),
    #[error("{0} is not an image this build can decode (PNG, JPEG, BMP and GIF are supported)")]
    Unsupported(String),
    #[error("no usable font found on this system")]
    NoFont,
}

/// Loads a still image as an RGBA frame.
///
/// Transparency is preserved, so a PNG with an alpha channel works as an
/// overlay graphic rather than arriving on a black card.
pub fn load_image(path: &str) -> Result<Frame, SourceError> {
    let bytes =
        std::fs::read(path).map_err(|e| SourceError::Read(path.to_string(), e.to_string()))?;
    let decoded = image::load_from_memory(&bytes)
        .map_err(|_| SourceError::Unsupported(path.to_string()))?;

    let rgba = decoded.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(Frame {
        width: width as usize,
        height: height as usize,
        data: rgba.into_raw(),
    })
}

/// How a lower third is drawn.
///
/// A full-width bar is one look and not a very good one: it covers a third
/// of the frame to carry two words, and nothing broadcast has looked like
/// that for twenty years. These are the arrangements that actually get used,
/// and they are drawn to the text rather than to the frame — a short name
/// gets a short panel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TitleDesign {
    /// A panel sized to the words, on the left, with an accent edge. The
    /// one to reach for when in doubt.
    #[default]
    Box,
    /// The name on a solid block with the role on a narrower, softer block
    /// beneath it. The arrangement most broadcast lower thirds use.
    Stack,
    /// A thick accent bar down the left with the text beside it on a panel
    /// dark enough to read over anything.
    Stripe,
    /// No panel. Text with a shadow behind it and a short accent rule
    /// underneath, for a shot too good to cover.
    Minimal,
    /// The full-width bar. Kept because it is unmissable, which is what a
    /// notice or a warning wants.
    Bar,
}

impl TitleDesign {
    pub const ALL: [TitleDesign; 5] = [
        TitleDesign::Box,
        TitleDesign::Stack,
        TitleDesign::Stripe,
        TitleDesign::Minimal,
        TitleDesign::Bar,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TitleDesign::Box => "BOX",
            TitleDesign::Stack => "STACK",
            TitleDesign::Stripe => "STRIPE",
            TitleDesign::Minimal => "MINIMAL",
            TitleDesign::Bar => "BAR",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            TitleDesign::Box => "a panel sized to the words, with an accent edge",
            TitleDesign::Stack => "name on a solid block, role on a softer one beneath",
            TitleDesign::Stripe => "a thick accent bar with the text beside it",
            TitleDesign::Minimal => "text and a shadow, no panel — for a shot worth seeing",
            TitleDesign::Bar => "the full width of the frame, for a notice",
        }
    }
}

/// How a title is drawn.
#[derive(Debug, Clone)]
pub struct TitleStyle {
    pub text: String,
    /// Smaller line beneath the headline. Empty to omit.
    pub subtitle: String,
    /// Height of the headline as a fraction of the frame height.
    pub size: f32,
    pub colour: [u8; 3],
    pub subtitle_colour: [u8; 3],
    /// The bar behind the text.
    pub background: [u8; 3],
    /// 0 transparent, 255 solid. A bar at about 220 keeps the shot readable
    /// behind the text without the words losing contrast.
    pub background_alpha: u8,
    /// Draw as a lower third rather than filling the frame.
    pub lower_third: bool,
    /// Accent stripe down the leading edge of the bar.
    pub accent: [u8; 3],
    /// How it is arranged.
    pub design: TitleDesign,
}

impl Default for TitleStyle {
    fn default() -> Self {
        Self {
            text: String::new(),
            subtitle: String::new(),
            size: 0.075,
            colour: [255, 255, 255],
            subtitle_colour: [200, 206, 218],
            background: [16, 20, 28],
            background_alpha: 220,
            lower_third: true,
            accent: [76, 215, 246],
            design: TitleDesign::default(),
        }
    }
}

/// Loads a font from the system.
///
/// Nothing is bundled: shipping a typeface means shipping its licence, and
/// every desktop already has usable faces installed.
pub fn system_font() -> Result<FontVec, SourceError> {
    const CANDIDATES: &[&str] = &[
        r"C:\Windows\Fonts\segoeui.ttf",
        r"C:\Windows\Fonts\arial.ttf",
        r"C:\Windows\Fonts\calibri.ttf",
        r"C:\Windows\Fonts\tahoma.ttf",
        "/System/Library/Fonts/Helvetica.ttc",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
    ];

    for path in CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(font) = FontVec::try_from_vec(bytes) {
                return Ok(font);
            }
        }
    }
    Err(SourceError::NoFont)
}

/// How wide `text` is at `size`, in pixels.
///
/// A lower third is drawn to its words, not to the frame: a two-word name
/// gets a short panel. That cannot be done without measuring first.
fn measure(font: &FontVec, text: &str, size: f32) -> f32 {
    let scaled = font.as_scaled(PxScale::from(size));
    let mut width = 0.0;
    let mut previous: Option<char> = None;
    for character in text.chars().filter(|c| *c != '\n') {
        let glyph = font.glyph_id(character);
        if let Some(prev) = previous {
            width += scaled.kern(font.glyph_id(prev), glyph);
        }
        width += scaled.h_advance(glyph);
        previous = Some(character);
    }
    width
}

/// Fills a rectangle with rounded corners.
///
/// Square corners are the single thing that makes a graphic look like a
/// programmer drew it. The radius is small and the edge is antialiased, so
/// it reads as a panel rather than as a box.
fn rounded(frame: &mut Frame, x: f32, y: f32, w: f32, h: f32, radius: f32, rgba: [u8; 4]) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let radius = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    let (x0, y0, x1, y1) = (x, y, x + w, y + h);

    let top = y0.floor().max(0.0) as usize;
    let bottom = (y1.ceil() as usize).min(frame.height);
    let left = x0.floor().max(0.0) as usize;
    let right = (x1.ceil() as usize).min(frame.width);

    for py in top..bottom {
        for px in left..right {
            let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
            // Signed distance to the rounded rectangle: negative inside,
            // positive outside, zero on the edge. Taking only the positive
            // part -- which is the obvious way to write it -- leaves every
            // interior pixel at distance zero, and therefore at half
            // coverage, so the whole panel came out at half the opacity it
            // was asked for.
            let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
            let qx = (fx - cx).abs() - (x1 - x0) / 2.0 + radius;
            let qy = (fy - cy).abs() - (y1 - y0) / 2.0 + radius;
            let outside = (qx.max(0.0).hypot(qy.max(0.0)))
                + qx.max(qy).min(0.0)
                - radius;
            // One pixel of softness along the edge.
            let coverage = (0.5 - outside).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }
            let a = rgba[3] as f32 / 255.0 * coverage;
            let under = frame.pixel(px, py).unwrap_or([0, 0, 0, 0]);
            frame.set_pixel(
                px,
                py,
                [
                    (rgba[0] as f32 * a + under[0] as f32 * (1.0 - a)) as u8,
                    (rgba[1] as f32 * a + under[1] as f32 * (1.0 - a)) as u8,
                    (rgba[2] as f32 * a + under[2] as f32 * (1.0 - a)) as u8,
                    ((a + under[3] as f32 / 255.0 * (1.0 - a)) * 255.0).min(255.0) as u8,
                ],
            );
        }
    }
}

/// A soft dark shadow behind text, so it reads over a bright shot.
///
/// Drawn as the text itself in black at a few offsets rather than as a blur,
/// which costs a fraction as much and is indistinguishable at this size.
fn text_shadow(
    frame: &mut Frame,
    font: &FontVec,
    text: &str,
    left: f32,
    baseline: f32,
    size: f32,
) {
    let spread = (size * 0.045).max(1.0);
    for (dx, dy) in [(spread, spread), (-spread, spread), (0.0, spread * 1.6)] {
        draw_text(frame, font, text, left + dx, baseline + dy, size, [0, 0, 0]);
    }
}

/// Renders a title onto a transparent frame, ready to composite as an overlay.
pub fn render_title(
    font: &FontVec,
    style: &TitleStyle,
    width: usize,
    height: usize,
) -> Frame {
    let mut frame = Frame::new(width, height);
    if width == 0 || height == 0 {
        return frame;
    }

    let headline_px = (height as f32 * style.size).max(8.0);
    let subtitle_px = headline_px * 0.55;
    let pad = headline_px * 0.55;
    let has_subtitle = !style.subtitle.trim().is_empty();

    let panel = [
        style.background[0],
        style.background[1],
        style.background[2],
        style.background_alpha,
    ];
    let accent = [style.accent[0], style.accent[1], style.accent[2], 255];
    let radius = headline_px * 0.14;

    // Where the whole thing sits. Clear of the bottom edge so it survives a
    // display that overscans, which a projector in a hall usually does.
    let block = headline_px + if has_subtitle { subtitle_px * 1.45 } else { 0.0 };
    let panel_height = block + pad * 2.0;
    let margin = width as f32 * 0.055;
    let top = if style.lower_third {
        (height as f32 * 0.72).min(height as f32 - panel_height - height as f32 * 0.07)
    } else {
        (height as f32 - panel_height) / 2.0
    }
    .max(0.0);

    let headline_width = measure(font, &style.text, headline_px);
    let subtitle_width = if has_subtitle {
        measure(font, &style.subtitle, subtitle_px)
    } else {
        0.0
    };

    match style.design {
        // ---- the full-width bar -----------------------------------------
        TitleDesign::Bar => {
            let stripe = (width as f32 * 0.006).max(3.0);
            rounded(&mut frame, 0.0, top, width as f32, panel_height, 0.0, panel);
            rounded(&mut frame, margin * 0.5, top, stripe, panel_height, 0.0, accent);

            let left = margin * 0.5 + stripe + pad;
            let baseline = top + pad + headline_px * 0.78;
            draw_text(&mut frame, font, &style.text, left, baseline, headline_px, style.colour);
            if has_subtitle {
                draw_text(
                    &mut frame,
                    font,
                    &style.subtitle,
                    left,
                    baseline + subtitle_px * 1.45,
                    subtitle_px,
                    style.subtitle_colour,
                );
            }
        }

        // ---- a panel sized to the words ---------------------------------
        TitleDesign::Box => {
            let stripe = (headline_px * 0.12).max(3.0);
            let text_width = headline_width.max(subtitle_width);
            let panel_width = (stripe + pad * 1.6 + text_width + pad * 1.4)
                .min(width as f32 - margin * 2.0);

            rounded(&mut frame, margin, top, panel_width, panel_height, radius, panel);
            // The accent sits inside the rounded edge rather than on it, so
            // the corner stays round.
            rounded(
                &mut frame,
                margin + pad * 0.45,
                top + pad * 0.5,
                stripe,
                panel_height - pad,
                stripe * 0.5,
                accent,
            );

            let left = margin + pad * 0.45 + stripe + pad;
            let baseline = top + pad + headline_px * 0.78;
            draw_text(&mut frame, font, &style.text, left, baseline, headline_px, style.colour);
            if has_subtitle {
                draw_text(
                    &mut frame,
                    font,
                    &style.subtitle,
                    left,
                    baseline + subtitle_px * 1.45,
                    subtitle_px,
                    style.subtitle_colour,
                );
            }
        }

        // ---- name over role, two blocks ---------------------------------
        TitleDesign::Stack => {
            let name_height = headline_px + pad * 1.2;
            let name_width =
                (headline_width + pad * 2.4).min(width as f32 - margin * 2.0);
            rounded(&mut frame, margin, top, name_width, name_height, radius, panel);

            let baseline = top + pad * 0.6 + headline_px * 0.78;
            draw_text(
                &mut frame,
                font,
                &style.text,
                margin + pad * 1.2,
                baseline,
                headline_px,
                style.colour,
            );

            if has_subtitle {
                // The role sits under the name on the accent, indented, and
                // shorter — which is what tells the eye which is which
                // before either has been read.
                let role_height = subtitle_px + pad * 0.9;
                let role_width =
                    (subtitle_width + pad * 1.8).min(width as f32 - margin * 2.0);
                let role_top = top + name_height + pad * 0.18;
                rounded(
                    &mut frame,
                    margin + pad * 0.5,
                    role_top,
                    role_width,
                    role_height,
                    radius * 0.8,
                    accent,
                );
                // Dark text on the accent, which is bright by definition.
                draw_text(
                    &mut frame,
                    font,
                    &style.subtitle,
                    margin + pad * 0.5 + pad * 0.9,
                    role_top + pad * 0.45 + subtitle_px * 0.78,
                    subtitle_px,
                    [10, 14, 20],
                );
            }
        }

        // ---- a thick accent bar with the text beside it -----------------
        TitleDesign::Stripe => {
            let stripe = (headline_px * 0.3).max(6.0);
            let text_width = headline_width.max(subtitle_width);
            let panel_width =
                (stripe + pad * 1.2 + text_width + pad * 1.2).min(width as f32 - margin * 2.0);

            rounded(&mut frame, margin, top, panel_width, panel_height, radius, panel);
            rounded(&mut frame, margin, top, stripe, panel_height, radius, accent);

            let left = margin + stripe + pad * 1.2;
            let baseline = top + pad + headline_px * 0.78;
            draw_text(&mut frame, font, &style.text, left, baseline, headline_px, style.colour);
            if has_subtitle {
                draw_text(
                    &mut frame,
                    font,
                    &style.subtitle,
                    left,
                    baseline + subtitle_px * 1.45,
                    subtitle_px,
                    style.subtitle_colour,
                );
            }
        }

        // ---- no panel at all --------------------------------------------
        TitleDesign::Minimal => {
            let left = margin;
            let baseline = top + pad + headline_px * 0.78;
            text_shadow(&mut frame, font, &style.text, left, baseline, headline_px);
            draw_text(&mut frame, font, &style.text, left, baseline, headline_px, style.colour);

            // A short rule under the name, the width of the accent rather
            // than of the words: it is a mark, not an underline.
            let rule_top = baseline + headline_px * 0.28;
            rounded(
                &mut frame,
                left,
                rule_top,
                headline_px * 1.6,
                (headline_px * 0.075).max(2.0),
                headline_px * 0.04,
                accent,
            );

            if has_subtitle {
                let subtitle_baseline = rule_top + subtitle_px * 1.5;
                text_shadow(
                    &mut frame,
                    font,
                    &style.subtitle,
                    left,
                    subtitle_baseline,
                    subtitle_px,
                );
                draw_text(
                    &mut frame,
                    font,
                    &style.subtitle,
                    left,
                    subtitle_baseline,
                    subtitle_px,
                    style.subtitle_colour,
                );
            }
        }
    }

    frame
}

/// Draws one line of text with its baseline at `baseline_y`.
fn draw_text(
    frame: &mut Frame,
    font: &FontVec,
    text: &str,
    left: f32,
    baseline_y: f32,
    size: f32,
    colour: [u8; 3],
) {
    let scaled = font.as_scaled(PxScale::from(size));
    let mut caret = left;
    let mut previous: Option<char> = None;

    for character in text.chars() {
        if character == '\n' {
            continue;
        }
        let glyph_id = font.glyph_id(character);

        // Kerning, so pairs like "AV" do not sit with a gap between them.
        if let Some(prev) = previous {
            caret += scaled.kern(font.glyph_id(prev), glyph_id);
        }
        previous = Some(character);

        let glyph = glyph_id.with_scale_and_position(size, ab_glyph::point(caret, baseline_y));
        if let Some(outline) = font.outline_glyph(glyph) {
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, coverage| {
                if coverage <= 0.0 {
                    return;
                }
                let x = bounds.min.x as i32 + gx as i32;
                let y = bounds.min.y as i32 + gy as i32;
                if x < 0 || y < 0 {
                    return;
                }
                let (x, y) = (x as usize, y as usize);

                // Blended rather than written, so text sits on the bar and on
                // whatever the bar is transparent over.
                let under = frame.pixel(x, y).unwrap_or([0, 0, 0, 0]);
                let a = coverage.clamp(0.0, 1.0);
                let blended = [
                    (colour[0] as f32 * a + under[0] as f32 * (1.0 - a)) as u8,
                    (colour[1] as f32 * a + under[1] as f32 * (1.0 - a)) as u8,
                    (colour[2] as f32 * a + under[2] as f32 * (1.0 - a)) as u8,
                    ((a + under[3] as f32 / 255.0 * (1.0 - a)) * 255.0).min(255.0) as u8,
                ];
                frame.set_pixel(x, y, blended);
            });
        }

        caret += scaled.h_advance(glyph_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_font() -> Option<FontVec> {
        system_font().ok()
    }

    #[test]
    fn a_missing_image_reports_the_path_rather_than_panicking() {
        let err = load_image("definitely-not-here.png").unwrap_err();
        assert!(
            err.to_string().contains("definitely-not-here.png"),
            "the error should name the file: {err}"
        );
    }

    #[test]
    fn a_file_that_is_not_an_image_is_rejected_cleanly() {
        let dir = std::env::temp_dir().join("rhevia-source-tests");
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("not-an-image.png");
        std::fs::write(&path, b"this is plainly not a PNG").unwrap();

        let err = load_image(path.to_str().unwrap()).unwrap_err();
        assert!(matches!(err, SourceError::Unsupported(_)), "got {err}");
    }

    #[test]
    fn a_png_round_trips_with_its_alpha_intact() {
        // Transparency is the whole reason a still is useful as an overlay.
        let dir = std::env::temp_dir().join("rhevia-source-tests");
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("alpha.png");

        let mut buffer = image::RgbaImage::new(4, 2);
        buffer.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        buffer.put_pixel(1, 0, image::Rgba([0, 255, 0, 128]));
        buffer.put_pixel(2, 0, image::Rgba([0, 0, 255, 0]));
        buffer.save(&path).unwrap();

        let frame = load_image(path.to_str().unwrap()).expect("should load");
        assert_eq!((frame.width, frame.height), (4, 2));
        assert_eq!(frame.pixel(0, 0), Some([255, 0, 0, 255]));
        assert_eq!(frame.pixel(1, 0), Some([0, 255, 0, 128]), "alpha must survive");
        assert_eq!(frame.pixel(2, 0), Some([0, 0, 255, 0]));
    }

    /// True if anything solid is drawn in this column.
    fn drawn_in_column(frame: &Frame, x: usize, from: usize, to: usize) -> bool {
        (from..to).filter_map(|y| frame.pixel(x, y)).any(|px| px[3] > 200)
    }

    #[test]
    fn every_design_stays_in_the_lower_third_and_leaves_the_top_clear() {
        // Whatever it is arranged as, a lower third belongs at the bottom.
        // A graphic that creeps up the frame covers the face it is naming.
        let Some(font) = test_font() else {
            eprintln!("SKIP: no system font");
            return;
        };
        for design in TitleDesign::ALL {
            let style = TitleStyle {
                text: "ALEX CARTER".into(),
                subtitle: "LEAD ANALYST".into(),
                design,
                ..Default::default()
            };
            let frame = render_title(&font, &style, 640, 360);

            for y in [10, 60, 120, 180] {
                let clear = (0..640)
                    .step_by(7)
                    .filter_map(|x| frame.pixel(x, y))
                    .all(|px| px[3] == 0);
                assert!(clear, "{} drew something at y={y}", design.label());
            }
            assert!(
                drawn_in_column(&frame, 90, 230, 360),
                "{} drew nothing in the lower third at all",
                design.label()
            );
        }
    }

    #[test]
    fn only_the_bar_takes_the_whole_width() {
        // The point of the other four is that they are drawn to their words
        // and leave the shot showing. A design that reaches the far edge of
        // a 640-wide frame with two short lines in it is drawing a bar by
        // another name.
        let Some(font) = test_font() else {
            eprintln!("SKIP: no system font");
            return;
        };
        for design in TitleDesign::ALL {
            let style = TitleStyle {
                text: "ALEX CARTER".into(),
                subtitle: "LEAD ANALYST".into(),
                design,
                ..Default::default()
            };
            let frame = render_title(&font, &style, 640, 360);
            let reaches_the_edge = drawn_in_column(&frame, 620, 230, 360);

            match design {
                TitleDesign::Bar => assert!(
                    reaches_the_edge,
                    "the bar is supposed to cross the frame"
                ),
                _ => assert!(
                    !reaches_the_edge,
                    "{} covered the whole width, which is what the bar is for",
                    design.label()
                ),
            }
        }
    }

    #[test]
    fn a_title_actually_rasterises_glyphs() {
        let Some(font) = test_font() else {
            eprintln!("SKIP: no system font");
            return;
        };
        let with_text = render_title(
            &font,
            &TitleStyle { text: "HELLO".into(), ..Default::default() },
            640,
            360,
        );
        let without = render_title(
            &font,
            &TitleStyle { text: String::new(), ..Default::default() },
            640,
            360,
        );
        assert_ne!(with_text.data, without.data, "text should change the picture");
    }

    #[test]
    fn a_full_frame_title_sits_in_the_middle_instead() {
        // Turning the lower third off means "put it in the middle", which is
        // what a notice or a holding slide wants.
        let Some(font) = test_font() else {
            eprintln!("SKIP: no system font");
            return;
        };
        for design in TitleDesign::ALL {
            let style = TitleStyle {
                text: "FULL SCREEN".into(),
                lower_third: false,
                design,
                ..Default::default()
            };
            let frame = render_title(&font, &style, 640, 360);

            // Drawn across the middle band, and not down at the bottom.
            assert!(
                drawn_in_column(&frame, 90, 140, 230),
                "{} drew nothing in the middle",
                design.label()
            );
            let at_the_bottom = (330..360)
                .filter_map(|y| frame.pixel(90, y))
                .any(|px| px[3] > 200);
            assert!(
                !at_the_bottom,
                "{} stayed at the bottom when it was asked for the middle",
                design.label()
            );
        }
    }

    #[test]
    fn a_title_on_a_zero_sized_frame_is_empty_rather_than_a_panic() {
        let Some(font) = test_font() else {
            eprintln!("SKIP: no system font");
            return;
        };
        let frame = render_title(&font, &TitleStyle::default(), 0, 0);
        assert!(frame.is_empty());
    }

    #[test]
    fn very_long_text_does_not_write_outside_the_frame() {
        // Overrunning text must clip, not corrupt memory.
        let Some(font) = test_font() else {
            eprintln!("SKIP: no system font");
            return;
        };
        let style = TitleStyle {
            text: "A".repeat(400),
            ..Default::default()
        };
        let frame = render_title(&font, &style, 320, 180);
        assert_eq!(frame.data.len(), 320 * 180 * 4);
    }
}
