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
    let padding = headline_px * 0.55;

    let has_subtitle = !style.subtitle.trim().is_empty();
    let text_block = headline_px + if has_subtitle { subtitle_px * 1.35 } else { 0.0 };
    let bar_height = text_block + padding * 2.0;

    let bar_top = if style.lower_third {
        // Sits on the lower third line, clear of the bottom edge so it
        // survives a display that overscans.
        (height as f32 * 0.70).min(height as f32 - bar_height - height as f32 * 0.06)
    } else {
        (height as f32 - bar_height) / 2.0
    };
    let bar_top = bar_top.max(0.0);
    let bar_bottom = (bar_top + bar_height).min(height as f32);

    // ---- the bar --------------------------------------------------------
    let accent_width = (width as f32 * 0.006).max(3.0);
    for y in bar_top as usize..bar_bottom as usize {
        for x in 0..width {
            let rgba = if (x as f32) < padding * 0.6 + accent_width && (x as f32) >= padding * 0.6 {
                [style.accent[0], style.accent[1], style.accent[2], 255]
            } else {
                [
                    style.background[0],
                    style.background[1],
                    style.background[2],
                    style.background_alpha,
                ]
            };
            frame.set_pixel(x, y, rgba);
        }
    }

    // ---- the text -------------------------------------------------------
    let text_left = padding * 0.6 + accent_width + padding * 0.7;
    let headline_baseline = bar_top + padding + headline_px * 0.78;
    draw_text(
        &mut frame,
        font,
        &style.text,
        text_left,
        headline_baseline,
        headline_px,
        style.colour,
    );

    if has_subtitle {
        let subtitle_baseline = headline_baseline + subtitle_px * 1.35;
        draw_text(
            &mut frame,
            font,
            &style.subtitle,
            text_left,
            subtitle_baseline,
            subtitle_px,
            style.subtitle_colour,
        );
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

    #[test]
    fn a_title_draws_a_bar_in_the_lower_third_and_leaves_the_top_clear() {
        let Some(font) = test_font() else {
            eprintln!("SKIP: no system font");
            return;
        };
        let style = TitleStyle {
            text: "ALEX CARTER".into(),
            subtitle: "LEAD ANALYST".into(),
            ..Default::default()
        };
        let frame = render_title(&font, &style, 640, 360);

        let top = frame.pixel(320, 30).unwrap();
        assert_eq!(top[3], 0, "the top of the frame must stay transparent");

        // Somewhere in the lower third there should be an opaque bar.
        let bar = (250..330)
            .filter_map(|y| frame.pixel(320, y))
            .any(|px| px[3] > 200);
        assert!(bar, "expected a lower-third bar");
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
    fn a_full_frame_title_centres_its_bar_instead() {
        let Some(font) = test_font() else {
            eprintln!("SKIP: no system font");
            return;
        };
        let style = TitleStyle {
            text: "FULL SCREEN".into(),
            lower_third: false,
            ..Default::default()
        };
        let frame = render_title(&font, &style, 640, 360);

        let centre = frame.pixel(320, 180).unwrap();
        assert!(centre[3] > 200, "the bar should cross the middle: {centre:?}");
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
