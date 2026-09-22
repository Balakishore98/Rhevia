//! Transition effects.
//!
//! The full bus, matching what vMix offers: eighteen effects plus four
//! stingers. Every one is a pure function of progress, so the engine can drive
//! them from a clock, a T-bar or a script without any of them knowing the
//! difference — and each is testable at its endpoints, which is where
//! transitions actually go wrong.

use crate::frame::Frame;

/// The effects on the transition bus, in the order vMix presents them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transition {
    /// Instant. No intermediate state exists.
    Cut,
    /// Crossfade — the default, and the one that never looks wrong.
    #[default]
    Fade,
    /// The incoming shot grows from the centre.
    Zoom,
    /// A hard edge sweeps left to right.
    Wipe,
    /// The incoming shot pushes the outgoing one off to the left.
    Slide,
    /// The incoming shot flies in from the top-right, growing.
    Fly,
    /// Outgoing rushes toward the viewer as incoming arrives from behind.
    CrossZoom,
    /// Fly, with the incoming shot also spinning into place.
    FlyRotate,
    /// Both shots slide as faces of a turning cube.
    Cube,
    /// Cube, with the faces also pulling back toward the middle of the move.
    CubeZoom,
    /// A hard edge sweeps top to bottom.
    VerticalWipe,
    /// The incoming shot pushes the outgoing one off upward.
    VerticalSlide,
    /// Scales and crossfades together, for shots that nearly match.
    Merge,
    /// Wipe, right to left.
    WipeReverse,
    /// Slide, entering from the left.
    SlideReverse,
    /// Vertical wipe, bottom to top.
    VerticalWipeReverse,
    /// Vertical slide, entering from the top.
    VerticalSlideReverse,
    /// Two halves part from the centre, revealing what is behind.
    BarnDoor,
    /// The incoming shot rolls down over the outgoing one like a shutter.
    RollerDoor,
    /// Dips through a designated source, cutting underneath at the midpoint.
    Stinger1,
    Stinger2,
    Stinger3,
    Stinger4,
}

impl Transition {
    pub fn label(self) -> &'static str {
        match self {
            Transition::Cut => "Cut",
            Transition::Fade => "Fade",
            Transition::Zoom => "Zoom",
            Transition::Wipe => "Wipe",
            Transition::Slide => "Slide",
            Transition::Fly => "Fly",
            Transition::CrossZoom => "CrossZoom",
            Transition::FlyRotate => "FlyRotate",
            Transition::Cube => "Cube",
            Transition::CubeZoom => "CubeZoom",
            Transition::VerticalWipe => "VerticalWipe",
            Transition::VerticalSlide => "VerticalSlide",
            Transition::Merge => "Merge",
            Transition::WipeReverse => "WipeReverse",
            Transition::SlideReverse => "SlideReverse",
            Transition::VerticalWipeReverse => "VerticalWipeReverse",
            Transition::VerticalSlideReverse => "VerticalSlideReverse",
            Transition::BarnDoor => "BarnDoor",
            Transition::RollerDoor => "RollerDoor",
            Transition::Stinger1 => "Stinger 1",
            Transition::Stinger2 => "Stinger 2",
            Transition::Stinger3 => "Stinger 3",
            Transition::Stinger4 => "Stinger 4",
        }
    }

    /// Everything on the bus, in presentation order.
    pub const ALL: [Transition; 23] = [
        Transition::Fade,
        Transition::Zoom,
        Transition::Wipe,
        Transition::Slide,
        Transition::Fly,
        Transition::CrossZoom,
        Transition::FlyRotate,
        Transition::Cube,
        Transition::CubeZoom,
        Transition::VerticalWipe,
        Transition::VerticalSlide,
        Transition::Merge,
        Transition::WipeReverse,
        Transition::SlideReverse,
        Transition::VerticalWipeReverse,
        Transition::VerticalSlideReverse,
        Transition::BarnDoor,
        Transition::RollerDoor,
        Transition::Stinger1,
        Transition::Stinger2,
        Transition::Stinger3,
        Transition::Stinger4,
        Transition::Cut,
    ];

    /// True if the effect resolves instantly and needs no animation.
    pub fn is_instant(self) -> bool {
        matches!(self, Transition::Cut)
    }

    /// Which stinger slot this effect uses, if any.
    pub fn stinger_slot(self) -> Option<usize> {
        match self {
            Transition::Stinger1 => Some(0),
            Transition::Stinger2 => Some(1),
            Transition::Stinger3 => Some(2),
            Transition::Stinger4 => Some(3),
            _ => None,
        }
    }
}

/// Renders `from` transitioning to `to` at `progress`, into `out`.
///
/// `via` supplies the covering picture for stinger effects and is ignored by
/// every other one. `out` is resized to match and fully written, so no
/// clearing is needed beforehand.
pub fn render(
    kind: Transition,
    from: &Frame,
    to: &Frame,
    via: Option<&Frame>,
    progress: f32,
    out: &mut Frame,
) {
    let p = progress.clamp(0.0, 1.0);

    // Endpoints are exact rather than computed. A transition that is 99.6%
    // complete but never quite lands leaves a seam on air.
    //
    // Progress is tested before instantness so the contract is uniform: at 0
    // nothing has happened yet, whatever the effect.
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
        Transition::Wipe => wipe(from, to, p, out, Axis::Horizontal, false),
        Transition::WipeReverse => wipe(from, to, p, out, Axis::Horizontal, true),
        Transition::VerticalWipe => wipe(from, to, p, out, Axis::Vertical, false),
        Transition::VerticalWipeReverse => wipe(from, to, p, out, Axis::Vertical, true),
        Transition::Slide => slide(from, to, p, out, Axis::Horizontal, false),
        Transition::SlideReverse => slide(from, to, p, out, Axis::Horizontal, true),
        Transition::VerticalSlide => slide(from, to, p, out, Axis::Vertical, false),
        Transition::VerticalSlideReverse => slide(from, to, p, out, Axis::Vertical, true),
        Transition::Fly => fly(from, to, p, out, 0.0),
        Transition::FlyRotate => fly(from, to, p, out, std::f32::consts::TAU),
        Transition::CrossZoom => cross_zoom(from, to, p, out),
        Transition::Cube => cube(from, to, p, out, false),
        Transition::CubeZoom => cube(from, to, p, out, true),
        Transition::Merge => merge(from, to, p, out),
        Transition::BarnDoor => barn_door(from, to, p, out),
        Transition::RollerDoor => roller_door(from, to, p, out),
        Transition::Stinger1
        | Transition::Stinger2
        | Transition::Stinger3
        | Transition::Stinger4 => stinger(from, to, via, p, out),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Axis {
    Horizontal,
    Vertical,
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

/// Reads a frame on the output's own grid.
///
/// Both sides of a transition are full renders at the production size, so an
/// effect that does not move the picture asks for the pixel it is already
/// standing on — and sampling spends four texel fetches and eight
/// interpolations arriving back there. Deciding once, outside the loop,
/// whether any of that is needed is what separates a two millisecond
/// transition from a hundred and forty-four millisecond one.
#[derive(Clone, Copy)]
struct OnGrid<'a> {
    frame: &'a Frame,
    /// True when the frame lines up with the output pixel for pixel.
    aligned: bool,
}

impl<'a> OnGrid<'a> {
    fn new(frame: &'a Frame, width: usize, height: usize) -> Self {
        let aligned = !frame.is_empty() && frame.width == width && frame.height == height;
        Self { frame, aligned }
    }

    /// The pixel under output pixel `(x, y)`, whose coordinates are `(u, v)`.
    #[inline]
    fn at(&self, x: usize, y: usize, u: f32, v: f32) -> [u8; 4] {
        if self.aligned {
            let i = (y * self.frame.width + x) * 4;
            let px = &self.frame.data[i..i + 4];
            [px[0], px[1], px[2], px[3]]
        } else {
            sample_or_black(self.frame, u, v)
        }
    }
}

/// Below this there is nothing to gain from handing the work out.
const PARALLEL_FROM: usize = 128 * 128;

/// At most this many threads, whatever the machine has.
///
/// Starting a thread costs tens of microseconds and this runs every frame of
/// a transition. Past a handful the starting costs more than the extra hands
/// save, and the engine has an encoder and several decoders to share the
/// machine with.
const MAX_BANDS: usize = 8;

/// Walks every output pixel, handing the closure its position.
///
/// Split across threads by bands of rows. A transition is the most expensive
/// thing the engine does — both arrangements composited in full and then
/// blended — and it happens at the one moment an operator is watching
/// closely. Every output pixel is independent of every other, so this is the
/// rare case where the work divides perfectly.
fn for_each_pixel<F>(out: &mut Frame, f: F)
where
    F: Fn(usize, usize, f32, f32) -> [u8; 4] + Sync,
{
    let (w, h) = (out.width, out.height);
    if w == 0 || h == 0 {
        return;
    }

    let row_bytes = w * 4;
    let band = |data: &mut [u8], first_row: usize| {
        for (row, line) in data.chunks_mut(row_bytes).enumerate() {
            let y = first_row + row;
            let v = (y as f32 + 0.5) / h as f32;
            for x in 0..w {
                let u = (x as f32 + 0.5) / w as f32;
                line[x * 4..x * 4 + 4].copy_from_slice(&f(x, y, u, v));
            }
        }
    };

    if w * h < PARALLEL_FROM {
        band(&mut out.data, 0);
        return;
    }

    let bands = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, MAX_BANDS)
        .min(h);
    let rows_each = h.div_ceil(bands);

    std::thread::scope(|scope| {
        for (index, rows) in out.data.chunks_mut(rows_each * row_bytes).enumerate() {
            let band = &band;
            scope.spawn(move || band(rows, index * rows_each));
        }
    });
}

fn fade(from: &Frame, to: &Frame, p: f32, out: &mut Frame) {
    // The default effect and the one that matters most. When both sides are
    // the same size as the output — which they are for every ordinary take —
    // the blend is a straight walk down three byte arrays, with no
    // coordinates, no sampling and nothing to decide per pixel.
    let straight = !from.is_empty()
        && !to.is_empty()
        && from.width == out.width
        && from.height == out.height
        && to.width == out.width
        && to.height == out.height;

    if straight {
        let inv = 1.0 - p;
        let blend = |o: &mut [u8], a: &[u8], b: &[u8]| {
            for ((o, &a), &b) in o.iter_mut().zip(a.iter()).zip(b.iter()) {
                *o = (a as f32 * inv + b as f32 * p).round().clamp(0.0, 255.0) as u8;
            }
        };

        if out.data.len() < PARALLEL_FROM * 4 {
            blend(&mut out.data, &from.data, &to.data);
            return;
        }

        // Split the same way the sampled path is. This is the default effect
        // and the one taken most often, so it is worth every hand available.
        let bands = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .clamp(1, MAX_BANDS);
        let each = out.data.len().div_ceil(bands).next_multiple_of(4);
        std::thread::scope(|scope| {
            for ((o, a), b) in out
                .data
                .chunks_mut(each)
                .zip(from.data.chunks(each))
                .zip(to.data.chunks(each))
            {
                let blend = &blend;
                scope.spawn(move || blend(o, a, b));
            }
        });
        return;
    }

    let a = OnGrid::new(from, out.width, out.height);
    let b = OnGrid::new(to, out.width, out.height);
    for_each_pixel(out, |x, y, u, v| mix(a.at(x, y, u, v), b.at(x, y, u, v), p));
}

fn wipe(from: &Frame, to: &Frame, p: f32, out: &mut Frame, axis: Axis, reverse: bool) {
    // A short soft edge rather than a hard one: a single-pixel boundary crawls
    // and aliases badly once the stream has been through an encoder.
    const SOFTNESS: f32 = 0.01;

    let a = OnGrid::new(from, out.width, out.height);
    let b = OnGrid::new(to, out.width, out.height);
    for_each_pixel(out, |x, y, u, v| {
        let raw = if axis == Axis::Vertical { v } else { u };
        let position = if reverse { 1.0 - raw } else { raw };
        let blend = ((p - position) / SOFTNESS + 0.5).clamp(0.0, 1.0);
        mix(a.at(x, y, u, v), b.at(x, y, u, v), blend)
    });
}

fn slide(from: &Frame, to: &Frame, p: f32, out: &mut Frame, axis: Axis, reverse: bool) {
    let shift = if reverse { -p } else { p };

    for_each_pixel(out, |_x, _y, u, v| {
        let (mut su, mut sv) = (u, v);
        let coordinate = if axis == Axis::Vertical { &mut sv } else { &mut su };
        let moved = *coordinate + shift;

        if (0.0..1.0).contains(&moved) {
            *coordinate = moved;
            sample_or_black(from, su, sv)
        } else {
            // Past the edge of the outgoing shot, so the incoming one occupies
            // the gap it left behind.
            *coordinate = moved - shift.signum();
            sample_or_black(to, su, sv)
        }
    });
}

fn zoom(from: &Frame, to: &Frame, p: f32, out: &mut Frame) {
    // The incoming shot grows from nothing, fading in over the first part so
    // it does not pop into existence as a hard rectangle.
    let scale = p.max(0.001);
    let opacity = (p * 2.0).min(1.0);

    let a = OnGrid::new(from, out.width, out.height);
    for_each_pixel(out, |x, y, u, v| {
        let background = a.at(x, y, u, v);
        let su = (u - 0.5) / scale + 0.5;
        let sv = (v - 0.5) / scale + 0.5;
        if !(0.0..1.0).contains(&su) || !(0.0..1.0).contains(&sv) {
            return background;
        }
        mix(background, sample_or_black(to, su, sv), opacity)
    });
}

fn cross_zoom(from: &Frame, to: &Frame, p: f32, out: &mut Frame) {
    let out_scale = 1.0 + p * 0.8;
    let in_scale = 0.6 + p * 0.4;

    for_each_pixel(out, |_x, _y, u, v| {
        let ou = (u - 0.5) / out_scale + 0.5;
        let ov = (v - 0.5) / out_scale + 0.5;
        let iu = (u - 0.5) / in_scale + 0.5;
        let iv = (v - 0.5) / in_scale + 0.5;
        mix(sample_or_black(from, ou, ov), sample_or_black(to, iu, iv), p)
    });
}

/// Fly, optionally spinning. `spin` is the total rotation in radians.
fn fly(from: &Frame, to: &Frame, p: f32, out: &mut Frame, spin: f32) {
    let scale = 0.25 + 0.75 * p;
    let centre_x = 0.82 - 0.32 * p;
    let centre_y = 0.18 + 0.32 * p;
    // Unwinds to zero as it lands, so the shot finishes square.
    let angle = spin * (1.0 - p);
    let (sin, cos) = angle.sin_cos();

    let a = OnGrid::new(from, out.width, out.height);
    for_each_pixel(out, |x, y, u, v| {
        let background = a.at(x, y, u, v);
        let dx = (u - centre_x) / scale;
        let dy = (v - centre_y) / scale;
        let su = dx * cos - dy * sin + 0.5;
        let sv = dx * sin + dy * cos + 0.5;
        if !(0.0..1.0).contains(&su) || !(0.0..1.0).contains(&sv) {
            return background;
        }
        sample_or_black(to, su, sv)
    });
}

/// Two faces of a turning cube. `pull_back` also shrinks them mid-move.
fn cube(from: &Frame, to: &Frame, p: f32, out: &mut Frame, pull_back: bool) {
    let squeeze = if pull_back {
        1.0 - 0.25 * (p * std::f32::consts::PI).sin()
    } else {
        1.0
    };

    for_each_pixel(out, |_x, _y, u, v| {
        let sv = (v - 0.5) / squeeze + 0.5;
        if !(0.0..1.0).contains(&sv) {
            return [0, 0, 0, 255];
        }

        // Each face is foreshortened toward the edge it is turning away on,
        // which is what sells the shape without real perspective.
        if u + p < 1.0 {
            let face = (u + p - p) / (1.0 - p);
            let shaded = 1.0 - 0.35 * p;
            let px = sample_or_black(from, (face * (1.0 - p) + p).clamp(0.0, 0.999), sv);
            shade(px, shaded)
        } else {
            let face = (u + p - 1.0) / p.max(0.001);
            let shaded = 0.65 + 0.35 * p;
            let px = sample_or_black(to, (face * p).clamp(0.0, 0.999), sv);
            shade(px, shaded)
        }
    });
}

#[inline]
fn shade(px: [u8; 4], factor: f32) -> [u8; 4] {
    [
        (px[0] as f32 * factor) as u8,
        (px[1] as f32 * factor) as u8,
        (px[2] as f32 * factor) as u8,
        px[3],
    ]
}

/// Scales both shots toward each other while crossfading.
///
/// vMix's Merge animates matching inputs between Preview and Program. Without
/// scene-graph correspondence that is not reproducible exactly, so this is the
/// honest approximation: a crossfade with a matched scale move, which reads
/// correctly when the two shots are similar and degrades to a fade when they
/// are not.
fn merge(from: &Frame, to: &Frame, p: f32, out: &mut Frame) {
    let eased = p * p * (3.0 - 2.0 * p);
    let out_scale = 1.0 + 0.12 * eased;
    let in_scale = 1.0 - 0.12 * (1.0 - eased);

    for_each_pixel(out, |_x, _y, u, v| {
        let ou = (u - 0.5) / out_scale + 0.5;
        let ov = (v - 0.5) / out_scale + 0.5;
        let iu = (u - 0.5) / in_scale + 0.5;
        let iv = (v - 0.5) / in_scale + 0.5;
        mix(sample_or_black(from, ou, ov), sample_or_black(to, iu, iv), eased)
    });
}

/// Both halves of the outgoing shot part from the centre.
fn barn_door(from: &Frame, to: &Frame, p: f32, out: &mut Frame) {
    const SOFTNESS: f32 = 0.008;

    let a = OnGrid::new(from, out.width, out.height);
    let b = OnGrid::new(to, out.width, out.height);
    for_each_pixel(out, |x, y, u, v| {
        // Distance from the centre line: 0 in the middle, 1 at either edge.
        // The doors part outward, so the revealed band is everything closer to
        // the centre than the current progress.
        let distance = (u - 0.5).abs() * 2.0;
        let blend = ((p - distance) / SOFTNESS + 0.5).clamp(0.0, 1.0);
        mix(a.at(x, y, u, v), b.at(x, y, u, v), blend)
    });
}

/// The incoming shot rolls down over the outgoing one.
fn roller_door(from: &Frame, to: &Frame, p: f32, out: &mut Frame) {
    let a = OnGrid::new(from, out.width, out.height);
    for_each_pixel(out, |x, y, u, v| {
        if v > p {
            return a.at(x, y, u, v);
        }
        // The shutter shows the bottom of the incoming shot first, as though
        // it were being unrolled from above.
        let sv = 1.0 - (p - v) / p.max(0.001);
        let px = sample_or_black(to, u, sv.clamp(0.0, 0.999));
        // A darkened leading edge reads as the roller itself.
        if p - v < 0.02 {
            shade(px, 0.55)
        } else {
            px
        }
    });
}

/// Dips through a covering source, cutting underneath at the midpoint.
///
/// A real stinger plays a full-screen animation and switches the programme
/// behind it while the screen is covered. With `via` supplying the covering
/// picture, that is exactly what this does — and with a colour source it
/// becomes a dip to colour, which is a useful transition in its own right.
fn stinger(from: &Frame, to: &Frame, via: Option<&Frame>, p: f32, out: &mut Frame) {
    let Some(cover) = via else {
        // No stinger source configured. Falling back to a fade keeps the show
        // running rather than cutting to black.
        fade(from, to, p, out);
        return;
    };

    // Cover rises to full by the midpoint, falls away after it. The programme
    // switch happens while the screen is fully covered, so it is never seen.
    let coverage = if p < 0.5 { p * 2.0 } else { (1.0 - p) * 2.0 };
    let underneath = if p < 0.5 { from } else { to };

    let under = OnGrid::new(underneath, out.width, out.height);
    let over = OnGrid::new(cover, out.width, out.height);
    for_each_pixel(out, |x, y, u, v| {
        let base = under.at(x, y, u, v);
        let cover_px = over.at(x, y, u, v);
        // The cover's own alpha modulates it, so a graphic with transparency
        // works as a proper stinger rather than a solid wipe.
        let alpha = (cover_px[3] as f32 / 255.0) * coverage;
        mix(base, cover_px, alpha)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every effect, timed at the size a production actually runs at.
    ///
    /// Reported as lag and struggle taking a playing clip to air. A
    /// transition composites both arrangements in full and blends them, and
    /// the blend was sampling two 1080p frames per pixel to land back on the
    /// pixel it started from: 144 ms a frame against a 33 ms budget, which
    /// dropped the whole production to under six frames a second.
    ///
    /// The budget here is the blend alone. The two composites either side of
    /// it have to come out of the same 33 ms, so this is held to half.
    #[test]
    #[ignore = "timing; run with --ignored"]
    fn every_effect_blends_within_the_frame_budget() {
        use std::time::Instant;

        const W: usize = 1920;
        const H: usize = 1080;
        let budget_ms = 1000.0 / 30.0 / 2.0;

        let from = Frame::filled(W, H, [40, 90, 160]);
        let to = Frame::filled(W, H, [200, 70, 30]);
        let via = Frame::filled(W, H, [10, 10, 10]);
        let mut out = Frame::new(W, H);

        let mut worst = (Transition::Cut, 0.0f64);
        for kind in Transition::ALL {
            if kind.is_instant() {
                continue;
            }
            // Warmed, so the first run's page faults are not the measurement.
            render(kind, &from, &to, Some(&via), 0.5, &mut out);

            let runs = 5;
            let start = Instant::now();
            for i in 0..runs {
                render(kind, &from, &to, Some(&via), 0.3 + i as f32 * 0.1, &mut out);
            }
            let each = start.elapsed().as_secs_f64() * 1000.0 / runs as f64;
            eprintln!("  {:<22} {each:6.2} ms", kind.label());
            if each > worst.1 {
                worst = (kind, each);
            }
        }

        eprintln!("  worst: {} at {:.2} ms (budget {budget_ms:.1} ms)", worst.0.label(), worst.1);
        assert!(
            worst.1 < budget_ms as f64,
            "{} takes {:.1} ms a frame, over half the {:.1} ms the engine has",
            worst.0.label(),
            worst.1,
            1000.0 / 30.0
        );
    }

    fn red() -> Frame {
        Frame::filled(64, 36, [220, 30, 30])
    }

    fn blue() -> Frame {
        Frame::filled(64, 36, [30, 30, 220])
    }

    fn white() -> Frame {
        Frame::filled(64, 36, [255, 255, 255])
    }

    /// Every effect must land exactly on its endpoints. A transition that
    /// nearly finishes leaves a seam on air.
    #[test]
    fn every_effect_is_exact_at_both_ends() {
        let (from, to, via) = (red(), blue(), white());
        let mut out = Frame::new(0, 0);

        for kind in Transition::ALL {
            render(kind, &from, &to, Some(&via), 0.0, &mut out);
            assert_eq!(
                out.pixel(32, 18),
                Some([220, 30, 30, 255]),
                "{} at progress 0 must be the outgoing shot",
                kind.label()
            );

            render(kind, &from, &to, Some(&via), 1.0, &mut out);
            assert_eq!(
                out.pixel(32, 18),
                Some([30, 30, 220, 255]),
                "{} at progress 1 must be the incoming shot",
                kind.label()
            );
        }
    }

    #[test]
    fn every_effect_produces_a_full_frame_midway() {
        // A partially written buffer shows whatever was there before, which on
        // a reused canvas is the previous frame.
        let (from, to, via) = (red(), blue(), white());
        let mut out = Frame::new(0, 0);

        for kind in Transition::ALL {
            render(kind, &from, &to, Some(&via), 0.5, &mut out);
            assert_eq!(out.width, 64, "{} resized wrongly", kind.label());
            assert_eq!(out.height, 36, "{} resized wrongly", kind.label());
            assert!(
                out.data.iter().any(|&b| b != 0),
                "{} left the canvas blank",
                kind.label()
            );
        }
    }

    #[test]
    fn the_bus_matches_the_vmix_effect_list() {
        // Eighteen effects plus four stingers plus cut.
        assert_eq!(Transition::ALL.len(), 23);
        for expected in [
            "Fade", "Zoom", "Wipe", "Slide", "Fly", "CrossZoom", "FlyRotate", "Cube", "CubeZoom",
            "VerticalWipe", "VerticalSlide", "Merge", "WipeReverse", "SlideReverse",
            "VerticalWipeReverse", "VerticalSlideReverse", "BarnDoor", "RollerDoor", "Stinger 1",
            "Stinger 2", "Stinger 3", "Stinger 4", "Cut",
        ] {
            assert!(
                Transition::ALL.iter().any(|t| t.label() == expected),
                "{expected} is missing from the bus"
            );
        }
    }

    #[test]
    fn cut_is_instant_even_halfway_through() {
        let (from, to) = (red(), blue());
        let mut out = Frame::new(0, 0);
        render(Transition::Cut, &from, &to, None, 0.5, &mut out);
        assert_eq!(out.pixel(32, 18), Some([30, 30, 220, 255]));
    }

    #[test]
    fn a_wipe_and_its_reverse_move_in_opposite_directions() {
        let (from, to) = (red(), blue());
        let mut forward = Frame::new(0, 0);
        let mut backward = Frame::new(0, 0);

        render(Transition::Wipe, &from, &to, None, 0.5, &mut forward);
        render(Transition::WipeReverse, &from, &to, None, 0.5, &mut backward);

        assert!(forward.pixel(8, 18).unwrap()[2] > 150, "forward reveals from the left");
        assert!(backward.pixel(8, 18).unwrap()[0] > 150, "reverse reveals from the right");
        assert!(backward.pixel(56, 18).unwrap()[2] > 150);
    }

    #[test]
    fn a_vertical_wipe_and_its_reverse_move_in_opposite_directions() {
        let (from, to) = (red(), blue());
        let mut forward = Frame::new(0, 0);
        let mut backward = Frame::new(0, 0);

        render(Transition::VerticalWipe, &from, &to, None, 0.5, &mut forward);
        render(Transition::VerticalWipeReverse, &from, &to, None, 0.5, &mut backward);

        assert!(forward.pixel(32, 4).unwrap()[2] > 150, "forward reveals from the top");
        assert!(backward.pixel(32, 4).unwrap()[0] > 150, "reverse reveals from the bottom");
    }

    #[test]
    fn slides_stay_hard_edged_rather_than_blending() {
        // A slide that blends is just a fade with extra steps.
        let (from, to) = (red(), blue());
        let mut out = Frame::new(0, 0);
        render(Transition::Slide, &from, &to, None, 0.5, &mut out);

        let left = out.pixel(8, 18).unwrap();
        let right = out.pixel(56, 18).unwrap();
        assert!(left[0] > 180 && left[2] < 80, "outgoing stays saturated: {left:?}");
        assert!(right[2] > 180 && right[0] < 80, "incoming stays saturated: {right:?}");
    }

    #[test]
    fn slide_reverse_brings_the_new_shot_in_from_the_other_side() {
        let (from, to) = (red(), blue());
        let mut forward = Frame::new(0, 0);
        let mut backward = Frame::new(0, 0);
        render(Transition::Slide, &from, &to, None, 0.5, &mut forward);
        render(Transition::SlideReverse, &from, &to, None, 0.5, &mut backward);

        assert!(forward.pixel(56, 18).unwrap()[2] > 150, "forward enters from the right");
        assert!(backward.pixel(8, 18).unwrap()[2] > 150, "reverse enters from the left");
    }

    #[test]
    fn barn_door_opens_from_the_centre() {
        let (from, to) = (red(), blue());
        let mut out = Frame::new(0, 0);
        render(Transition::BarnDoor, &from, &to, None, 0.4, &mut out);

        let centre = out.pixel(32, 18).unwrap();
        let edge = out.pixel(1, 18).unwrap();
        assert!(centre[2] > 120, "the centre should reveal first: {centre:?}");
        assert!(edge[0] > 120, "the edges should still be the old shot: {edge:?}");
    }

    #[test]
    fn roller_door_comes_down_from_the_top() {
        let (from, to) = (red(), blue());
        let mut out = Frame::new(0, 0);
        render(Transition::RollerDoor, &from, &to, None, 0.5, &mut out);

        let top = out.pixel(32, 3).unwrap();
        let bottom = out.pixel(32, 33).unwrap();
        assert!(top[2] > 100, "the top should be covered first: {top:?}");
        assert!(bottom[0] > 150, "the bottom should still be the old shot: {bottom:?}");
    }

    #[test]
    fn a_stinger_covers_the_screen_at_its_midpoint() {
        // The whole point: the programme switch happens while covered, so the
        // cut is never seen.
        let (from, to, via) = (red(), blue(), white());
        let mut out = Frame::new(0, 0);
        render(Transition::Stinger1, &from, &to, Some(&via), 0.5, &mut out);

        let px = out.pixel(32, 18).unwrap();
        assert!(
            px[0] > 230 && px[1] > 230 && px[2] > 230,
            "the midpoint should be fully covered, got {px:?}"
        );
    }

    #[test]
    fn a_stinger_without_a_source_falls_back_to_a_fade() {
        // Cutting to black because a graphic was not configured would be far
        // worse than quietly fading.
        let (from, to) = (red(), blue());
        let mut out = Frame::new(0, 0);
        render(Transition::Stinger2, &from, &to, None, 0.5, &mut out);

        let px = out.pixel(32, 18).unwrap();
        assert!(
            (100..=150).contains(&px[0]) && (100..=150).contains(&px[2]),
            "expected a fade, got {px:?}"
        );
    }

    #[test]
    fn stinger_slots_map_to_their_numbers() {
        assert_eq!(Transition::Stinger1.stinger_slot(), Some(0));
        assert_eq!(Transition::Stinger4.stinger_slot(), Some(3));
        assert_eq!(Transition::Fade.stinger_slot(), None);
    }

    #[test]
    fn fly_rotate_actually_rotates_where_fly_does_not() {
        let (from, to) = (red(), blue());
        let mut plain = Frame::new(0, 0);
        let mut spun = Frame::new(0, 0);
        render(Transition::Fly, &from, &to, None, 0.35, &mut plain);
        render(Transition::FlyRotate, &from, &to, None, 0.35, &mut spun);
        assert_ne!(plain.data, spun.data, "FlyRotate should differ from Fly");
    }

    #[test]
    fn cube_and_cubezoom_differ() {
        let (from, to) = (red(), blue());
        let mut plain = Frame::new(0, 0);
        let mut zoomed = Frame::new(0, 0);
        render(Transition::Cube, &from, &to, None, 0.5, &mut plain);
        render(Transition::CubeZoom, &from, &to, None, 0.5, &mut zoomed);
        assert_ne!(plain.data, zoomed.data, "CubeZoom should pull back where Cube does not");
    }

    #[test]
    fn progress_outside_the_range_is_clamped() {
        let (from, to) = (red(), blue());
        let mut out = Frame::new(0, 0);
        render(Transition::Fade, &from, &to, None, -5.0, &mut out);
        assert_eq!(out.pixel(10, 10), Some([220, 30, 30, 255]));
        render(Transition::Fade, &from, &to, None, 99.0, &mut out);
        assert_eq!(out.pixel(10, 10), Some([30, 30, 220, 255]));
    }

    #[test]
    fn effects_cope_with_an_empty_incoming_frame() {
        // An input that has not produced a picture yet must not panic the
        // transition, which runs on the render thread.
        let from = red();
        let empty = Frame::new(0, 0);
        let mut out = Frame::new(0, 0);
        for kind in Transition::ALL {
            render(kind, &from, &empty, None, 0.5, &mut out);
        }
    }

    #[test]
    fn effects_cope_with_differently_sized_sources() {
        let from = Frame::filled(32, 18, [220, 30, 30]);
        let to = Frame::filled(128, 72, [30, 30, 220]);
        let mut out = Frame::new(0, 0);
        render(Transition::Fade, &from, &to, None, 0.5, &mut out);
        assert_eq!((out.width, out.height), (128, 72));
    }

    #[test]
    fn labels_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        for kind in Transition::ALL {
            assert!(seen.insert(kind.label()), "duplicate label {}", kind.label());
        }
    }
}
