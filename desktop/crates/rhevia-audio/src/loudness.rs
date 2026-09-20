//! Loudness metering to ITU-R BS.1770-4.
//!
//! Peak tells you whether you will clip; loudness tells you whether the
//! programme is at the level the platform expects. They are different
//! questions, and a stream mastered by peak alone arrives either far quieter
//! or far louder than everything around it.
//!
//! Momentary is a 400 ms window, short-term 3 s, and integrated is the gated
//! mean over the whole programme — the figure a delivery specification
//! actually names.

use crate::mixer::{AudioBuffer, CHANNELS, SAMPLE_RATE};

/// Below this a block is silence and is excluded from the integrated figure,
/// so pauses do not drag the programme loudness down.
const ABSOLUTE_GATE_LUFS: f64 = -70.0;
/// Blocks more than this far below the ungated mean are also excluded, which
/// is what stops a quiet passage biasing the result.
const RELATIVE_GATE_LU: f64 = -10.0;

/// The offset in the BS.1770 loudness equation.
const LOUDNESS_OFFSET: f64 = -0.691;

/// A cascade of two biquads forming the K-weighting curve: a high shelf that
/// models the acoustic effect of a head, and a high-pass that removes content
/// too low to contribute to perceived loudness.
#[derive(Debug, Clone, Copy)]
struct KWeighting {
    shelf: Section,
    highpass: Section,
}

#[derive(Debug, Clone, Copy, Default)]
struct Section {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl Section {
    fn new(b: [f64; 3], a: [f64; 2]) -> Self {
        Self { b, a, x: [0.0; 2], y: [0.0; 2] }
    }

    #[inline]
    fn process(&mut self, input: f64) -> f64 {
        let output = self.b[0] * input + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x[1] = self.x[0];
        self.x[0] = input;
        self.y[1] = self.y[0];
        self.y[0] = output;
        output
    }
}

impl KWeighting {
    /// Coefficients as specified for 48 kHz in BS.1770-4.
    ///
    /// Everything upstream is resampled to 48 kHz, so these are used directly
    /// rather than being re-derived per rate.
    fn new() -> Self {
        Self {
            shelf: Section::new(
                [1.535_124_859_586_97, -2.691_696_189_406_38, 1.198_392_810_852_85],
                [-1.690_659_293_182_41, 0.732_480_774_215_85],
            ),
            highpass: Section::new([1.0, -2.0, 1.0], [-1.990_047_454_833_98, 0.990_072_250_366_21]),
        }
    }

    #[inline]
    fn process(&mut self, input: f64) -> f64 {
        self.highpass.process(self.shelf.process(input))
    }
}

/// Momentary, short-term and integrated loudness.
#[derive(Debug, Clone)]
pub struct LoudnessMeter {
    weighting: [KWeighting; CHANNELS],
    /// Mean square per 100 ms block, per channel, newest last.
    blocks: Vec<[f64; CHANNELS]>,
    /// Accumulator for the block currently being filled.
    running: [f64; CHANNELS],
    samples_in_block: usize,
    /// Mean squares of every gated block, for the integrated figure.
    history: Vec<f64>,
    true_peak: f32,
}

impl Default for LoudnessMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl LoudnessMeter {
    /// BS.1770 overlaps its gating blocks by 75%, which means a 100 ms step.
    const BLOCK_MS: usize = 100;
    /// Momentary loudness is a 400 ms window: four blocks.
    const MOMENTARY_BLOCKS: usize = 4;
    /// Short-term is 3 s: thirty blocks.
    const SHORT_TERM_BLOCKS: usize = 30;

    pub fn new() -> Self {
        Self {
            weighting: [KWeighting::new(); CHANNELS],
            blocks: Vec::new(),
            running: [0.0; CHANNELS],
            samples_in_block: 0,
            history: Vec::new(),
            true_peak: 0.0,
        }
    }

    fn block_samples() -> usize {
        SAMPLE_RATE as usize * Self::BLOCK_MS / 1000
    }

    /// Feeds a block of interleaved audio.
    pub fn measure(&mut self, buffer: &AudioBuffer) {
        let block_samples = Self::block_samples();

        for frame in buffer.samples.chunks_exact(CHANNELS) {
            for channel in 0..CHANNELS {
                let sample = frame[channel];
                self.true_peak = self.true_peak.max(sample.abs());

                let weighted = self.weighting[channel].process(sample as f64);
                self.running[channel] += weighted * weighted;
            }
            self.samples_in_block += 1;

            if self.samples_in_block >= block_samples {
                let mut block = [0.0f64; CHANNELS];
                for channel in 0..CHANNELS {
                    block[channel] = self.running[channel] / block_samples as f64;
                }
                self.blocks.push(block);
                self.running = [0.0; CHANNELS];
                self.samples_in_block = 0;

                // Only the last three seconds are needed for the live
                // windows; the integrated figure keeps its own history.
                if self.blocks.len() > Self::SHORT_TERM_BLOCKS {
                    let excess = self.blocks.len() - Self::SHORT_TERM_BLOCKS;
                    self.blocks.drain(..excess);
                }

                if self.blocks.len() >= Self::MOMENTARY_BLOCKS {
                    let window = &self.blocks[self.blocks.len() - Self::MOMENTARY_BLOCKS..];
                    let mean = mean_square(window);
                    if loudness_from_mean_square(mean) > ABSOLUTE_GATE_LUFS {
                        self.history.push(mean);
                    }
                }
            }
        }
    }

    /// Loudness over the last 400 ms.
    pub fn momentary_lufs(&self) -> f64 {
        self.window_loudness(Self::MOMENTARY_BLOCKS)
    }

    /// Loudness over the last 3 seconds.
    pub fn short_term_lufs(&self) -> f64 {
        self.window_loudness(Self::SHORT_TERM_BLOCKS)
    }

    /// Gated loudness over everything measured so far.
    ///
    /// This is the figure a delivery specification names, and it is the one to
    /// aim at when setting programme level.
    pub fn integrated_lufs(&self) -> f64 {
        if self.history.is_empty() {
            return ABSOLUTE_GATE_LUFS;
        }

        // The relative gate is computed from the ungated mean, then applied.
        let ungated: f64 = self.history.iter().sum::<f64>() / self.history.len() as f64;
        let threshold = loudness_from_mean_square(ungated) + RELATIVE_GATE_LU;

        let kept: Vec<f64> = self
            .history
            .iter()
            .copied()
            .filter(|&mean| loudness_from_mean_square(mean) > threshold)
            .collect();

        if kept.is_empty() {
            return loudness_from_mean_square(ungated);
        }
        loudness_from_mean_square(kept.iter().sum::<f64>() / kept.len() as f64)
    }

    /// Highest sample seen, as an amplitude.
    pub fn true_peak(&self) -> f32 {
        self.true_peak
    }

    /// Clears everything, for the start of a new programme.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    fn window_loudness(&self, blocks: usize) -> f64 {
        if self.blocks.len() < blocks.min(Self::MOMENTARY_BLOCKS) {
            return ABSOLUTE_GATE_LUFS;
        }
        let take = blocks.min(self.blocks.len());
        let window = &self.blocks[self.blocks.len() - take..];
        loudness_from_mean_square(mean_square(window))
    }
}

/// Mean square across a window, summed over channels as BS.1770 specifies.
///
/// Left and right both carry a weight of 1.0; only surround channels differ,
/// and there are none here.
fn mean_square(window: &[[f64; CHANNELS]]) -> f64 {
    if window.is_empty() {
        return 0.0;
    }
    let mut total = 0.0;
    for channel in 0..CHANNELS {
        let sum: f64 = window.iter().map(|b| b[channel]).sum();
        total += sum / window.len() as f64;
    }
    total
}

fn loudness_from_mean_square(mean: f64) -> f64 {
    if mean <= 1e-12 {
        return ABSOLUTE_GATE_LUFS;
    }
    LOUDNESS_OFFSET + 10.0 * mean.log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sine at `db` dBFS, identical in both channels.
    fn sine(seconds: f32, freq: f32, db: f32) -> AudioBuffer {
        let amplitude = 10f32.powf(db / 20.0);
        let count = (SAMPLE_RATE as f32 * seconds) as usize;
        let mut samples = Vec::with_capacity(count * CHANNELS);
        for n in 0..count {
            let t = n as f32 / SAMPLE_RATE as f32;
            let value = amplitude * (std::f32::consts::TAU * freq * t).sin();
            samples.push(value);
            samples.push(value);
        }
        AudioBuffer::from_samples(samples)
    }

    #[test]
    fn a_minus_twenty_dbfs_tone_reads_about_minus_twenty_lufs() {
        // The reference case from BS.1770: a 1 kHz sine at -20 dBFS in both
        // channels. K-weighting is roughly flat at 1 kHz, and summing two
        // channels adds 3 dB, so the expected figure is about -20.7 LUFS.
        let mut meter = LoudnessMeter::new();
        meter.measure(&sine(3.0, 1000.0, -20.0));

        let momentary = meter.momentary_lufs();
        assert!(
            (momentary - -20.7).abs() < 1.0,
            "expected about -20.7 LUFS, got {momentary:.2}"
        );
    }

    #[test]
    fn halving_the_amplitude_drops_loudness_by_six() {
        let mut loud = LoudnessMeter::new();
        loud.measure(&sine(2.0, 1000.0, -20.0));

        let mut quiet = LoudnessMeter::new();
        quiet.measure(&sine(2.0, 1000.0, -26.0));

        let difference = loud.momentary_lufs() - quiet.momentary_lufs();
        assert!(
            (difference - 6.0).abs() < 0.5,
            "6 dB of level should be 6 LU, got {difference:.2}"
        );
    }

    #[test]
    fn silence_reads_the_absolute_gate_rather_than_negative_infinity() {
        let mut meter = LoudnessMeter::new();
        meter.measure(&AudioBuffer::silent(SAMPLE_RATE as usize));
        assert_eq!(meter.momentary_lufs(), ABSOLUTE_GATE_LUFS);
        assert_eq!(meter.integrated_lufs(), ABSOLUTE_GATE_LUFS);
    }

    #[test]
    fn k_weighting_lifts_high_frequencies_relative_to_low() {
        // The whole point of K-weighting: a bright tone is perceived louder
        // than a low one at the same amplitude, and the meter must agree.
        let mut high = LoudnessMeter::new();
        high.measure(&sine(2.0, 6000.0, -20.0));

        let mut low = LoudnessMeter::new();
        low.measure(&sine(2.0, 60.0, -20.0));

        assert!(
            high.momentary_lufs() > low.momentary_lufs() + 2.0,
            "6 kHz {:.1} should read louder than 60 Hz {:.1}",
            high.momentary_lufs(),
            low.momentary_lufs()
        );
    }

    #[test]
    fn the_gate_keeps_silence_from_dragging_the_integrated_figure_down() {
        // A programme with pauses must not read quieter than the same
        // programme without them.
        let mut with_pauses = LoudnessMeter::new();
        for _ in 0..3 {
            with_pauses.measure(&sine(1.0, 1000.0, -20.0));
            with_pauses.measure(&AudioBuffer::silent(SAMPLE_RATE as usize));
        }

        let mut continuous = LoudnessMeter::new();
        continuous.measure(&sine(3.0, 1000.0, -20.0));

        let difference = (with_pauses.integrated_lufs() - continuous.integrated_lufs()).abs();
        assert!(
            difference < 1.5,
            "pauses shifted the integrated figure by {difference:.2} LU"
        );
    }

    #[test]
    fn short_term_follows_a_level_change_more_slowly_than_momentary() {
        let mut meter = LoudnessMeter::new();
        meter.measure(&sine(3.0, 1000.0, -30.0));
        let before = meter.momentary_lufs();

        // A sudden lift: momentary should move first.
        meter.measure(&sine(0.5, 1000.0, -14.0));
        assert!(meter.momentary_lufs() > before + 5.0, "momentary should react quickly");
        assert!(
            meter.short_term_lufs() < meter.momentary_lufs(),
            "short-term should still be catching up"
        );
    }

    #[test]
    fn true_peak_records_the_loudest_sample_seen() {
        let mut meter = LoudnessMeter::new();
        meter.measure(&sine(0.5, 1000.0, -6.0));
        let expected = 10f32.powf(-6.0 / 20.0);
        assert!(
            (meter.true_peak() - expected).abs() < 0.02,
            "expected about {expected:.3}, got {:.3}",
            meter.true_peak()
        );
    }

    #[test]
    fn resetting_clears_everything() {
        let mut meter = LoudnessMeter::new();
        meter.measure(&sine(1.0, 1000.0, -12.0));
        meter.reset();
        assert_eq!(meter.momentary_lufs(), ABSOLUTE_GATE_LUFS);
        assert_eq!(meter.true_peak(), 0.0);
    }

    #[test]
    fn a_partial_block_does_not_produce_a_reading_yet() {
        // Reporting loudness from 10 ms of audio would be meaningless and
        // would jump wildly at the start of every programme.
        let mut meter = LoudnessMeter::new();
        meter.measure(&sine(0.01, 1000.0, -20.0));
        assert_eq!(meter.momentary_lufs(), ABSOLUTE_GATE_LUFS);
    }
}
