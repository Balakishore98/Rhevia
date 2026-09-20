//! Transition effects.
//!
//! Every effect is a function of one number: how far through it is, from 0.0
//! showing the outgoing shot to 1.0 showing the incoming one. Keeping them
//! stateless means the engine can drive them from a clock, a T-bar, or a
//! script without any of them knowing the difference — and it makes each one
//! testable at its endpoints, which is where transitions actually go wrong.

use crate::frame::Frame;

/// The effects on the transition bus.
///
/// vMix exposes four customisable buttons plus Cut and FTB; this is the set
/// those buttons choose from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transition {
    /// Instant. No intermediate state exists.
    Cut,
    /// Crossfade — the default, and the one that never looks wrong.
    #[default]
    Fade,
    /// The incoming shot grows from the centre over the outgoing one.
    Zoom,
    /// A hard edge sweeps left to right.
    Wipe,
    /// A hard edge sweeps top to bottom.
    WipeVertical,
    /// The incoming shot pushes the outgoing one off to the left.
    Slide,
    /// The incoming shot pushes the outgoing one off upward.
    SlideVertical,
    /// The incoming shot flies in from the top-right, growing.
    Fly,
    /// The outgoing shot rushes toward the viewer as the incoming one
    /// arrives from behind it.
    CrossZoom,
    /// Both shots slide and scale, reading as a cube face turning.
    CubeZoom,
}

impl Transition {
    pub fn label(self) -> &'static str {
        match self {
            Transition::Cut => "CUT",
            Transition::Fade => "FADE",
            Transition::Zoom => "ZOOM",
            Transition::Wipe => "WIPE",
            Transition::WipeVertical => "WIPE V",
            Transition::Slide => "SLIDE",
            Transition::SlideVertical => "SLIDE V",
            Transition::Fly => "FLY",
            Transition::CrossZoom => "X-ZOOM",
            Transition::CubeZoom => "CUBE",
        }
    }

    /// Everything the bus can offer, in the order it is presented.
    pub const ALL: [Transition; 10] = [
        Transition::Fade,
        Transition::Zoom,
        Transition::Wipe,
        Transition::WipeVertical,
        Transition::Slide,
        Transition::SlideVertical,
        Transition::Fly,
        Transition::CrossZoom,
        Transition::CubeZoom,
        Transition::Cut,
    ];

    /// True if the effect resolves instantly and needs no animation.
    pub fn is_instant(self) -> bool {
        matches!(self, Transition::Cut)
    }
}

/// Renders `from` transitioning to `to` at `progress`, into `out`.
///
/// `out` is resized to match and fully written, so no clearing is needed
/// beforehand.
pub fn render(kind: Transition, from: &Frame, to: &Frame, progress: f32, out: &mut Frame) {
    let p = progress.clamp(0.0, 1.0);

    // Endpoints are exact rather than computed. A transition that is 99.6%
    // complete but never quite lands leaves a seam on air.
    //
    // Progress is tested before instantness so the contract is uniform: at 0
    // nothing has happened yet, whatever the effect. Checking Cut first put
    // the incoming shot on air before the transition had even started.
    if p <= 0.0 {
        copy_into(from, out);
        return;
    }
    if kind.is_instant() || p >= 1.0 {
        copy_into(to, out);
        return;
    }

    let (width, height) = output_size(from, to);
    out.resize(width, height);

    match kind {
        Transition::Cut => unreachable!("handled above"),
        Transition::Fade => fade(from, to, p, out),
        Transition::Zoom => zoom(from, to, p, out),
        Transition::Wipe => wipe(from, to, p, out, false),
        Transition::WipeVertical => wipe(from, to, p, out, true),
        Transition::Slide => slide(from, to, p, out, false),
        Transition::SlideVertical => slide(from, to, p, out, true),
        Transition::Fly => fly(from, to, p, out),
        Transition::CrossZoom => cross_zoom(from, to, p, out),
        Transition::CubeZoom => cube_zoom(from, to, p, out),
    }
}

/// The canvas both shots are drawn onto.
fn output_size(from: &Frame, to: &Frame) -> (usize, usize) {
    if !to.is_empty() {
        (to.width, to.height)
    } else if !from.is_empty() {
        (from.width, from.height)
    } else {
        (0, 0)
    }
}

fn copy_into(source: &Frame, out: &mut Frame) {
    out.resize(source.width, source.height);
    out.data.copy_from_slice(&source.data);
}

/// Samples a frame at normalised coordinates, black outside 0..1.
#[inline]
fn sample_or_black(frame: &Frame, u: f32, v: f32) -> [u8; 4] {
    if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) || frame.is_empty() {
        [0, 0, 0, 255]
    } else {
        frame.sample(u, v)
    }
}

#[inline]
fn mix(a: [u8; 4], b: [u8; 4], t: f32) -> [u8; 4] {
    let mut out = [0u8; 4];
    for c in 0..4 {
        out[c] = (a[c] as f32 * (1.0 - t) + b[c] as f32 * t)
            .round()
            .clamp(0.0, 255.0) as u8;
    }
    out
}

/// Walks every output pixel, handing the closure normalised coordinates.
fn for_each_pixel<F>(out: &mut Frame, mut f: F)
where
    F: FnMut(f32, f32) -> [u8; 4],
{
    let (w, h) = (out.width, out.height);
    for y in 0..h {
        let v = (y as f32 + 0.5) / h as f32;
        for x in 0..w {
            let u = (x as f32 + 0.5) / w as f32;
            let rgba = f(u, v);
            let i = (y * w + x) * 4;
            out.data[i..i + 4].copy_from_slice(&rgba);
        }
    }
}

fn fade(from: &Frame, to: &Frame, p: f32, out: &mut Frame) {
    for_each_pixel(out, |u, v| {
        mix(sample_or_black(from, u, v), sample_or_black(to, u, v), p)
    });
}

fn wipe(from: &Frame, to: &Frame, p: f32, out: &mut Frame, vertical: bool) {
    // A short soft edge rather than a hard one: a single-pixel boundary
    // crawls and aliases badly once the stream is compressed.
    const SOFTNESS: f32 = 0.01;

    for_each_pixel(out, |u, v| {
        let axis = if vertical { v } else { u };
        let distance = p - axis;
        let blend = ((distance / SOFTNESS) + 0.5).clamp(0.0, 1.0);
        mix(sample_or_black(from, u, v), sample_or_black(to, u, v), blend)
    });
}

fn slide(from: &Frame, to: &Frame, p: f32, out: &mut Frame, vertical: bool) {
    for_each_pixel(out, |u, v| {
        if vertical {
            // Outgoing pushed up, incoming arriving from below.
            if v + p < 1.0 {
                sample_or_black(from, u, v + p)
            } else {
                sample_or_black(to, u, v + p - 1.0)
            }
        } else if u + p < 1.0 {
            sample_or_black(from, u + p, v)
        } else {
            sample_or_black(to, u + p - 1.0, v)
        }
    });
}

fn zoom(from: &Frame, to: &Frame, p: f32, out: &mut Frame) {
    // The incoming shot grows from nothing to full frame, fading in over the
    // first part so it does not pop into existence as a hard rectangle.
    let scale = p.max(0.001);
    let opacity = (p * 2.0).min(1.0);

    for_each_pixel(out, |u, v| {
        let background = sample_or_black(from, u, v);
        let su = (u - 0.5) / scale + 0.5;
        let sv = (v - 0.5) / scale + 0.5;
        if !(0.0..1.0).contains(&su) || !(0.0..1.0).contains(&sv) {
            return background;
        }
        mix(background, sample_or_black(to, su, sv), opacity)
    });
}

fn cross_zoom(from: &Frame, to: &Frame, p: f32, out: &mut Frame) {
    // Outgoing rushes past the camera while incoming comes up from behind.
    let out_scale = 1.0 + p * 0.8;
    let in_scale = 0.6 + p * 0.4;

    for_each_pixel(out, |u, v| {
        let ou = (u - 0.5) / out_scale + 0.5;
        let ov = (v - 0.5) / out_scale + 0.5;
        let iu = (u - 0.5) / in_scale + 0.5;
        let iv = (v - 0.5) / in_scale + 0.5;
        mix(
            sample_or_black(from, ou, ov),
            sample_or_black(to, iu, iv),
            p,
        )
    });
}

fn fly(from: &Frame, to: &Frame, p: f32, out: &mut Frame) {
    // Incoming flies in from the top-right, growing into place.
    let scale = 0.25 + 0.75 * p;
    let centre_x = 0.82 - 0.32 * p;
    let centre_y = 0.18 + 0.32 * p;

    for_each_pixel(out, |u, v| {
        let background = sample_or_black(from, u, v);
        let su = (u - centre_x) / scale + 0.5;
        let sv = (v - centre_y) / scale + 0.5;
        if !(0.0..1.0).contains(&su) || !(0.0..1.0).contains(&sv) {
            return background;
        }
        sample_or_black(to, su, sv)
    });
}

fn cube_zoom(from: &Frame, to: &Frame, p: f32, out: &mut Frame) {
    // Both faces slide while shrinking toward the middle of the move, which
    // reads as a cube turning without needing real perspective.
    let squeeze = 1.0 - 0.25 * (p * std::f32::consts::PI).sin();

    for_each_pixel(out, |u, v| {
        let sv = (v - 0.5) / squeeze + 0.5;
        if !(0.0..1.0).contains(&sv) {
            return [0, 0, 0, 255];
        }
        if u + p < 1.0 {
            sample_or_black(from, u + p, sv)
        } else {
            sample_or_black(to, u + p - 1.0, sv)
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn red() -> Frame {
        Frame::filled(64, 36, [220, 30, 30])
    }

    fn blue() -> Frame {
        Frame::filled(64, 36, [30, 30, 220])
    }

    /// Every effect must land exactly on its endpoints. A transition that
    /// nearly finishes leaves a seam on air.
    #[test]
    fn every_effect_is_exact_at_both_ends() {
        let (from, to) = (red(), blue());
        let mut out = Frame::new(0, 0);

        for kind in Transition::ALL {
            render(kind, &from, &to, 0.0, &mut out);
            assert_eq!(
                out.pixel(32, 18),
                Some([220, 30, 30, 255]),
                "{:?} at progress 0 must be the outgoing shot",
                kind
            );

            render(kind, &from, &to, 1.0, &mut out);
            assert_eq!(
                out.pixel(32, 18),
                Some([30, 30, 220, 255]),
                "{:?} at progress 1 must be the incoming shot",
                kind
            );
        }
    }

    #[test]
    fn progress_outside_the_range_is_clamped_rather_than_extrapolated() {
        let (from, to) = (red(), blue());
        let mut out = Frame::new(0, 0);

        render(Transition::Fade, &from, &to, -5.0, &mut out);
        assert_eq!(out.pixel(10, 10), Some([220, 30, 30, 255]));

        render(Transition::Fade, &from, &to, 99.0, &mut out);
        assert_eq!(out.pixel(10, 10), Some([30, 30, 220, 255]));
    }

    #[test]
    fn cut_is_instant_even_halfway_through() {
        let (from, to) = (red(), blue());
        let mut out = Frame::new(0, 0);
        render(Transition::Cut, &from, &to, 0.5, &mut out);
        assert_eq!(
            out.pixel(32, 18),
            Some([30, 30, 220, 255]),
            "a cut has no intermediate state"
        );
    }

    #[test]
    fn fade_is_a_genuine_blend_at_the_midpoint() {
        let (from, to) = (red(), blue());
        let mut out = Frame::new(0, 0);
        render(Transition::Fade, &from, &to, 0.5, &mut out);

        let px = out.pixel(32, 18).unwrap();
        assert!(
            (100..=150).contains(&px[0]) && (100..=150).contains(&px[2]),
            "halfway should be a mix of both, got {px:?}"
        );
    }

    #[test]
    fn a_horizontal_wipe_reveals_from_the_left() {
        let (from, to) = (red(), blue());
        let mut out = Frame::new(0, 0);
        render(Transition::Wipe, &from, &to, 0.5, &mut out);

        let left = out.pixel(8, 18).unwrap();
        let right = out.pixel(56, 18).unwrap();
        assert!(left[2] > 150, "the left side should already be the new shot: {left:?}");
        assert!(right[0] > 150, "the right side should still be the old shot: {right:?}");
    }

    #[test]
    fn a_vertical_wipe_reveals_from_the_top() {
        let (from, to) = (red(), blue());
        let mut out = Frame::new(0, 0);
        render(Transition::WipeVertical, &from, &to, 0.5, &mut out);

        let top = out.pixel(32, 5).unwrap();
        let bottom = out.pixel(32, 31).unwrap();
        assert!(top[2] > 150, "the top should be the new shot: {top:?}");
        assert!(bottom[0] > 150, "the bottom should be the old shot: {bottom:?}");
    }

    #[test]
    fn slide_moves_both_shots_rather_than_blending_them() {
        // A slide must stay hard-edged; if it blends it is just a fade with
        // extra steps.
        let (from, to) = (red(), blue());
        let mut out = Frame::new(0, 0);
        render(Transition::Slide, &from, &to, 0.5, &mut out);

        let left = out.pixel(8, 18).unwrap();
        let right = out.pixel(56, 18).unwrap();
        assert!(left[0] > 180 && left[2] < 80, "outgoing stays saturated: {left:?}");
        assert!(right[2] > 180 && right[0] < 80, "incoming stays saturated: {right:?}");
    }

    #[test]
    fn zoom_grows_the_incoming_shot_from_the_centre() {
        let (from, to) = (red(), blue());
        let mut out = Frame::new(0, 0);

        // Early on, the centre has the new shot while the edges still show the old.
        render(Transition::Zoom, &from, &to, 0.25, &mut out);
        let centre = out.pixel(32, 18).unwrap();
        let corner = out.pixel(1, 1).unwrap();
        assert!(centre[2] > 100, "the centre should be showing the new shot: {centre:?}");
        assert!(corner[0] > 150, "the corner should still be the old shot: {corner:?}");
    }

    #[test]
    fn effects_cope_with_an_empty_incoming_frame() {
        // An input that has not produced a picture yet must not panic the
        // transition, which runs on the render thread.
        let from = red();
        let empty = Frame::new(0, 0);
        let mut out = Frame::new(0, 0);

        for kind in Transition::ALL {
            render(kind, &from, &empty, 0.5, &mut out);
        }
    }

    #[test]
    fn effects_cope_with_differently_sized_sources() {
        let from = Frame::filled(32, 18, [220, 30, 30]);
        let to = Frame::filled(128, 72, [30, 30, 220]);
        let mut out = Frame::new(0, 0);

        render(Transition::Fade, &from, &to, 0.5, &mut out);
        assert_eq!(
            (out.width, out.height),
            (128, 72),
            "the output should take the incoming shot's size"
        );
    }

    #[test]
    fn labels_are_present_and_distinct() {
        let mut seen = std::collections::HashSet::new();
        for kind in Transition::ALL {
            assert!(!kind.label().is_empty());
            assert!(seen.insert(kind.label()), "duplicate label for {kind:?}");
        }
    }
}
