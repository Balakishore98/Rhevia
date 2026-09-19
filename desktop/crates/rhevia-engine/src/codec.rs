//! H.264 decode and encode.
//!
//! Built on OpenH264 (BSD-2-Clause), which Cisco also covers for H.264 patent
//! licensing when their binary is used — the one route to a shippable H.264
//! codec that does not drag GPL into the tree. See `docs/02-licensing.md`.
//!
//! Hardware NVENC/NVDEC will be faster and will replace this for the GPU
//! pipeline, but this path works on every machine with no driver assumptions,
//! which makes it the right default and the right fallback.

use openh264::decoder::Decoder;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod};
use openh264::formats::{RgbaSliceU8, YUVBuffer, YUVSource};

use crate::frame::Frame;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("decoder failed: {0}")]
    Decode(String),
    #[error("encoder failed: {0}")]
    Encode(String),
    #[error("encoder was configured for {expected:?} but given a {actual:?} frame")]
    SizeMismatch {
        expected: (usize, usize),
        actual: (usize, usize),
    },
}

/// Decodes Annex-B H.264 into RGBA frames.
pub struct H264Decoder {
    inner: Decoder,
    /// Reused between frames; a 1080p RGBA allocation per frame would dominate
    /// the cost of decoding at all.
    scratch: Vec<u8>,
}

impl H264Decoder {
    pub fn new() -> Result<Self, CodecError> {
        Ok(Self {
            inner: Decoder::new().map_err(|e| CodecError::Decode(e.to_string()))?,
            scratch: Vec::new(),
        })
    }

    /// Decodes one access unit.
    ///
    /// Returns `None` when the decoder needs more data — normal at the start of
    /// a stream, before parameter sets and the first keyframe have arrived.
    pub fn decode(&mut self, annexb: &[u8]) -> Result<Option<Frame>, CodecError> {
        let decoded = self
            .inner
            .decode(annexb)
            .map_err(|e| CodecError::Decode(e.to_string()))?;

        let Some(yuv) = decoded else { return Ok(None) };

        let (width, height) = yuv.dimensions();
        if width == 0 || height == 0 {
            return Ok(None);
        }

        self.scratch.resize(yuv.rgba8_len(), 0);
        yuv.write_rgba8(&mut self.scratch);

        Ok(Some(Frame {
            width,
            height,
            data: std::mem::take(&mut self.scratch),
        }))
    }
}

/// How the encoder should behave.
#[derive(Debug, Clone, Copy)]
pub struct EncoderSettings {
    pub width: usize,
    pub height: usize,
    pub bitrate_bps: u32,
    pub fps: f32,
    /// Frames between keyframes. Platforms want one every 2 seconds or so;
    /// too long and viewers joining wait for a picture, too short and
    /// bitrate is wasted.
    pub keyframe_interval: u32,
}

impl EncoderSettings {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            bitrate_bps: 4_000_000,
            fps: 30.0,
            keyframe_interval: 60,
        }
    }
}

/// Encodes RGBA frames to Annex-B H.264.
pub struct H264Encoder {
    inner: Encoder,
    settings: EncoderSettings,
    /// Reused: colour conversion allocating per frame would show up as jitter.
    yuv: YUVBuffer,
}

impl H264Encoder {
    pub fn new(settings: EncoderSettings) -> Result<Self, CodecError> {
        // OpenH264 requires even dimensions for 4:2:0 chroma.
        if settings.width % 2 != 0 || settings.height % 2 != 0 {
            return Err(CodecError::Encode(format!(
                "dimensions must be even for 4:2:0, got {}x{}",
                settings.width, settings.height
            )));
        }

        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(settings.bitrate_bps))
            .max_frame_rate(FrameRate::from_hz(settings.fps))
            .intra_frame_period(IntraFramePeriod::from_num_frames(settings.keyframe_interval));

        Ok(Self {
            inner: Encoder::with_api_config(openh264::OpenH264API::from_source(), config)
                .map_err(|e| CodecError::Encode(e.to_string()))?,
            settings,
            yuv: YUVBuffer::new(settings.width, settings.height),
        })
    }

    pub fn settings(&self) -> EncoderSettings {
        self.settings
    }

    /// Encodes one frame, returning Annex-B bytes.
    ///
    /// An empty result is not an error: the encoder legitimately emits nothing
    /// for some frames.
    pub fn encode(&mut self, frame: &Frame) -> Result<Vec<u8>, CodecError> {
        if frame.width != self.settings.width || frame.height != self.settings.height {
            return Err(CodecError::SizeMismatch {
                expected: (self.settings.width, self.settings.height),
                actual: (frame.width, frame.height),
            });
        }

        self.yuv
            .read_rgba8(RgbaSliceU8::new(&frame.data, (frame.width, frame.height)));

        let bitstream = self
            .inner
            .encode(&self.yuv)
            .map_err(|e| CodecError::Encode(e.to_string()))?;

        Ok(bitstream.to_vec())
    }

    /// Forces the next frame to be a keyframe.
    ///
    /// Needed whenever a viewer might have just joined, or after a
    /// reconnection: without one, the stream stays black until the next
    /// scheduled keyframe comes round.
    pub fn request_keyframe(&mut self) {
        self.inner.force_intra_frame();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A recognisable test picture: red left half, green right half.
    fn picture(width: usize, height: usize) -> Frame {
        let mut f = Frame::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let rgba = if x < width / 2 {
                    [220, 30, 30, 255]
                } else {
                    [30, 220, 30, 255]
                };
                f.set_pixel(x, y, rgba);
            }
        }
        f
    }

    #[test]
    fn a_frame_survives_encoding_and_decoding() {
        // The round trip is the real test: either half alone can look fine
        // while the pair is useless.
        let mut encoder = H264Encoder::new(EncoderSettings::new(320, 240)).expect("encoder");
        let mut decoder = H264Decoder::new().expect("decoder");
        let source = picture(320, 240);

        let mut decoded: Option<Frame> = None;
        for _ in 0..5 {
            let bitstream = encoder.encode(&source).expect("encode");
            if bitstream.is_empty() {
                continue;
            }
            if let Some(frame) = decoder.decode(&bitstream).expect("decode") {
                decoded = Some(frame);
                break;
            }
        }

        let decoded = decoded.expect("should decode within a few frames");
        assert_eq!((decoded.width, decoded.height), (320, 240));

        // H.264 is lossy, so compare approximately — but the left half must
        // still be clearly red and the right half clearly green, or the
        // colour conversion is wrong somewhere.
        let left = decoded.pixel(80, 120).unwrap();
        let right = decoded.pixel(240, 120).unwrap();
        assert!(
            left[0] > 150 && left[1] < 100,
            "left half should be red, got {left:?}"
        );
        assert!(
            right[1] > 150 && right[0] < 100,
            "right half should be green, got {right:?}"
        );
    }

    #[test]
    fn the_first_encoded_frame_carries_parameter_sets() {
        // Without SPS and PPS nothing downstream can start decoding, so the
        // very first output must contain them.
        let mut encoder = H264Encoder::new(EncoderSettings::new(320, 240)).expect("encoder");
        let bitstream = encoder.encode(&picture(320, 240)).expect("encode");

        let mut kinds = Vec::new();
        let mut i = 0;
        while i + 4 < bitstream.len() {
            if bitstream[i..i + 3] == [0, 0, 1] {
                kinds.push(bitstream[i + 3] & 0x1F);
                i += 3;
            } else if bitstream[i..i + 4] == [0, 0, 0, 1] {
                kinds.push(bitstream[i + 4] & 0x1F);
                i += 4;
            } else {
                i += 1;
            }
        }
        assert!(kinds.contains(&7), "no SPS in the first frame: {kinds:?}");
        assert!(kinds.contains(&8), "no PPS in the first frame: {kinds:?}");
        assert!(kinds.contains(&5), "no IDR in the first frame: {kinds:?}");
    }

    #[test]
    fn a_wrongly_sized_frame_is_refused_rather_than_producing_garbage() {
        let mut encoder = H264Encoder::new(EncoderSettings::new(320, 240)).expect("encoder");
        let err = encoder.encode(&Frame::new(640, 480)).unwrap_err();
        assert!(matches!(err, CodecError::SizeMismatch { .. }), "got {err}");
    }

    #[test]
    fn odd_dimensions_are_rejected_at_construction() {
        // 4:2:0 chroma cannot represent them, and failing here is far clearer
        // than failing deep inside the encoder later.
        assert!(H264Encoder::new(EncoderSettings::new(321, 240)).is_err());
        assert!(H264Encoder::new(EncoderSettings::new(320, 241)).is_err());
    }

    #[test]
    fn a_decoder_given_nonsense_does_not_panic() {
        let mut decoder = H264Decoder::new().expect("decoder");
        // Garbage in must be an error or an empty result, never a crash: this
        // data arrives from the network.
        let _ = decoder.decode(&[0, 0, 0, 1, 0x65, 0xFF, 0xAB, 0x12]);
        let _ = decoder.decode(&[]);
        let _ = decoder.decode(&[0xFF; 64]);
    }
}
