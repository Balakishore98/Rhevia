//! The compositor: many inputs, one picture.
//!
//! A scene is an ordered list of layers, each pointing at an input and
//! declaring where on the canvas it goes. Rendering walks them back to front.
//!
//! This is the piece that makes Rhevia a switcher rather than a relay, and the
//! shape here is what the GPU version will keep: the scene is plain data, so
//! it serialises into a show file, travels over the command bus, and can be
//! diffed between Preview and Program.

use crate::frame::Frame;

/// A rectangle in output pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self { x, y, width, height }
    }

    /// Covers the whole canvas.
    pub fn full(width: usize, height: usize) -> Self {
        Self::new(0.0, 0.0, width as f32, height as f32)
    }

    /// Scales a source to fit inside `self` without distorting it, centred —
    /// the letterbox every broadcast mixer applies when aspect ratios differ.
    pub fn fit(&self, source_width: usize, source_height: usize) -> Rect {
        if source_width == 0 || source_height == 0 || self.width <= 0.0 || self.height <= 0.0 {
            return *self;
        }
        let source_aspect = source_width as f32 / source_height as f32;
        let dest_aspect = self.width / self.height;

        let (w, h) = if source_aspect > dest_aspect {
            (self.width, self.width / source_aspect)
        } else {
            (self.height * source_aspect, self.height)
        };
        Rect::new(
            self.x + (self.width - w) / 2.0,
            self.y + (self.height - h) / 2.0,
            w,
            h,
        )
    }
}

/// Per-input picture adjustment, as every switcher offers on its inputs.
///
/// Applied at composite time rather than to the stored frame, so turning a
/// control does not cost a re-decode and the original is never destroyed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColourAdjust {
    /// -1.0 to 1.0, 0.0 neutral.
    pub brightness: f32,
    /// 0.0 to 2.0, 1.0 neutral.
    pub contrast: f32,
    /// 0.0 greyscale, 1.0 neutral, 2.0 doubled.
    pub saturation: f32,
}

impl Default for ColourAdjust {
    fn default() -> Self {
        Self { brightness: 0.0, contrast: 1.0, saturation: 1.0 }
    }
}

impl ColourAdjust {
    /// True when the settings would leave every pixel untouched, so the
    /// per-pixel work can be skipped entirely.
    pub fn is_neutral(&self) -> bool {
        self.brightness.abs() < 0.001
            && (self.contrast - 1.0).abs() < 0.001
            && (self.saturation - 1.0).abs() < 0.001
    }

    /// Applies the adjustment to one pixel.
    pub fn apply(&self, rgba: [u8; 4]) -> [u8; 4] {
        if self.is_neutral() {
            return rgba;
        }
        let mut channels = [0.0f32; 3];
        for c in 0..3 {
            channels[c] = rgba[c] as f32 / 255.0;
        }

        // Contrast pivots around mid grey, so raising it darkens shadows and
        // lifts highlights rather than simply brightening everything.
        for value in &mut channels {
            *value = (*value - 0.5) * self.contrast + 0.5 + self.brightness;
        }

        if (self.saturation - 1.0).abs() >= 0.001 {
            // Rec. 709 luma, matching the colour space the encoder works in.
            let luma = 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2];
            for value in &mut channels {
                *value = luma + (*value - luma) * self.saturation;
            }
        }

        [
            (channels[0].clamp(0.0, 1.0) * 255.0) as u8,
            (channels[1].clamp(0.0, 1.0) * 255.0) as u8,
            (channels[2].clamp(0.0, 1.0) * 255.0) as u8,
            rgba[3],
        ]
    }
}

/// One element of a scene.
#[derive(Debug, Clone)]
pub struct Layer {
    /// Index into the inputs passed to `render`.
    pub input: usize,
    pub rect: Rect,
    /// 0.0 transparent, 1.0 opaque. Drives fades and dissolves.
    pub opacity: f32,
    pub visible: bool,
    /// Preserve the source aspect ratio inside `rect`.
    pub preserve_aspect: bool,
    pub colour: ColourAdjust,
}

impl Layer {
    pub fn new(input: usize, rect: Rect) -> Self {
        Self {
            input,
            rect,
            opacity: 1.0,
            visible: true,
            preserve_aspect: true,
            colour: ColourAdjust::default(),
        }
    }

    pub fn with_colour(mut self, colour: ColourAdjust) -> Self {
        self.colour = colour;
        self
    }

    pub fn with_opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity.clamp(0.0, 1.0);
        self
    }
}

/// An ordered stack of layers, drawn back to front.
#[derive(Debug, Clone, Default)]
pub struct Scene {
    pub layers: Vec<Layer>,
    /// Shows through wherever nothing is drawn.
    pub background: [u8; 3],
}

impl Scene {
    pub fn new() -> Self {
        Self::default()
    }

    /// A single input filling the canvas — the common case, and what a cut
    /// between two full-screen sources uses.
    pub fn single(input: usize, width: usize, height: usize) -> Self {
        Self {
            layers: vec![Layer::new(input, Rect::full(width, height))],
            background: [0, 0, 0],
        }
    }

    pub fn push(&mut self, layer: Layer) -> &mut Self {
        self.layers.push(layer);
        self
    }
}

/// Renders scenes onto a reusable canvas.
#[derive(Debug, Default)]
pub struct Compositor {
    canvas: Frame,
}

impl Compositor {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            canvas: Frame::filled(width, height, [0, 0, 0]),
        }
    }

    pub fn size(&self) -> (usize, usize) {
        (self.canvas.width, self.canvas.height)
    }

    /// Composites `scene` and returns the result.
    ///
    /// `inputs[i]` supplies layer `i`; a `None` entry means that input has no
    /// current frame, which is normal while a camera is connecting. Such layers
    /// are skipped rather than drawn black, so a still-connecting source does
    /// not punch a hole through what is underneath it.
    pub fn render(&mut self, scene: &Scene, inputs: &[Option<&Frame>]) -> &Frame {
        self.canvas.fill(scene.background);

        for layer in &scene.layers {
            if !layer.visible || layer.opacity <= 0.0 {
                continue;
            }
            let Some(Some(source)) = inputs.get(layer.input) else {
                continue;
            };
            if source.is_empty() {
                continue;
            }

            let rect = if layer.preserve_aspect {
                layer.rect.fit(source.width, source.height)
            } else {
                layer.rect
            };
            draw(&mut self.canvas, source, rect, layer.opacity, layer.colour);
        }

        &self.canvas
    }

    /// The last rendered picture.
    pub fn output(&self) -> &Frame {
        &self.canvas
    }

    pub fn resize(&mut self, width: usize, height: usize) {
        self.canvas.resize(width, height);
    }
}

/// Draws `source` into `dest` at `rect`, scaled, with `opacity` applied.
fn draw(dest: &mut Frame, source: &Frame, rect: Rect, opacity: f32, colour: ColourAdjust) {
    // Clip to the canvas up front rather than testing every pixel.
    let x0 = rect.x.floor().max(0.0) as usize;
    let y0 = rect.y.floor().max(0.0) as usize;
    let x1 = ((rect.x + rect.width).ceil().max(0.0) as usize).min(dest.width);
    let y1 = ((rect.y + rect.height).ceil().max(0.0) as usize).min(dest.height);
    if x0 >= x1 || y0 >= y1 || rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }

    for y in y0..y1 {
        // Sample at pixel centres, so a 1:1 copy lands exactly on source
        // pixels instead of drifting half a pixel and blurring.
        let v = (y as f32 + 0.5 - rect.y) / rect.height;
        for x in x0..x1 {
            let u = (x as f32 + 0.5 - rect.x) / rect.width;
            if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
                continue;
            }

            let src = colour.apply(source.sample(u, v));
            let alpha = (src[3] as f32 / 255.0) * opacity;
            if alpha <= 0.0 {
                continue;
            }

            let Some(dst) = dest.pixel(x, y) else { continue };
            let blended = [
                blend(src[0], dst[0], alpha),
                blend(src[1], dst[1], alpha),
                blend(src[2], dst[2], alpha),
                // Output alpha: whatever was there, plus what we added.
                ((dst[3] as f32 / 255.0 + alpha).min(1.0) * 255.0) as u8,
            ];
            dest.set_pixel(x, y, blended);
        }
    }
}

#[inline]
fn blend(src: u8, dst: u8, alpha: f32) -> u8 {
    (src as f32 * alpha + dst as f32 * (1.0 - alpha))
        .round()
        .clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: usize, h: usize, rgb: [u8; 3]) -> Frame {
        Frame::filled(w, h, rgb)
    }

    #[test]
    fn a_full_screen_layer_replaces_the_background() {
        let red = solid(4, 4, [255, 0, 0]);
        let mut c = Compositor::new(4, 4);
        let scene = Scene::single(0, 4, 4);
        let out = c.render(&scene, &[Some(&red)]);
        assert_eq!(out.pixel(2, 2), Some([255, 0, 0, 255]));
    }

    #[test]
    fn layers_draw_back_to_front() {
        let red = solid(8, 8, [255, 0, 0]);
        let blue = solid(8, 8, [0, 0, 255]);
        let mut c = Compositor::new(8, 8);

        let mut scene = Scene::single(0, 8, 8);
        // Blue box over the middle of the red background.
        scene.push(Layer::new(1, Rect::new(2.0, 2.0, 4.0, 4.0)));

        let out = c.render(&scene, &[Some(&red), Some(&blue)]);
        assert_eq!(out.pixel(0, 0), Some([255, 0, 0, 255]), "background stays red");
        assert_eq!(out.pixel(4, 4), Some([0, 0, 255, 255]), "inset is blue");
    }

    #[test]
    fn opacity_blends_toward_what_is_underneath() {
        // This is what a dissolve is made of, so it has to be right.
        let black = solid(4, 4, [0, 0, 0]);
        let white = solid(4, 4, [255, 255, 255]);
        let mut c = Compositor::new(4, 4);

        let mut scene = Scene::single(0, 4, 4);
        scene.push(Layer::new(1, Rect::full(4, 4)).with_opacity(0.5));

        let out = c.render(&scene, &[Some(&black), Some(&white)]);
        let px = out.pixel(2, 2).unwrap();
        assert!(
            (120..=135).contains(&px[0]),
            "half-opacity white over black should be mid grey, got {px:?}"
        );
    }

    #[test]
    fn a_fully_transparent_layer_is_skipped() {
        let red = solid(4, 4, [255, 0, 0]);
        let blue = solid(4, 4, [0, 0, 255]);
        let mut c = Compositor::new(4, 4);
        let mut scene = Scene::single(0, 4, 4);
        scene.push(Layer::new(1, Rect::full(4, 4)).with_opacity(0.0));

        let out = c.render(&scene, &[Some(&red), Some(&blue)]);
        assert_eq!(out.pixel(2, 2), Some([255, 0, 0, 255]));
    }

    #[test]
    fn an_input_with_no_frame_yet_does_not_punch_a_hole() {
        // A camera that is still connecting must not black out what is under it.
        let red = solid(4, 4, [255, 0, 0]);
        let mut c = Compositor::new(4, 4);
        let mut scene = Scene::single(0, 4, 4);
        scene.push(Layer::new(1, Rect::full(4, 4)));

        let out = c.render(&scene, &[Some(&red), None]);
        assert_eq!(out.pixel(2, 2), Some([255, 0, 0, 255]));
    }

    #[test]
    fn a_missing_input_index_is_ignored_rather_than_panicking() {
        let red = solid(4, 4, [255, 0, 0]);
        let mut c = Compositor::new(4, 4);
        let mut scene = Scene::single(0, 4, 4);
        scene.push(Layer::new(99, Rect::full(4, 4)));
        let out = c.render(&scene, &[Some(&red)]);
        assert_eq!(out.pixel(1, 1), Some([255, 0, 0, 255]));
    }

    #[test]
    fn a_neutral_adjustment_changes_nothing() {
        let neutral = ColourAdjust::default();
        assert!(neutral.is_neutral());
        for px in [[0u8, 0, 0, 255], [128, 64, 200, 255], [255, 255, 255, 0]] {
            assert_eq!(neutral.apply(px), px);
        }
    }

    #[test]
    fn brightness_lifts_and_clamps_rather_than_wrapping() {
        // Wrapping would turn a highlight black, which is the worst possible
        // failure on a picture that is already too bright.
        let bright = ColourAdjust { brightness: 0.5, ..Default::default() };
        assert_eq!(bright.apply([200, 200, 200, 255])[0], 255);
        let dark = ColourAdjust { brightness: -0.5, ..Default::default() };
        assert_eq!(dark.apply([40, 40, 40, 255])[0], 0);
    }

    #[test]
    fn contrast_pivots_around_mid_grey() {
        let punchy = ColourAdjust { contrast: 1.5, ..Default::default() };
        // Mid grey is the pivot, so it should barely move.
        let mid = punchy.apply([128, 128, 128, 255])[0];
        assert!((120..=136).contains(&mid), "mid grey moved to {mid}");
        // Either side of it should push away from the middle.
        assert!(punchy.apply([180, 180, 180, 255])[0] > 180);
        assert!(punchy.apply([70, 70, 70, 255])[0] < 70);
    }

    #[test]
    fn zero_saturation_produces_grey_at_the_right_luma() {
        let grey = ColourAdjust { saturation: 0.0, ..Default::default() };
        let out = grey.apply([255, 0, 0, 255]);
        assert_eq!(out[0], out[1], "channels should match once desaturated");
        assert_eq!(out[1], out[2]);
        // Rec. 709 puts pure red at about 21% luma.
        assert!((45..=65).contains(&out[0]), "expected Rec.709 luma, got {}", out[0]);
    }

    #[test]
    fn alpha_survives_every_adjustment() {
        let adjust = ColourAdjust { brightness: 0.3, contrast: 1.4, saturation: 0.2 };
        assert_eq!(adjust.apply([10, 20, 30, 77])[3], 77);
    }

    #[test]
    fn a_layer_adjustment_reaches_the_composited_picture() {
        let source = Frame::filled(8, 8, [200, 40, 40]);
        let mut c = Compositor::new(8, 8);
        let mut scene = Scene::new();
        scene.push(
            Layer::new(0, Rect::full(8, 8))
                .with_colour(ColourAdjust { saturation: 0.0, ..Default::default() }),
        );
        let out = c.render(&scene, &[Some(&source)]);
        let px = out.pixel(4, 4).unwrap();
        assert_eq!(px[0], px[1], "the adjustment should have desaturated it");
    }

    #[test]
    fn fit_letterboxes_instead_of_distorting() {
        // 16:9 into a 4:3 box: full width, bars top and bottom.
        let box_4x3 = Rect::new(0.0, 0.0, 400.0, 300.0);
        let fitted = box_4x3.fit(1920, 1080);
        assert_eq!(fitted.width, 400.0);
        assert!((fitted.height - 225.0).abs() < 0.01, "got {fitted:?}");
        assert!((fitted.y - 37.5).abs() < 0.01, "should be centred: {fitted:?}");

        // 4:3 into a 16:9 box: full height, pillars left and right.
        let box_16x9 = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let fitted = box_16x9.fit(640, 480);
        assert_eq!(fitted.height, 1080.0);
        assert!((fitted.width - 1440.0).abs() < 0.01, "got {fitted:?}");
    }

    #[test]
    fn layers_are_clipped_to_the_canvas() {
        let red = solid(4, 4, [255, 0, 0]);
        let mut c = Compositor::new(4, 4);
        let mut scene = Scene::new();
        // Mostly off-canvas, including negative coordinates.
        scene.push(Layer::new(0, Rect::new(-2.0, -2.0, 4.0, 4.0)));
        let out = c.render(&scene, &[Some(&red)]);
        assert_eq!(out.pixel(0, 0), Some([255, 0, 0, 255]));
        assert_eq!(out.pixel(3, 3), Some([0, 0, 0, 255]), "outside the layer");
    }

    #[test]
    fn scaling_up_preserves_the_picture() {
        let mut small = Frame::new(2, 2);
        small.set_pixel(0, 0, [255, 0, 0, 255]);
        small.set_pixel(1, 0, [255, 0, 0, 255]);
        small.set_pixel(0, 1, [0, 255, 0, 255]);
        small.set_pixel(1, 1, [0, 255, 0, 255]);

        let mut c = Compositor::new(64, 64);
        let mut scene = Scene::new();
        scene.push(Layer {
            preserve_aspect: false,
            ..Layer::new(0, Rect::full(64, 64))
        });
        let out = c.render(&scene, &[Some(&small)]);

        let top = out.pixel(32, 4).unwrap();
        let bottom = out.pixel(32, 60).unwrap();
        assert!(top[0] > 200 && top[1] < 60, "top should stay red: {top:?}");
        assert!(bottom[1] > 200 && bottom[0] < 60, "bottom should stay green: {bottom:?}");
    }
}
