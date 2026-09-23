//! Raster frames.
//!
//! RGBA8 throughout. YUV would halve the memory and skip two conversions, and
//! the GPU pipeline will use it — but compositing correctly in YUV means
//! handling chroma siting and subsampling on every blend, and getting that
//! subtly wrong is what makes a switcher look soft. RGBA first, correct;
//! optimise once there is something to measure.

/// An RGBA8 image. `data` is `width * height * 4` bytes, row-major.
///
/// The default is an empty frame, which is what an input that has not produced
/// a picture yet looks like.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the pixels: a 1080p frame is 8 MB of noise in a log.
        f.debug_struct("Frame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.data.len())
            .finish()
    }
}

impl Frame {
    /// A transparent frame.
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            data: vec![0; width * height * 4],
        }
    }

    /// A frame filled with one opaque colour.
    pub fn filled(width: usize, height: usize, rgb: [u8; 3]) -> Self {
        let mut frame = Self::new(width, height);
        for px in frame.data.chunks_exact_mut(4) {
            px[0] = rgb[0];
            px[1] = rgb[1];
            px[2] = rgb[2];
            px[3] = 255;
        }
        frame
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Pixel at (x, y), or None if outside the frame.
    pub fn pixel(&self, x: usize, y: usize) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = (y * self.width + x) * 4;
        Some([self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]])
    }

    pub fn set_pixel(&mut self, x: usize, y: usize, rgba: [u8; 4]) {
        if x >= self.width || y >= self.height {
            return;
        }
        let i = (y * self.width + x) * 4;
        self.data[i..i + 4].copy_from_slice(&rgba);
    }

    /// Resets to fully transparent without reallocating.
    pub fn clear(&mut self) {
        self.data.fill(0);
    }

    /// Fills with one opaque colour, reusing the buffer.
    pub fn fill(&mut self, rgb: [u8; 3]) {
        for px in self.data.chunks_exact_mut(4) {
            px[0] = rgb[0];
            px[1] = rgb[1];
            px[2] = rgb[2];
            px[3] = 255;
        }
    }

    /// Resizes in place if the dimensions differ, reusing the allocation when
    /// it is already large enough. Called every frame, so it must not churn.
    pub fn resize(&mut self, width: usize, height: usize) {
        if self.width == width && self.height == height {
            return;
        }
        self.width = width;
        self.height = height;
        self.data.resize(width * height * 4, 0);
    }

    /// Draws this frame into `out` at whatever size `out` already is.
    ///
    /// Box filtered on the way down rather than sampled: shrinking a 1080p
    /// picture to 360p by taking one pixel in three throws away nine tenths
    /// of the detail and turns every edge into a staircase that an encoder
    /// then spends its bitrate describing. Averaging the pixels that are
    /// being dropped is what makes a small stream look like a small picture
    /// rather than a broken one.
    pub fn scale_into(&self, out: &mut Frame) {
        if self.is_empty() || out.is_empty() {
            return;
        }
        if self.width == out.width && self.height == out.height {
            out.data.copy_from_slice(&self.data);
            return;
        }

        let (ow, oh) = (out.width, out.height);
        // How many source pixels each output pixel covers. Below one this is
        // an enlargement and there is nothing to average, so it samples.
        let sx = self.width as f32 / ow as f32;
        let sy = self.height as f32 / oh as f32;

        if sx <= 1.0 || sy <= 1.0 {
            for y in 0..oh {
                let v = (y as f32 + 0.5) / oh as f32;
                for x in 0..ow {
                    let u = (x as f32 + 0.5) / ow as f32;
                    let px = self.sample(u, v);
                    let i = (y * ow + x) * 4;
                    out.data[i..i + 4].copy_from_slice(&px);
                }
            }
            return;
        }

        for y in 0..oh {
            let y0 = (y as f32 * sy) as usize;
            let y1 = (((y + 1) as f32 * sy) as usize).clamp(y0 + 1, self.height);
            for x in 0..ow {
                let x0 = (x as f32 * sx) as usize;
                let x1 = (((x + 1) as f32 * sx) as usize).clamp(x0 + 1, self.width);

                let mut total = [0u32; 4];
                let mut n = 0u32;
                for sy in y0..y1 {
                    let row = sy * self.width * 4;
                    for sx in x0..x1 {
                        let i = row + sx * 4;
                        for c in 0..4 {
                            total[c] += self.data[i + c] as u32;
                        }
                        n += 1;
                    }
                }
                let n = n.max(1);
                let i = (y * ow + x) * 4;
                for c in 0..4 {
                    out.data[i + c] = (total[c] / n) as u8;
                }
            }
        }
    }

    /// Bilinear sample at normalised coordinates, clamped at the edges.
    ///
    /// Bilinear rather than nearest because a switcher scales constantly —
    /// picture-in-picture, multiview thumbnails, downscaled program — and
    /// nearest-neighbour makes all of it visibly crunchy.
    pub fn sample(&self, u: f32, v: f32) -> [u8; 4] {
        if self.is_empty() {
            return [0, 0, 0, 0];
        }

        let fx = (u * self.width as f32 - 0.5).max(0.0);
        let fy = (v * self.height as f32 - 0.5).max(0.0);
        let x0 = fx.floor() as usize;
        let y0 = fy.floor() as usize;
        let x1 = (x0 + 1).min(self.width - 1);
        let y1 = (y0 + 1).min(self.height - 1);
        let tx = fx - x0 as f32;
        let ty = fy - y0 as f32;

        let x0 = x0.min(self.width - 1);
        let y0 = y0.min(self.height - 1);

        // Indexed directly rather than through `pixel`, which hands back an
        // Option per corner. This runs once per pixel of every scaled layer
        // and twice per pixel of a moving transition -- four million times a
        // frame at 1080p -- so the bounds check and the Option are worth
        // taking out by hand.
        let row0 = y0 * self.width * 4;
        let row1 = y1 * self.width * 4;
        let (i00, i10) = (row0 + x0 * 4, row0 + x1 * 4);
        let (i01, i11) = (row1 + x0 * 4, row1 + x1 * 4);
        let d = &self.data;

        let mut out = [0u8; 4];
        for c in 0..4 {
            let a = d[i00 + c] as f32;
            let b = d[i10 + c] as f32;
            let e = d[i01 + c] as f32;
            let f = d[i11 + c] as f32;
            let top = a + (b - a) * tx;
            let bottom = e + (f - e) * tx;
            // No rounding call and no clamp: interpolating between four
            // values that are already 0..255 cannot leave that range, and
            // adding a half before truncating is what rounding a
            // non-negative number does.
            out[c] = (top + (bottom - top) * ty + 0.5) as u8;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_frame_is_transparent_and_correctly_sized() {
        let f = Frame::new(4, 3);
        assert_eq!(f.data.len(), 4 * 3 * 4);
        assert_eq!(f.pixel(0, 0), Some([0, 0, 0, 0]));
        assert_eq!(f.pixel(4, 0), None, "out of bounds must not panic");
    }

    #[test]
    fn filled_frames_are_opaque() {
        let f = Frame::filled(2, 2, [10, 20, 30]);
        assert_eq!(f.pixel(1, 1), Some([10, 20, 30, 255]));
    }

    #[test]
    fn resizing_keeps_the_buffer_consistent_with_the_dimensions() {
        let mut f = Frame::new(8, 8);
        f.resize(4, 4);
        assert_eq!(f.data.len(), 4 * 4 * 4);
        f.resize(16, 2);
        assert_eq!(f.data.len(), 16 * 2 * 4);
        // A no-op resize must not disturb contents.
        f.fill([1, 2, 3]);
        f.resize(16, 2);
        assert_eq!(f.pixel(0, 0), Some([1, 2, 3, 255]));
    }

    #[test]
    fn bilinear_sampling_interpolates_between_neighbours() {
        // Two pixels, black and white. The midpoint must be grey — with
        // nearest-neighbour it would snap to one or the other.
        let mut f = Frame::new(2, 1);
        f.set_pixel(0, 0, [0, 0, 0, 255]);
        f.set_pixel(1, 0, [255, 255, 255, 255]);

        let mid = f.sample(0.5, 0.5);
        assert!(
            (100..=155).contains(&mid[0]),
            "midpoint should interpolate, got {mid:?}"
        );
        assert_eq!(f.sample(0.0, 0.5)[0], 0, "left edge clamps to black");
        assert_eq!(f.sample(1.0, 0.5)[0], 255, "right edge clamps to white");
    }

    #[test]
    fn sampling_an_empty_frame_is_transparent_rather_than_a_panic() {
        let f = Frame::new(0, 0);
        assert_eq!(f.sample(0.5, 0.5), [0, 0, 0, 0]);
    }
}
