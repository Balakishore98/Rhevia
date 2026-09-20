//! AAC encoding for the outgoing stream.
//!
//! FLV, and therefore RTMP, wants raw AAC frames preceded once by an
//! AudioSpecificConfig. The encoder is driven in `Raw` transport mode for
//! exactly that reason: ADTS would wrap every frame in a header the muxer
//! then has to strip.

use fdk_aac::enc::{
    AudioObjectType, BitRate, ChannelMode, Encoder as FdkEncoder, EncoderParams, Transport,
};

/// AAC-LC always works in frames of this many samples per channel. The mixer
/// produces audio a video frame at a time, so a buffer bridges the two.
pub const FRAME_SAMPLES: usize = 1024;

#[derive(Debug, thiserror::Error)]
pub enum AacError {
    #[error("could not start the AAC encoder: {0}")]
    Start(String),
    #[error("AAC encoding failed: {0}")]
    Encode(String),
}

/// One encoded AAC frame, with the time it represents.
#[derive(Debug, Clone)]
pub struct AacFrame {
    pub data: Vec<u8>,
    /// Samples per channel this frame covers, for timestamping.
    pub samples: usize,
}

pub struct AacEncoder {
    inner: FdkEncoder,
    sample_rate: u32,
    channels: usize,
    /// Interleaved i16 waiting to make up a full AAC frame.
    pending: Vec<i16>,
    /// Scratch for the encoder's output, reused between frames.
    scratch: Vec<u8>,
    /// AudioSpecificConfig, which must precede any audio on the wire.
    config: Vec<u8>,
}

impl AacEncoder {
    /// Starts an encoder for `channels` at `sample_rate`.
    pub fn new(sample_rate: u32, channels: usize, bitrate: u32) -> Result<Self, AacError> {
        let params = EncoderParams {
            bit_rate: BitRate::Cbr(bitrate),
            sample_rate,
            // Raw rather than ADTS: FLV carries the configuration separately
            // and wants bare frames after it.
            transport: Transport::Raw,
            channels: if channels >= 2 { ChannelMode::Stereo } else { ChannelMode::Mono },
            audio_object_type: AudioObjectType::Mpeg4LowComplexity,
        };
        let inner = FdkEncoder::new(params).map_err(|e| AacError::Start(format!("{e:?}")))?;

        // The configuration comes from the encoder rather than being built by
        // hand: it has to agree with the encoder exactly, and a mismatch here
        // is silence at the far end.
        let info = inner.info().map_err(|e| AacError::Start(format!("{e:?}")))?;
        let config = info.confBuf[..info.confSize as usize].to_vec();

        Ok(Self {
            inner,
            sample_rate,
            channels: channels.max(1),
            pending: Vec::with_capacity(FRAME_SAMPLES * 2 * 2),
            scratch: vec![0u8; 8192],
            config,
        })
    }

    /// The AudioSpecificConfig for the FLV sequence header.
    pub fn config(&self) -> &[u8] {
        &self.config
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Feeds interleaved f32 samples and returns whatever complete AAC frames
    /// that produced.
    ///
    /// Audio arrives a video frame at a time — 1600 samples at 30 fps — while
    /// AAC works in 1024s, so the two never line up and the remainder has to
    /// carry over.
    pub fn push(&mut self, samples: &[f32]) -> Result<Vec<AacFrame>, AacError> {
        self.pending.reserve(samples.len());
        for &sample in samples {
            // Clamped before conversion: a sample above full scale would wrap
            // to the opposite polarity and arrive as a loud click.
            let clamped = sample.clamp(-1.0, 1.0);
            self.pending.push((clamped * i16::MAX as f32) as i16);
        }

        let per_frame = FRAME_SAMPLES * self.channels;
        let mut frames = Vec::new();

        while self.pending.len() >= per_frame {
            let info = self
                .inner
                .encode(&self.pending[..per_frame], &mut self.scratch)
                .map_err(|e| AacError::Encode(format!("{e:?}")))?;

            let consumed = if info.input_consumed == 0 { per_frame } else { info.input_consumed };
            self.pending.drain(..consumed.min(self.pending.len()));

            if info.output_size > 0 {
                frames.push(AacFrame {
                    data: self.scratch[..info.output_size].to_vec(),
                    samples: FRAME_SAMPLES,
                });
            }
        }

        Ok(frames)
    }

    /// How many samples per channel are still waiting for a full frame.
    pub fn pending_samples(&self) -> usize {
        self.pending.len() / self.channels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sine, so the encoder has something real to work on rather than
    /// silence it may legitimately encode to almost nothing.
    fn tone(samples: usize, channels: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(samples * channels);
        for n in 0..samples {
            let t = n as f32 / 48_000.0;
            let value = 0.4 * (std::f32::consts::TAU * 440.0 * t).sin();
            for _ in 0..channels {
                out.push(value);
            }
        }
        out
    }

    #[test]
    fn an_encoder_starts_and_produces_a_configuration() {
        let encoder = AacEncoder::new(48_000, 2, 128_000).expect("should start");
        assert!(
            !encoder.config().is_empty(),
            "the AudioSpecificConfig must exist or nothing downstream can decode"
        );
        assert_eq!(encoder.sample_rate(), 48_000);
        assert_eq!(encoder.channels(), 2);
    }

    #[test]
    fn a_full_frame_of_audio_produces_encoded_output() {
        let mut encoder = AacEncoder::new(48_000, 2, 128_000).expect("encoder");
        let mut produced = 0;
        // A few frames in: the encoder has latency and may swallow the first.
        for _ in 0..8 {
            produced += encoder.push(&tone(FRAME_SAMPLES, 2)).expect("encode").len();
        }
        assert!(produced > 0, "expected encoded frames, got none");
    }

    #[test]
    fn a_partial_frame_is_held_rather_than_encoded() {
        // Audio arrives 1600 samples at a time against AAC's 1024, so the
        // remainder has to carry over or the stream gains a gap every frame.
        let mut encoder = AacEncoder::new(48_000, 2, 128_000).expect("encoder");
        let frames = encoder.push(&tone(500, 2)).expect("encode");
        assert!(frames.is_empty(), "half a frame should not encode");
        assert_eq!(encoder.pending_samples(), 500);
    }

    #[test]
    fn a_video_sized_block_carries_its_remainder_forward() {
        let mut encoder = AacEncoder::new(48_000, 2, 128_000).expect("encoder");
        // 1600 samples is one video frame of audio at 30 fps.
        encoder.push(&tone(1600, 2)).expect("encode");
        // 1600 - 1024 leaves 576 waiting.
        assert_eq!(encoder.pending_samples(), 1600 - FRAME_SAMPLES);
    }

    #[test]
    fn samples_beyond_full_scale_are_clamped_not_wrapped() {
        // Wrapping turns a loud passage into a burst of noise at the opposite
        // polarity, which is far worse than clipping.
        let mut encoder = AacEncoder::new(48_000, 2, 128_000).expect("encoder");
        let hot: Vec<f32> = vec![4.0; FRAME_SAMPLES * 2];
        assert!(encoder.push(&hot).is_ok(), "hot audio must not fail the encoder");
    }

    #[test]
    fn mono_is_accepted_as_well_as_stereo() {
        let mut encoder = AacEncoder::new(48_000, 1, 96_000).expect("mono encoder");
        assert_eq!(encoder.channels(), 1);
        assert!(encoder.push(&tone(FRAME_SAMPLES, 1)).is_ok());
    }

    #[test]
    fn an_empty_push_is_harmless() {
        let mut encoder = AacEncoder::new(48_000, 2, 128_000).expect("encoder");
        assert!(encoder.push(&[]).expect("encode").is_empty());
    }
}
