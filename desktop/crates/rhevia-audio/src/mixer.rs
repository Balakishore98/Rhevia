//! The audio mixer: channel strips summed into a master bus.
//!
//! Everything here is pure arithmetic on sample buffers, which is why it is
//! testable. The real-time rules that matter later — no allocation, no locks,
//! no logging on the audio thread — are easier to hold if the mixing itself
//! never needs any of them, so nothing in this file allocates during `mix`.

/// Everything runs at this rate internally; captured audio is resampled to it.
pub const SAMPLE_RATE: u32 = 48_000;
/// Stereo throughout. Mono sources are duplicated on capture.
pub const CHANNELS: usize = 2;

/// Interleaved stereo f32, nominal range -1.0..1.0.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioBuffer {
    pub samples: Vec<f32>,
}

impl AudioBuffer {
    /// A silent buffer of `frames` stereo frames.
    pub fn silent(frames: usize) -> Self {
        Self {
            samples: vec![0.0; frames * CHANNELS],
        }
    }

    pub fn from_samples(samples: Vec<f32>) -> Self {
        Self { samples }
    }

    /// Stereo frames, i.e. samples divided by channel count.
    pub fn frames(&self) -> usize {
        self.samples.len() / CHANNELS
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn clear(&mut self) {
        self.samples.fill(0.0);
    }

    pub fn resize(&mut self, frames: usize) {
        self.samples.resize(frames * CHANNELS, 0.0);
    }
}

/// Peak and RMS with broadcast ballistics.
///
/// Peak rises instantly and falls slowly so a transient stays visible long
/// enough to see; RMS tracks perceived loudness. A meter that only showed
/// instantaneous peak would be unreadable at 30 fps — the spike that clipped
/// you would be gone before the next repaint.
#[derive(Debug, Clone, Copy)]
pub struct Meter {
    peak: f32,
    rms: f32,
    /// Latches when a sample exceeds full scale, until explicitly cleared.
    clipped: bool,
}

impl Default for Meter {
    fn default() -> Self {
        Self::new()
    }
}

impl Meter {
    /// How much of the previous peak survives each block. Roughly 20 dB per
    /// second at 48 kHz in ~10 ms blocks.
    const PEAK_DECAY: f32 = 0.85;
    /// RMS follows more slowly still, which is what makes it readable.
    const RMS_SMOOTHING: f32 = 0.75;

    pub fn new() -> Self {
        Self {
            peak: 0.0,
            rms: 0.0,
            clipped: false,
        }
    }

    /// Feeds a block and updates the ballistics.
    pub fn measure(&mut self, buffer: &AudioBuffer) {
        let mut block_peak = 0.0f32;
        let mut sum_squares = 0.0f64;

        for &sample in &buffer.samples {
            let magnitude = sample.abs();
            if magnitude > block_peak {
                block_peak = magnitude;
            }
            // Anything past full scale will clip on the way out, whatever the
            // meter shows afterwards, so latch it.
            if magnitude > 1.0 {
                self.clipped = true;
            }
            sum_squares += (sample as f64) * (sample as f64);
        }

        self.peak = block_peak.max(self.peak * Self::PEAK_DECAY);

        let block_rms = if buffer.samples.is_empty() {
            0.0
        } else {
            (sum_squares / buffer.samples.len() as f64).sqrt() as f32
        };
        self.rms = block_rms.max(self.rms * Self::RMS_SMOOTHING);
    }

    pub fn peak(&self) -> f32 {
        self.peak
    }

    pub fn rms(&self) -> f32 {
        self.rms
    }

    pub fn peak_db(&self) -> f32 {
        amplitude_to_db(self.peak)
    }

    pub fn rms_db(&self) -> f32 {
        amplitude_to_db(self.rms)
    }

    pub fn clipped(&self) -> bool {
        self.clipped
    }

    /// Clears the clip latch. The operator does this deliberately, so a clip
    /// that happened twenty minutes ago is still visible until acknowledged.
    pub fn clear_clip(&mut self) {
        self.clipped = false;
    }
}

/// Silence floor. Below this the meter reads as off rather than as a very
/// large negative number.
pub const SILENCE_DB: f32 = -100.0;

pub fn amplitude_to_db(amplitude: f32) -> f32 {
    if amplitude <= 1e-6 {
        SILENCE_DB
    } else {
        20.0 * amplitude.log10()
    }
}

pub fn db_to_amplitude(db: f32) -> f32 {
    if db <= SILENCE_DB {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

/// One input's audio controls.
#[derive(Debug, Clone)]
pub struct ChannelStrip {
    pub name: String,
    /// Fader position in dB. 0.0 is unity.
    pub gain_db: f32,
    pub muted: bool,
    /// Solo silences every channel that is not soloed.
    pub solo: bool,
    /// -1.0 hard left, 0.0 centre, 1.0 hard right.
    pub pan: f32,
    /// Follows the video switcher: audio comes up only when the input is on
    /// air. Off means the channel is always live, which is what a presenter
    /// microphone wants.
    pub follow_program: bool,
    pub meter: Meter,
}

impl ChannelStrip {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            gain_db: 0.0,
            muted: false,
            solo: false,
            pan: 0.0,
            follow_program: false,
            meter: Meter::new(),
        }
    }

    /// Per-channel gains after fader, mute and pan.
    ///
    /// Constant-power panning: a centred source is -3 dB in each channel
    /// rather than full in both, so panning across does not get louder in the
    /// middle.
    fn channel_gains(&self, audible: bool) -> (f32, f32) {
        if self.muted || !audible {
            return (0.0, 0.0);
        }
        let gain = db_to_amplitude(self.gain_db);
        let angle = (self.pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
        (gain * angle.cos(), gain * angle.sin())
    }
}

/// Sums channel strips into a master bus.
#[derive(Debug, Default)]
pub struct AudioMixer {
    pub channels: Vec<ChannelStrip>,
    pub master_gain_db: f32,
    pub master_muted: bool,
    pub master_meter: Meter,
    /// Reused between blocks so mixing never allocates.
    scratch: AudioBuffer,
}

impl AudioMixer {
    pub fn new() -> Self {
        Self {
            channels: Vec::new(),
            master_gain_db: 0.0,
            master_muted: false,
            master_meter: Meter::new(),
            scratch: AudioBuffer::default(),
        }
    }

    pub fn add_channel(&mut self, name: impl Into<String>) -> usize {
        self.channels.push(ChannelStrip::new(name));
        self.channels.len() - 1
    }

    pub fn remove_channel(&mut self, index: usize) {
        if index < self.channels.len() {
            self.channels.remove(index);
        }
    }

    pub fn channel(&self, index: usize) -> Option<&ChannelStrip> {
        self.channels.get(index)
    }

    pub fn channel_mut(&mut self, index: usize) -> Option<&mut ChannelStrip> {
        self.channels.get_mut(index)
    }

    fn any_solo(&self) -> bool {
        self.channels.iter().any(|c| c.solo)
    }

    /// Mixes `inputs` into the master bus.
    ///
    /// `inputs[i]` supplies channel `i`; `on_air[i]` says whether that input is
    /// currently on Program, which matters for channels set to follow it.
    /// Returns the mixed master buffer.
    pub fn mix(&mut self, inputs: &[Option<&AudioBuffer>], on_air: &[bool], frames: usize) -> &AudioBuffer {
        self.scratch.resize(frames);
        self.scratch.clear();

        let soloing = self.any_solo();

        for (index, channel) in self.channels.iter_mut().enumerate() {
            let Some(Some(input)) = inputs.get(index) else {
                // No audio from this source this block. Still decay its meter,
                // or a disconnected input keeps showing its last level forever.
                channel.meter.measure(&AudioBuffer::silent(0));
                continue;
            };

            // Solo overrides everything except mute; a channel that follows
            // Program is silent while its input is off air.
            let audible = if soloing {
                channel.solo
            } else if channel.follow_program {
                on_air.get(index).copied().unwrap_or(false)
            } else {
                true
            };

            let (left_gain, right_gain) = channel.channel_gains(audible);

            // Meter pre-fader but post-mute, so the operator can see that a
            // muted source is still producing sound — which is how you catch a
            // microphone that was left down.
            channel.meter.measure(input);

            let count = frames.min(input.frames());
            for frame in 0..count {
                let left = input.samples[frame * CHANNELS];
                let right = input.samples[frame * CHANNELS + 1];
                self.scratch.samples[frame * CHANNELS] += left * left_gain;
                self.scratch.samples[frame * CHANNELS + 1] += right * right_gain;
            }
        }

        let master = if self.master_muted {
            0.0
        } else {
            db_to_amplitude(self.master_gain_db)
        };
        for sample in &mut self.scratch.samples {
            *sample *= master;
        }

        self.master_meter.measure(&self.scratch);
        &self.scratch
    }

    /// The last mixed block.
    pub fn output(&self) -> &AudioBuffer {
        &self.scratch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize, amplitude: f32) -> AudioBuffer {
        // A constant value rather than a sine: easier to reason about exactly.
        AudioBuffer::from_samples(vec![amplitude; frames * CHANNELS])
    }

    #[test]
    fn db_conversion_round_trips() {
        assert!((db_to_amplitude(0.0) - 1.0).abs() < 1e-6, "unity is 0 dB");
        assert!((amplitude_to_db(1.0)).abs() < 1e-4);
        assert!((db_to_amplitude(-6.0) - 0.501).abs() < 0.01, "-6 dB halves amplitude");
        assert_eq!(db_to_amplitude(SILENCE_DB), 0.0);
        assert_eq!(amplitude_to_db(0.0), SILENCE_DB, "silence must not be -inf");
    }

    #[test]
    fn a_muted_channel_contributes_nothing() {
        let mut mixer = AudioMixer::new();
        mixer.add_channel("Mic");
        mixer.channel_mut(0).unwrap().muted = true;

        let input = tone(64, 0.5);
        let out = mixer.mix(&[Some(&input)], &[true], 64);
        assert!(out.samples.iter().all(|&s| s == 0.0), "muted must be silent");
    }

    #[test]
    fn a_muted_channel_still_meters() {
        // The whole point: you must be able to see that a muted source is
        // making sound, or you never find the fader that was left down.
        let mut mixer = AudioMixer::new();
        mixer.add_channel("Mic");
        mixer.channel_mut(0).unwrap().muted = true;

        let input = tone(64, 0.5);
        mixer.mix(&[Some(&input)], &[true], 64);
        assert!(
            mixer.channel(0).unwrap().meter.peak() > 0.4,
            "a muted channel should still show level"
        );
    }

    #[test]
    fn the_fader_scales_the_contribution() {
        let mut mixer = AudioMixer::new();
        mixer.add_channel("Mic");
        mixer.channel_mut(0).unwrap().gain_db = -6.0;

        let input = tone(32, 1.0);
        let out = mixer.mix(&[Some(&input)], &[true], 32);
        // -6 dB plus centre-pan's -3 dB is about 0.354.
        assert!(
            (out.samples[0] - 0.354).abs() < 0.02,
            "expected about 0.354, got {}",
            out.samples[0]
        );
    }

    #[test]
    fn centre_pan_is_constant_power_not_double_loud() {
        // Naive panning leaves a centred source 3 dB hotter than a hard-panned
        // one, which makes every pan move a level move too.
        let mut mixer = AudioMixer::new();
        mixer.add_channel("A");
        let input = tone(16, 1.0);

        let centred = mixer.mix(&[Some(&input)], &[true], 16).clone();
        let left_centre = centred.samples[0];
        let right_centre = centred.samples[1];
        assert!((left_centre - right_centre).abs() < 1e-5, "centre must be balanced");
        assert!(
            (left_centre - 0.707).abs() < 0.01,
            "centre should be -3 dB per channel, got {left_centre}"
        );

        mixer.channel_mut(0).unwrap().pan = -1.0;
        let hard_left = mixer.mix(&[Some(&input)], &[true], 16);
        assert!(hard_left.samples[0] > 0.99, "hard left should be full in the left");
        assert!(hard_left.samples[1].abs() < 0.01, "and silent in the right");
    }

    #[test]
    fn solo_silences_everything_else() {
        let mut mixer = AudioMixer::new();
        mixer.add_channel("A");
        mixer.add_channel("B");
        mixer.channel_mut(1).unwrap().solo = true;

        let a = tone(16, 0.4);
        let b = tone(16, 0.2);
        let out = mixer.mix(&[Some(&a), Some(&b)], &[true, true], 16);

        // Only B survives: 0.2 at centre pan.
        assert!(
            (out.samples[0] - 0.2 * 0.707).abs() < 0.01,
            "expected only the soloed channel, got {}",
            out.samples[0]
        );
    }

    #[test]
    fn follow_program_mutes_a_channel_whose_input_is_off_air() {
        let mut mixer = AudioMixer::new();
        mixer.add_channel("Camera");
        mixer.channel_mut(0).unwrap().follow_program = true;

        let input = tone(16, 0.8);
        let off = mixer.mix(&[Some(&input)], &[false], 16).clone();
        assert!(off.samples.iter().all(|&s| s == 0.0), "off air must be silent");

        let on = mixer.mix(&[Some(&input)], &[true], 16);
        assert!(on.samples[0] > 0.3, "on air should be audible");
    }

    #[test]
    fn a_channel_that_does_not_follow_program_stays_live() {
        // A presenter microphone must not cut out when the camera does.
        let mut mixer = AudioMixer::new();
        mixer.add_channel("Presenter Mic");
        let input = tone(16, 0.6);
        let out = mixer.mix(&[Some(&input)], &[false], 16);
        assert!(out.samples[0] > 0.3, "a non-following channel stays live off air");
    }

    #[test]
    fn channels_sum_into_the_master() {
        let mut mixer = AudioMixer::new();
        mixer.add_channel("A");
        mixer.add_channel("B");
        let a = tone(16, 0.3);
        let b = tone(16, 0.2);
        let out = mixer.mix(&[Some(&a), Some(&b)], &[true, true], 16);
        assert!(
            (out.samples[0] - 0.5 * 0.707).abs() < 0.01,
            "expected the sum, got {}",
            out.samples[0]
        );
    }

    #[test]
    fn the_master_fader_and_mute_apply_last() {
        let mut mixer = AudioMixer::new();
        mixer.add_channel("A");
        let input = tone(16, 1.0);

        mixer.master_gain_db = -6.0;
        let quiet = mixer.mix(&[Some(&input)], &[true], 16).clone();
        assert!(quiet.samples[0] < 0.4, "master fader should attenuate");

        mixer.master_muted = true;
        let silent = mixer.mix(&[Some(&input)], &[true], 16);
        assert!(silent.samples.iter().all(|&s| s == 0.0), "master mute is absolute");
    }

    #[test]
    fn clipping_latches_until_acknowledged() {
        // A clip that flashed by unseen is a clip you will ship to air again.
        let mut meter = Meter::new();
        meter.measure(&tone(8, 1.5));
        assert!(meter.clipped());

        for _ in 0..50 {
            meter.measure(&tone(8, 0.1));
        }
        assert!(meter.clipped(), "the latch must survive quiet audio");

        meter.clear_clip();
        assert!(!meter.clipped());
    }

    #[test]
    fn peak_decays_but_does_not_vanish_instantly() {
        let mut meter = Meter::new();
        meter.measure(&tone(64, 0.9));
        let immediately = meter.peak();
        assert!(immediately > 0.85);

        meter.measure(&AudioBuffer::silent(64));
        let after_one_block = meter.peak();
        assert!(
            after_one_block < immediately && after_one_block > 0.5,
            "peak should fall gradually, went {immediately} -> {after_one_block}"
        );
    }

    #[test]
    fn a_missing_input_does_not_freeze_its_meter() {
        let mut mixer = AudioMixer::new();
        mixer.add_channel("Dropped");
        mixer.mix(&[Some(&tone(32, 0.9))], &[true], 32);
        let before = mixer.channel(0).unwrap().meter.peak();

        for _ in 0..30 {
            mixer.mix(&[None], &[true], 32);
        }
        let after = mixer.channel(0).unwrap().meter.peak();
        assert!(
            after < before * 0.1,
            "a source that stopped should fall to silence, {before} -> {after}"
        );
    }

    #[test]
    fn a_short_input_block_does_not_read_past_its_end() {
        let mut mixer = AudioMixer::new();
        mixer.add_channel("Short");
        let short = tone(4, 0.5);
        // Ask for more frames than the input has; this must not panic.
        let out = mixer.mix(&[Some(&short)], &[true], 64);
        assert_eq!(out.frames(), 64);
        assert!(out.samples[4 * CHANNELS] == 0.0, "the tail stays silent");
    }
}
