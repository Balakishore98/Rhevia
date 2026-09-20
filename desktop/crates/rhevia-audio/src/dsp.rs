//! Per-channel DSP: EQ, compressor, noise gate and delay.
//!
//! The chain order matches what vMix documents, because operators carry that
//! mental model between desks: **gate → EQ → compressor → gain → delay**. Gain
//! after the compressor means the fader is makeup, not input trim, which is
//! what people expect when they push a channel up on a compressed source.
//!
//! Everything here is sample-rate aware and allocation-free once built, so it
//! can move to a real-time thread unchanged.

use crate::mixer::{db_to_amplitude, AudioBuffer, CHANNELS, SAMPLE_RATE};

/// A second-order IIR section, the building block of every EQ band.
///
/// Transposed direct form II: fewer state variables than direct form I and
/// better numerical behaviour in f32, which matters when a band sits at 60 Hz
/// against a 48 kHz rate.
#[derive(Debug, Clone, Copy)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Default for Biquad {
    fn default() -> Self {
        Self::bypass()
    }
}

impl Biquad {
    /// Passes audio through untouched.
    pub fn bypass() -> Self {
        Self { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0, z1: 0.0, z2: 0.0 }
    }

    fn from_coefficients(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    /// Bell curve centred on `freq`, the workhorse of a mid band.
    pub fn peaking(freq: f32, q: f32, gain_db: f32) -> Self {
        if gain_db.abs() < 0.01 {
            return Self::bypass();
        }
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = std::f32::consts::TAU * freq / SAMPLE_RATE as f32;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q.max(0.05));

        Self::from_coefficients(
            1.0 + alpha * a,
            -2.0 * cos,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos,
            1.0 - alpha / a,
        )
    }

    /// Lifts or cuts everything below `freq`. This is "bass".
    pub fn low_shelf(freq: f32, gain_db: f32) -> Self {
        if gain_db.abs() < 0.01 {
            return Self::bypass();
        }
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = std::f32::consts::TAU * freq / SAMPLE_RATE as f32;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / 2.0 * std::f32::consts::SQRT_2;
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;

        Self::from_coefficients(
            a * ((a + 1.0) - (a - 1.0) * cos + two_sqrt_a_alpha),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
            a * ((a + 1.0) - (a - 1.0) * cos - two_sqrt_a_alpha),
            (a + 1.0) + (a - 1.0) * cos + two_sqrt_a_alpha,
            -2.0 * ((a - 1.0) + (a + 1.0) * cos),
            (a + 1.0) + (a - 1.0) * cos - two_sqrt_a_alpha,
        )
    }

    /// Lifts or cuts everything above `freq`. This is "presence" or "sharp".
    pub fn high_shelf(freq: f32, gain_db: f32) -> Self {
        if gain_db.abs() < 0.01 {
            return Self::bypass();
        }
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = std::f32::consts::TAU * freq / SAMPLE_RATE as f32;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / 2.0 * std::f32::consts::SQRT_2;
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;

        Self::from_coefficients(
            a * ((a + 1.0) + (a - 1.0) * cos + two_sqrt_a_alpha),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
            a * ((a + 1.0) + (a - 1.0) * cos - two_sqrt_a_alpha),
            (a + 1.0) - (a - 1.0) * cos + two_sqrt_a_alpha,
            2.0 * ((a - 1.0) - (a + 1.0) * cos),
            (a + 1.0) - (a - 1.0) * cos - two_sqrt_a_alpha,
        )
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

/// Four bands, matching the controls people actually reach for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EqSettings {
    pub enabled: bool,
    /// Low shelf at 100 Hz — weight and warmth.
    pub bass_db: f32,
    /// Peaking at 400 Hz — where boxiness and mud live.
    pub low_mid_db: f32,
    /// Peaking at 2.5 kHz — intelligibility on speech.
    pub high_mid_db: f32,
    /// High shelf at 8 kHz — air and sibilance.
    pub presence_db: f32,
}

impl Default for EqSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            bass_db: 0.0,
            low_mid_db: 0.0,
            high_mid_db: 0.0,
            presence_db: 0.0,
        }
    }
}

/// Four-band EQ, one filter set per channel.
#[derive(Debug, Clone)]
pub struct Equaliser {
    settings: EqSettings,
    /// [channel][band]
    bands: [[Biquad; 4]; CHANNELS],
}

impl Default for Equaliser {
    fn default() -> Self {
        Self::new()
    }
}

impl Equaliser {
    pub fn new() -> Self {
        Self {
            settings: EqSettings::default(),
            bands: [[Biquad::bypass(); 4]; CHANNELS],
        }
    }

    pub fn settings(&self) -> EqSettings {
        self.settings
    }

    /// Recomputes coefficients. Called when a control moves, never per sample.
    pub fn set(&mut self, settings: EqSettings) {
        if settings == self.settings {
            return;
        }
        self.settings = settings;

        let designs = [
            Biquad::low_shelf(100.0, settings.bass_db),
            Biquad::peaking(400.0, 0.9, settings.low_mid_db),
            Biquad::peaking(2500.0, 0.9, settings.high_mid_db),
            Biquad::high_shelf(8000.0, settings.presence_db),
        ];
        for channel in 0..CHANNELS {
            for (band, design) in designs.iter().enumerate() {
                // Keep the existing state: recomputing coefficients mid-stream
                // must not click, and a reset here would.
                let z1 = self.bands[channel][band].z1;
                let z2 = self.bands[channel][band].z2;
                self.bands[channel][band] = *design;
                self.bands[channel][band].z1 = z1;
                self.bands[channel][band].z2 = z2;
            }
        }
    }

    pub fn process(&mut self, buffer: &mut AudioBuffer) {
        if !self.settings.enabled {
            return;
        }
        for frame in buffer.samples.chunks_exact_mut(CHANNELS) {
            for (channel, sample) in frame.iter_mut().enumerate() {
                let mut value = *sample;
                for band in 0..4 {
                    value = self.bands[channel][band].process(value);
                }
                *sample = value;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompressorSettings {
    pub enabled: bool,
    /// Level above which gain reduction starts.
    pub threshold_db: f32,
    /// 1.0 is no compression; 4.0 means 4 dB in for 1 dB out.
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    /// Applied after compression, to bring the level back up.
    pub makeup_db: f32,
}

impl Default for CompressorSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_db: -18.0,
            ratio: 3.0,
            attack_ms: 10.0,
            release_ms: 120.0,
            makeup_db: 0.0,
        }
    }
}

/// A feed-forward compressor with a smoothed gain envelope.
#[derive(Debug, Clone)]
pub struct Compressor {
    settings: CompressorSettings,
    /// Current gain reduction in dB, always <= 0.
    envelope_db: f32,
}

impl Default for Compressor {
    fn default() -> Self {
        Self::new()
    }
}

impl Compressor {
    pub fn new() -> Self {
        Self {
            settings: CompressorSettings::default(),
            envelope_db: 0.0,
        }
    }

    pub fn settings(&self) -> CompressorSettings {
        self.settings
    }

    pub fn set(&mut self, settings: CompressorSettings) {
        self.settings = settings;
    }

    /// How much the compressor is currently pulling down, for the meter.
    pub fn gain_reduction_db(&self) -> f32 {
        self.envelope_db
    }

    pub fn process(&mut self, buffer: &mut AudioBuffer) {
        if !self.settings.enabled || self.settings.ratio <= 1.0 {
            self.envelope_db = 0.0;
            return;
        }

        let attack = time_constant(self.settings.attack_ms);
        let release = time_constant(self.settings.release_ms);
        let makeup = db_to_amplitude(self.settings.makeup_db);

        for frame in buffer.samples.chunks_exact_mut(CHANNELS) {
            // Link the channels: compressing them independently would pull the
            // stereo image sideways whenever one side is louder.
            let peak = frame.iter().fold(0.0f32, |acc, &s| acc.max(s.abs()));
            let level_db = if peak <= 1e-6 { -100.0 } else { 20.0 * peak.log10() };

            let over = level_db - self.settings.threshold_db;
            let target_db = if over > 0.0 {
                -(over - over / self.settings.ratio)
            } else {
                0.0
            };

            // Attack when pulling down harder, release when letting go.
            let coefficient = if target_db < self.envelope_db { attack } else { release };
            self.envelope_db += (target_db - self.envelope_db) * coefficient;

            let gain = db_to_amplitude(self.envelope_db) * makeup;
            for sample in frame.iter_mut() {
                *sample *= gain;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GateSettings {
    pub enabled: bool,
    /// Below this the gate closes.
    pub threshold_db: f32,
    pub attack_ms: f32,
    /// Stays open this long after falling below threshold, so speech does not
    /// chatter between words.
    pub hold_ms: f32,
    pub release_ms: f32,
}

impl Default for GateSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_db: -45.0,
            attack_ms: 2.0,
            hold_ms: 120.0,
            release_ms: 180.0,
        }
    }
}

/// A noise gate with a hold time.
#[derive(Debug, Clone)]
pub struct NoiseGate {
    settings: GateSettings,
    /// 0.0 closed, 1.0 open.
    envelope: f32,
    hold_remaining: f32,
}

impl Default for NoiseGate {
    fn default() -> Self {
        Self::new()
    }
}

impl NoiseGate {
    pub fn new() -> Self {
        Self {
            settings: GateSettings::default(),
            envelope: 1.0,
            hold_remaining: 0.0,
        }
    }

    pub fn settings(&self) -> GateSettings {
        self.settings
    }

    pub fn set(&mut self, settings: GateSettings) {
        self.settings = settings;
    }

    /// True while the gate is passing audio, for the indicator.
    pub fn is_open(&self) -> bool {
        self.envelope > 0.5
    }

    pub fn process(&mut self, buffer: &mut AudioBuffer) {
        if !self.settings.enabled {
            self.envelope = 1.0;
            return;
        }

        let attack = time_constant(self.settings.attack_ms);
        let release = time_constant(self.settings.release_ms);
        let frame_seconds = 1.0 / SAMPLE_RATE as f32;

        for frame in buffer.samples.chunks_exact_mut(CHANNELS) {
            let peak = frame.iter().fold(0.0f32, |acc, &s| acc.max(s.abs()));
            let level_db = if peak <= 1e-6 { -100.0 } else { 20.0 * peak.log10() };

            if level_db > self.settings.threshold_db {
                self.hold_remaining = self.settings.hold_ms / 1000.0;
            } else {
                self.hold_remaining = (self.hold_remaining - frame_seconds).max(0.0);
            }

            let target = if level_db > self.settings.threshold_db || self.hold_remaining > 0.0 {
                1.0
            } else {
                0.0
            };
            let coefficient = if target > self.envelope { attack } else { release };
            self.envelope += (target - self.envelope) * coefficient;

            for sample in frame.iter_mut() {
                *sample *= self.envelope;
            }
        }
    }
}

/// A fixed delay, for lip-sync correction against a video path.
#[derive(Debug, Clone)]
pub struct Delay {
    buffer: Vec<f32>,
    write: usize,
    delay_frames: usize,
    milliseconds: f32,
}

impl Default for Delay {
    fn default() -> Self {
        Self::new()
    }
}

impl Delay {
    /// One second is far more than lip-sync ever needs and bounds the memory.
    const MAX_MS: f32 = 1000.0;

    pub fn new() -> Self {
        let capacity = (SAMPLE_RATE as f32 * Self::MAX_MS / 1000.0) as usize * CHANNELS;
        Self {
            buffer: vec![0.0; capacity],
            write: 0,
            delay_frames: 0,
            milliseconds: 0.0,
        }
    }

    pub fn milliseconds(&self) -> f32 {
        self.milliseconds
    }

    pub fn set_milliseconds(&mut self, ms: f32) {
        let ms = ms.clamp(0.0, Self::MAX_MS);
        if (ms - self.milliseconds).abs() < 0.01 {
            return;
        }
        self.milliseconds = ms;
        self.delay_frames = (SAMPLE_RATE as f32 * ms / 1000.0) as usize;
    }

    pub fn process(&mut self, buffer: &mut AudioBuffer) {
        if self.delay_frames == 0 {
            return;
        }
        let capacity_frames = self.buffer.len() / CHANNELS;

        for frame in buffer.samples.chunks_exact_mut(CHANNELS) {
            let read = (self.write + capacity_frames - self.delay_frames) % capacity_frames;
            for channel in 0..CHANNELS {
                let delayed = self.buffer[read * CHANNELS + channel];
                self.buffer[self.write * CHANNELS + channel] = frame[channel];
                frame[channel] = delayed;
            }
            self.write = (self.write + 1) % capacity_frames;
        }
    }
}

/// The whole per-channel chain.
#[derive(Debug, Clone, Default)]
pub struct ChannelDsp {
    pub gate: NoiseGate,
    pub eq: Equaliser,
    pub compressor: Compressor,
    pub delay: Delay,
}

impl ChannelDsp {
    pub fn new() -> Self {
        Self::default()
    }

    /// Gate first so the EQ is not lifting noise, compressor after the EQ so it
    /// reacts to the tone you actually chose, delay last so it does not shift
    /// the dynamics processing in time.
    pub fn process(&mut self, buffer: &mut AudioBuffer) {
        self.gate.process(buffer);
        self.eq.process(buffer);
        self.compressor.process(buffer);
        self.delay.process(buffer);
    }

    /// True if anything in the chain is doing work.
    pub fn is_active(&self) -> bool {
        self.gate.settings().enabled
            || self.eq.settings().enabled
            || self.compressor.settings().enabled
            || self.delay.milliseconds() > 0.0
    }
}

/// One-pole smoothing coefficient for a given time in milliseconds.
fn time_constant(ms: f32) -> f32 {
    if ms <= 0.01 {
        return 1.0;
    }
    1.0 - (-1.0 / (ms / 1000.0 * SAMPLE_RATE as f32)).exp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mixer::amplitude_to_db;

    /// A sine at `freq`, useful for checking what a filter does to a band.
    fn sine(freq: f32, frames: usize, amplitude: f32) -> AudioBuffer {
        let mut samples = Vec::with_capacity(frames * CHANNELS);
        for n in 0..frames {
            let t = n as f32 / SAMPLE_RATE as f32;
            let value = amplitude * (std::f32::consts::TAU * freq * t).sin();
            samples.push(value);
            samples.push(value);
        }
        AudioBuffer::from_samples(samples)
    }

    /// Peak of the second half, after any filter has settled.
    fn settled_peak(buffer: &AudioBuffer) -> f32 {
        let half = buffer.samples.len() / 2;
        buffer.samples[half..].iter().fold(0.0f32, |a, &s| a.max(s.abs()))
    }

    #[test]
    fn a_bypassed_biquad_changes_nothing() {
        let mut b = Biquad::bypass();
        for value in [0.0, 0.5, -0.25, 1.0] {
            assert!((b.process(value) - value).abs() < 1e-6);
        }
    }

    #[test]
    fn the_bass_band_lifts_low_frequencies_and_leaves_highs_alone() {
        let mut eq = Equaliser::new();
        eq.set(EqSettings { enabled: true, bass_db: 12.0, ..Default::default() });

        let mut low = sine(60.0, 4800, 0.2);
        eq.process(&mut low);
        let low_gain = amplitude_to_db(settled_peak(&low)) - amplitude_to_db(0.2);
        assert!(low_gain > 8.0, "60 Hz should be lifted, got {low_gain:.1} dB");

        let mut eq2 = Equaliser::new();
        eq2.set(EqSettings { enabled: true, bass_db: 12.0, ..Default::default() });
        let mut high = sine(8000.0, 4800, 0.2);
        eq2.process(&mut high);
        let high_gain = amplitude_to_db(settled_peak(&high)) - amplitude_to_db(0.2);
        assert!(high_gain.abs() < 2.0, "8 kHz should be untouched, got {high_gain:.1} dB");
    }

    #[test]
    fn the_presence_band_lifts_highs_and_leaves_bass_alone() {
        let mut eq = Equaliser::new();
        eq.set(EqSettings { enabled: true, presence_db: 12.0, ..Default::default() });
        let mut high = sine(12000.0, 4800, 0.2);
        eq.process(&mut high);
        let gain = amplitude_to_db(settled_peak(&high)) - amplitude_to_db(0.2);
        assert!(gain > 8.0, "12 kHz should be lifted, got {gain:.1} dB");

        let mut eq2 = Equaliser::new();
        eq2.set(EqSettings { enabled: true, presence_db: 12.0, ..Default::default() });
        let mut low = sine(60.0, 4800, 0.2);
        eq2.process(&mut low);
        let low_gain = amplitude_to_db(settled_peak(&low)) - amplitude_to_db(0.2);
        assert!(low_gain.abs() < 2.0, "60 Hz should be untouched, got {low_gain:.1} dB");
    }

    #[test]
    fn a_mid_cut_reduces_its_own_band() {
        let mut eq = Equaliser::new();
        eq.set(EqSettings { enabled: true, low_mid_db: -12.0, ..Default::default() });
        let mut buffer = sine(400.0, 4800, 0.3);
        eq.process(&mut buffer);
        let gain = amplitude_to_db(settled_peak(&buffer)) - amplitude_to_db(0.3);
        assert!(gain < -6.0, "400 Hz should be cut, got {gain:.1} dB");
    }

    #[test]
    fn a_disabled_eq_is_truly_transparent() {
        let mut eq = Equaliser::new();
        eq.set(EqSettings { enabled: false, bass_db: 12.0, ..Default::default() });
        let original = sine(1000.0, 480, 0.4);
        let mut buffer = original.clone();
        eq.process(&mut buffer);
        assert_eq!(buffer.samples, original.samples);
    }

    #[test]
    fn the_compressor_pulls_down_loud_audio_and_leaves_quiet_audio_alone() {
        let mut comp = Compressor::new();
        comp.set(CompressorSettings {
            enabled: true,
            threshold_db: -20.0,
            ratio: 4.0,
            attack_ms: 1.0,
            release_ms: 50.0,
            makeup_db: 0.0,
        });

        let mut loud = sine(1000.0, 9600, 0.8);
        comp.process(&mut loud);
        let reduction = amplitude_to_db(settled_peak(&loud)) - amplitude_to_db(0.8);
        assert!(reduction < -4.0, "loud audio should be compressed, got {reduction:.1} dB");
        assert!(comp.gain_reduction_db() < -1.0, "gain reduction should be reported");

        let mut quiet_comp = Compressor::new();
        quiet_comp.set(CompressorSettings {
            enabled: true,
            threshold_db: -20.0,
            ratio: 4.0,
            attack_ms: 1.0,
            release_ms: 50.0,
            makeup_db: 0.0,
        });
        let mut quiet = sine(1000.0, 9600, 0.02);
        quiet_comp.process(&mut quiet);
        let change = amplitude_to_db(settled_peak(&quiet)) - amplitude_to_db(0.02);
        assert!(change.abs() < 1.0, "quiet audio should pass, got {change:.1} dB");
    }

    #[test]
    fn makeup_gain_restores_level_after_compression() {
        let mut comp = Compressor::new();
        comp.set(CompressorSettings {
            enabled: true,
            threshold_db: -20.0,
            ratio: 4.0,
            attack_ms: 1.0,
            release_ms: 50.0,
            makeup_db: 6.0,
        });
        let mut buffer = sine(1000.0, 9600, 0.5);
        comp.process(&mut buffer);
        let with_makeup = amplitude_to_db(settled_peak(&buffer));

        let mut plain = Compressor::new();
        plain.set(CompressorSettings {
            enabled: true,
            threshold_db: -20.0,
            ratio: 4.0,
            attack_ms: 1.0,
            release_ms: 50.0,
            makeup_db: 0.0,
        });
        let mut buffer2 = sine(1000.0, 9600, 0.5);
        plain.process(&mut buffer2);
        let without = amplitude_to_db(settled_peak(&buffer2));

        assert!(
            (with_makeup - without - 6.0).abs() < 0.5,
            "makeup should add 6 dB, added {:.1}",
            with_makeup - without
        );
    }

    #[test]
    fn stereo_is_compressed_together_not_independently() {
        // Independent detection pulls the image sideways whenever one channel
        // is louder, which is very audible on a stereo music bed.
        let mut comp = Compressor::new();
        comp.set(CompressorSettings {
            enabled: true,
            threshold_db: -30.0,
            ratio: 8.0,
            attack_ms: 1.0,
            release_ms: 50.0,
            makeup_db: 0.0,
        });

        // Left loud, right quiet, constant.
        let frames = 4800;
        let mut samples = Vec::new();
        for _ in 0..frames {
            samples.push(0.9);
            samples.push(0.1);
        }
        let mut buffer = AudioBuffer::from_samples(samples);
        comp.process(&mut buffer);

        let half = frames / 2 * CHANNELS;
        let left = buffer.samples[half];
        let right = buffer.samples[half + 1];
        // The 9:1 ratio between the channels must survive.
        assert!(
            (left / right - 9.0).abs() < 0.5,
            "stereo balance should be preserved, got {:.2}",
            left / right
        );
    }

    #[test]
    fn the_gate_closes_on_silence_and_opens_on_speech() {
        let mut gate = NoiseGate::new();
        gate.set(GateSettings {
            enabled: true,
            threshold_db: -40.0,
            attack_ms: 1.0,
            hold_ms: 0.0,
            release_ms: 5.0,
        });

        let mut loud = sine(1000.0, 4800, 0.5);
        gate.process(&mut loud);
        assert!(settled_peak(&loud) > 0.4, "speech should pass the gate");
        assert!(gate.is_open());

        // Hiss well below threshold.
        let mut quiet = sine(1000.0, 24000, 0.001);
        gate.process(&mut quiet);
        assert!(settled_peak(&quiet) < 0.0005, "noise should be gated out");
        assert!(!gate.is_open());
    }

    #[test]
    fn the_gate_holds_open_between_words() {
        // Without hold, a gate chatters on every pause in speech.
        let mut gate = NoiseGate::new();
        gate.set(GateSettings {
            enabled: true,
            threshold_db: -40.0,
            attack_ms: 1.0,
            hold_ms: 200.0,
            release_ms: 5.0,
        });

        let mut word = sine(1000.0, 4800, 0.5);
        gate.process(&mut word);

        // A 50 ms gap, well inside the 200 ms hold.
        let mut gap = sine(1000.0, 2400, 0.0005);
        gate.process(&mut gap);
        assert!(gate.is_open(), "the gate should still be open inside the hold");
    }

    #[test]
    fn delay_shifts_audio_by_the_requested_time() {
        let mut delay = Delay::new();
        delay.set_milliseconds(10.0);
        let expected_frames = (SAMPLE_RATE as f32 * 0.01) as usize;

        // An impulse, then silence.
        let mut samples = vec![0.0; 4800 * CHANNELS];
        samples[0] = 1.0;
        samples[1] = 1.0;
        let mut buffer = AudioBuffer::from_samples(samples);
        delay.process(&mut buffer);

        assert_eq!(buffer.samples[0], 0.0, "the start must now be silent");
        assert!(
            (buffer.samples[expected_frames * CHANNELS] - 1.0).abs() < 1e-5,
            "the impulse should appear {expected_frames} frames later"
        );
    }

    #[test]
    fn zero_delay_is_a_straight_pass_through() {
        let mut delay = Delay::new();
        delay.set_milliseconds(0.0);
        let original = sine(1000.0, 480, 0.4);
        let mut buffer = original.clone();
        delay.process(&mut buffer);
        assert_eq!(buffer.samples, original.samples);
    }

    #[test]
    fn delay_is_bounded_so_a_silly_value_cannot_exhaust_memory() {
        let mut delay = Delay::new();
        delay.set_milliseconds(999_999.0);
        assert!(delay.milliseconds() <= Delay::MAX_MS);
    }

    #[test]
    fn an_untouched_chain_reports_inactive_and_passes_audio_through() {
        let mut dsp = ChannelDsp::new();
        assert!(!dsp.is_active());

        let original = sine(1000.0, 480, 0.4);
        let mut buffer = original.clone();
        dsp.process(&mut buffer);
        assert_eq!(buffer.samples, original.samples, "a bypassed chain must be transparent");
    }

    #[test]
    fn the_chain_reports_active_once_anything_is_switched_on() {
        let mut dsp = ChannelDsp::new();
        dsp.eq.set(EqSettings { enabled: true, bass_db: 3.0, ..Default::default() });
        assert!(dsp.is_active());
    }
}
