//! FLV tag bodies, which are what RTMP carries as video and audio messages.

/// FLV codec id for H.264.
const CODEC_AVC: u8 = 7;
/// FLV sound format for AAC.
const SOUND_FORMAT_AAC: u8 = 10;

const FRAME_KEY: u8 = 1;
const FRAME_INTER: u8 = 2;

const AVC_SEQUENCE_HEADER: u8 = 0;
const AVC_NALU: u8 = 1;
const AVC_END_OF_SEQUENCE: u8 = 2;

const AAC_SEQUENCE_HEADER: u8 = 0;
const AAC_RAW: u8 = 1;

/// The AVC sequence header: an AVCDecoderConfigurationRecord.
///
/// Must reach the server before any frame. Platforms that receive frames first
/// either drop them or show nothing until the next configuration arrives.
pub fn avc_sequence_header(decoder_config: &[u8]) -> Vec<u8> {
    let mut tag = Vec::with_capacity(5 + decoder_config.len());
    tag.push((FRAME_KEY << 4) | CODEC_AVC);
    tag.push(AVC_SEQUENCE_HEADER);
    tag.extend_from_slice(&[0, 0, 0]); // composition time is always 0 here
    tag.extend_from_slice(decoder_config);
    tag
}

/// One access unit, as length-prefixed AVCC NAL units.
///
/// `composition_time_ms` is the PTS/DTS offset for B-frames. It is signed and
/// 24-bit; with no B-frames it is zero, which is the common case for live.
pub fn avc_frame(avcc: &[u8], keyframe: bool, composition_time_ms: i32) -> Vec<u8> {
    let mut tag = Vec::with_capacity(5 + avcc.len());
    tag.push(((if keyframe { FRAME_KEY } else { FRAME_INTER }) << 4) | CODEC_AVC);
    tag.push(AVC_NALU);
    tag.extend_from_slice(&i24_be(composition_time_ms));
    tag.extend_from_slice(avcc);
    tag
}

/// Signals a clean end of the video stream.
pub fn avc_end_of_sequence() -> Vec<u8> {
    vec![(FRAME_KEY << 4) | CODEC_AVC, AVC_END_OF_SEQUENCE, 0, 0, 0]
}

/// Sample rate and channel layout, as FLV encodes them in the audio tag header.
#[derive(Debug, Clone, Copy)]
pub struct AudioFormat {
    pub stereo: bool,
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self { stereo: true }
    }
}

impl AudioFormat {
    fn header_byte(&self) -> u8 {
        // For AAC, FLV requires the rate and size bits to read 44 kHz / 16-bit
        // regardless of the real format; the truth lives in the AudioSpecificConfig.
        let sound_rate = 3u8; // 44 kHz
        let sound_size = 1u8; // 16-bit
        let sound_type = u8::from(self.stereo);
        (SOUND_FORMAT_AAC << 4) | (sound_rate << 2) | (sound_size << 1) | sound_type
    }
}

/// The AAC sequence header: an AudioSpecificConfig. Send before any audio.
pub fn aac_sequence_header(audio_specific_config: &[u8], format: AudioFormat) -> Vec<u8> {
    let mut tag = Vec::with_capacity(2 + audio_specific_config.len());
    tag.push(format.header_byte());
    tag.push(AAC_SEQUENCE_HEADER);
    tag.extend_from_slice(audio_specific_config);
    tag
}

/// One raw AAC frame, without an ADTS header.
pub fn aac_frame(raw: &[u8], format: AudioFormat) -> Vec<u8> {
    let mut tag = Vec::with_capacity(2 + raw.len());
    tag.push(format.header_byte());
    tag.push(AAC_RAW);
    tag.extend_from_slice(raw);
    tag
}

/// Big-endian signed 24-bit, as FLV stores composition time.
fn i24_be(value: i32) -> [u8; 3] {
    let v = value & 0x00FF_FFFF;
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_header_is_marked_as_a_keyframe_and_config() {
        let tag = avc_sequence_header(&[1, 0x42, 0xE0, 0x1F]);
        assert_eq!(tag[0] >> 4, FRAME_KEY);
        assert_eq!(tag[0] & 0x0F, CODEC_AVC);
        assert_eq!(tag[1], AVC_SEQUENCE_HEADER);
        assert_eq!(&tag[2..5], &[0, 0, 0]);
        assert_eq!(&tag[5..], &[1, 0x42, 0xE0, 0x1F]);
    }

    #[test]
    fn frames_carry_the_right_keyframe_flag() {
        // Platforms use this flag to decide where playback can start, so an
        // inter frame labelled as key produces a stream that only works for
        // viewers who joined at the right instant.
        let key = avc_frame(&[0, 0, 0, 1, 0x65], true, 0);
        assert_eq!(key[0] >> 4, FRAME_KEY);
        assert_eq!(key[1], AVC_NALU);

        let inter = avc_frame(&[0, 0, 0, 1, 0x41], false, 0);
        assert_eq!(inter[0] >> 4, FRAME_INTER);
    }

    #[test]
    fn composition_time_round_trips_including_negatives() {
        let tag = avc_frame(&[0xAA], false, 33);
        assert_eq!(&tag[2..5], &[0, 0, 33]);

        // B-frames produce negative offsets; truncating the sign here shows up
        // as juddering playback rather than an obvious failure.
        let negative = avc_frame(&[0xAA], false, -1);
        assert_eq!(&negative[2..5], &[0xFF, 0xFF, 0xFF]);

        let large = avc_frame(&[0xAA], false, 0x00AB_CDEF);
        assert_eq!(&large[2..5], &[0xAB, 0xCD, 0xEF]);
    }

    #[test]
    fn aac_tags_declare_aac_and_the_channel_layout() {
        let stereo = aac_frame(&[0x21, 0x00], AudioFormat { stereo: true });
        assert_eq!(stereo[0] >> 4, SOUND_FORMAT_AAC);
        assert_eq!(stereo[0] & 0x01, 1, "stereo");
        assert_eq!(stereo[1], AAC_RAW);

        let mono = aac_sequence_header(&[0x12, 0x10], AudioFormat { stereo: false });
        assert_eq!(mono[0] & 0x01, 0, "mono");
        assert_eq!(mono[1], AAC_SEQUENCE_HEADER);
    }

    #[test]
    fn end_of_sequence_is_well_formed() {
        let tag = avc_end_of_sequence();
        assert_eq!(tag.len(), 5);
        assert_eq!(tag[1], AVC_END_OF_SEQUENCE);
    }
}
