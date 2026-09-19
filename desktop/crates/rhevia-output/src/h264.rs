//! H.264 bitstream handling for delivery.
//!
//! Encoders emit Annex-B (start-code delimited). FLV, and therefore RTMP,
//! wants AVCC (length-prefixed) plus a separate configuration record carrying
//! SPS and PPS out of band. Getting this conversion subtly wrong is the usual
//! cause of a stream that connects, reports healthy, and shows a black frame.

/// NAL unit types we care about. The rest pass through untouched.
pub mod nal {
    pub const NON_IDR: u8 = 1;
    pub const IDR: u8 = 5;
    pub const SEI: u8 = 6;
    pub const SPS: u8 = 7;
    pub const PPS: u8 = 8;
    pub const AUD: u8 = 9;
}

/// The type field of a NAL header byte.
#[inline]
pub fn nal_type(header: u8) -> u8 {
    header & 0x1F
}

/// Splits an Annex-B buffer into NAL units, without the start codes.
///
/// Handles both 3-byte and 4-byte start codes, which encoders mix freely
/// within one stream.
pub fn split_annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut units = Vec::new();
    let mut starts = Vec::new();

    let mut i = 0;
    while i + 2 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push((i, 3));
            i += 3;
        } else if i + 3 < data.len()
            && data[i] == 0
            && data[i + 1] == 0
            && data[i + 2] == 0
            && data[i + 3] == 1
        {
            starts.push((i, 4));
            i += 4;
        } else {
            i += 1;
        }
    }

    for (idx, &(offset, code_len)) in starts.iter().enumerate() {
        let begin = offset + code_len;
        let end = starts.get(idx + 1).map(|&(next, _)| next).unwrap_or(data.len());
        if begin < end {
            units.push(&data[begin..end]);
        }
    }

    units
}

/// The parameter sets a decoder needs before it can decode anything.
#[derive(Debug, Clone, Default)]
pub struct ParameterSets {
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
}

impl ParameterSets {
    pub fn is_complete(&self) -> bool {
        self.sps.is_some() && self.pps.is_some()
    }

    /// Absorbs any SPS/PPS present in these NAL units.
    ///
    /// Parameter sets can be re-sent mid-stream (and change, on resolution
    /// switch), so this keeps the most recent rather than only the first.
    pub fn absorb(&mut self, units: &[&[u8]]) {
        for unit in units {
            match unit.first().map(|&b| nal_type(b)) {
                Some(nal::SPS) => self.sps = Some(unit.to_vec()),
                Some(nal::PPS) => self.pps = Some(unit.to_vec()),
                _ => {}
            }
        }
    }

    /// Builds the AVCDecoderConfigurationRecord that FLV carries as the AVC
    /// sequence header. Must be sent before any frame, and re-sent whenever
    /// the parameter sets change.
    pub fn to_avc_decoder_config(&self) -> Option<Vec<u8>> {
        let sps = self.sps.as_ref()?;
        let pps = self.pps.as_ref()?;
        // Bytes 1..4 of the SPS are profile_idc, profile_compatibility and
        // level_idc. A shorter SPS is malformed and would panic on slicing.
        if sps.len() < 4 {
            return None;
        }

        let mut out = Vec::with_capacity(16 + sps.len() + pps.len());
        out.push(1); // configurationVersion
        out.push(sps[1]); // AVCProfileIndication
        out.push(sps[2]); // profile_compatibility
        out.push(sps[3]); // AVCLevelIndication
        // 6 reserved bits set, then lengthSizeMinusOne = 3 (4-byte lengths).
        out.push(0xFF);
        // 3 reserved bits set, then numOfSequenceParameterSets = 1.
        out.push(0xE1);
        out.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        out.extend_from_slice(sps);
        out.push(1); // numOfPictureParameterSets
        out.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        out.extend_from_slice(pps);
        Some(out)
    }
}

/// Converts NAL units to AVCC: each prefixed with a 4-byte big-endian length.
///
/// Parameter sets and access unit delimiters are dropped: SPS/PPS travel in the
/// configuration record instead, and an AUD inside an AVCC sample confuses some
/// decoders.
pub fn annexb_to_avcc(units: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for unit in units {
        match unit.first().map(|&b| nal_type(b)) {
            Some(nal::SPS) | Some(nal::PPS) | Some(nal::AUD) => continue,
            None => continue,
            _ => {}
        }
        out.extend_from_slice(&(unit.len() as u32).to_be_bytes());
        out.extend_from_slice(unit);
    }
    out
}

/// True if these NAL units contain an IDR, making this access unit a keyframe.
///
/// FLV marks keyframes explicitly, and platforms use that flag to start
/// playback; mislabelling it produces a stream that only plays for viewers who
/// happened to join at the right moment.
pub fn is_keyframe(units: &[&[u8]]) -> bool {
    units
        .iter()
        .any(|u| u.first().map(|&b| nal_type(b)) == Some(nal::IDR))
}

/// True for VCL NAL types, the ones that actually carry picture data.
#[inline]
fn is_vcl(kind: u8) -> bool {
    (1..=5).contains(&kind)
}

/// Groups NAL units into access units — one per displayed frame.
///
/// An encoder hands us a continuous Annex-B stream; FLV and RTMP need it cut
/// into frames, each with its own timestamp. The rule: a slice NAL starts a new
/// access unit if the current one already has one. Parameter sets and SEI
/// attach to the frame that follows them, which is where a decoder expects to
/// find them.
pub fn split_access_units<'a>(units: &[&'a [u8]]) -> Vec<Vec<&'a [u8]>> {
    let mut access_units: Vec<Vec<&'a [u8]>> = Vec::new();
    let mut current: Vec<&'a [u8]> = Vec::new();
    let mut current_has_vcl = false;

    for unit in units {
        let Some(&header) = unit.first() else { continue };
        let kind = nal_type(header);

        if is_vcl(kind) && current_has_vcl {
            access_units.push(std::mem::take(&mut current));
            current_has_vcl = false;
        }

        current.push(unit);
        if is_vcl(kind) {
            current_has_vcl = true;
        }
    }

    // A trailing group with no picture data is parameter sets with no frame,
    // which is not a displayable access unit.
    if current_has_vcl {
        access_units.push(current);
    }
    access_units
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nal(kind: u8, len: usize) -> Vec<u8> {
        let mut v = vec![kind];
        v.extend(std::iter::repeat(0xAA).take(len));
        v
    }

    #[test]
    fn splits_both_start_code_lengths_in_one_stream() {
        // Encoders mix 3- and 4-byte start codes freely; handling only one
        // silently merges NAL units together.
        let mut stream = Vec::new();
        stream.extend_from_slice(&[0, 0, 0, 1]);
        stream.extend_from_slice(&nal(nal::SPS, 8));
        stream.extend_from_slice(&[0, 0, 1]);
        stream.extend_from_slice(&nal(nal::PPS, 4));
        stream.extend_from_slice(&[0, 0, 0, 1]);
        stream.extend_from_slice(&nal(nal::IDR, 32));

        let units = split_annexb(&stream);
        assert_eq!(units.len(), 3);
        assert_eq!(nal_type(units[0][0]), nal::SPS);
        assert_eq!(nal_type(units[1][0]), nal::PPS);
        assert_eq!(nal_type(units[2][0]), nal::IDR);
        assert_eq!(units[2].len(), 33);
    }

    #[test]
    fn builds_a_decoder_config_with_the_profile_from_the_sps() {
        let mut sets = ParameterSets::default();
        // Baseline 3.1: profile_idc 0x42, compat 0xE0, level 0x1F.
        let sps = vec![0x67, 0x42, 0xE0, 0x1F, 0xAA, 0xBB];
        let pps = vec![0x68, 0xCE, 0x3C, 0x80];
        sets.absorb(&[&sps[..], &pps[..]]);
        assert!(sets.is_complete());

        let config = sets.to_avc_decoder_config().expect("config record");
        assert_eq!(config[0], 1, "configurationVersion");
        assert_eq!(config[1], 0x42, "profile must come from the SPS");
        assert_eq!(config[2], 0xE0);
        assert_eq!(config[3], 0x1F, "level must come from the SPS");
        assert_eq!(config[4], 0xFF, "4-byte NAL lengths");
        assert_eq!(config[5], 0xE1, "exactly one SPS");

        let sps_len = u16::from_be_bytes([config[6], config[7]]) as usize;
        assert_eq!(sps_len, sps.len());
        assert_eq!(&config[8..8 + sps_len], &sps[..]);

        let after = 8 + sps_len;
        assert_eq!(config[after], 1, "exactly one PPS");
        let pps_len = u16::from_be_bytes([config[after + 1], config[after + 2]]) as usize;
        assert_eq!(&config[after + 3..after + 3 + pps_len], &pps[..]);
    }

    #[test]
    fn refuses_to_build_a_config_from_a_truncated_sps() {
        let mut sets = ParameterSets::default();
        let short = vec![0x67, 0x42];
        let pps = vec![0x68, 0xCE];
        sets.absorb(&[&short[..], &pps[..]]);
        assert!(
            sets.to_avc_decoder_config().is_none(),
            "a malformed SPS must not panic or emit a bogus record"
        );
    }

    #[test]
    fn avcc_drops_parameter_sets_and_length_prefixes_the_rest() {
        let sps = nal(nal::SPS, 6);
        let pps = nal(nal::PPS, 3);
        let aud = nal(nal::AUD, 1);
        let idr = nal(nal::IDR, 20);
        let units: Vec<&[u8]> = vec![&sps, &pps, &aud, &idr];

        let avcc = annexb_to_avcc(&units);
        // Only the IDR survives: 4-byte length + 21 bytes of NAL.
        assert_eq!(avcc.len(), 4 + idr.len());
        assert_eq!(u32::from_be_bytes([avcc[0], avcc[1], avcc[2], avcc[3]]) as usize, idr.len());
        assert_eq!(nal_type(avcc[4]), nal::IDR);
    }

    #[test]
    fn identifies_keyframes_by_the_presence_of_an_idr() {
        let idr = nal(nal::IDR, 4);
        let non_idr = nal(nal::NON_IDR, 4);
        let sei = nal(nal::SEI, 2);

        assert!(is_keyframe(&[&sei[..], &idr[..]]));
        assert!(!is_keyframe(&[&sei[..], &non_idr[..]]));
    }

    #[test]
    fn groups_nal_units_into_one_access_unit_per_frame() {
        let sps = nal(nal::SPS, 4);
        let pps = nal(nal::PPS, 2);
        let sei = nal(nal::SEI, 2);
        let idr = nal(nal::IDR, 10);
        let p1 = nal(nal::NON_IDR, 8);
        let p2 = nal(nal::NON_IDR, 8);

        let units: Vec<&[u8]> = vec![&sps, &pps, &sei, &idr, &p1, &p2];
        let access_units = split_access_units(&units);

        assert_eq!(access_units.len(), 3, "one keyframe and two inter frames");
        // Parameter sets and SEI belong to the frame they precede.
        assert_eq!(access_units[0].len(), 4);
        assert!(is_keyframe(&access_units[0]));
        assert_eq!(access_units[1].len(), 1);
        assert!(!is_keyframe(&access_units[1]));
        assert_eq!(access_units[2].len(), 1);
    }

    #[test]
    fn parameter_sets_with_no_following_frame_are_not_an_access_unit() {
        // A stream can end on a trailing SPS/PPS; emitting that as a frame
        // would publish a tag with no picture in it.
        let sps = nal(nal::SPS, 4);
        let pps = nal(nal::PPS, 2);
        let idr = nal(nal::IDR, 10);
        let units: Vec<&[u8]> = vec![&sps, &pps, &idr, &sps, &pps];

        let access_units = split_access_units(&units);
        assert_eq!(access_units.len(), 1);
        assert!(is_keyframe(&access_units[0]));
    }

    #[test]
    fn later_parameter_sets_replace_earlier_ones() {
        // A mid-stream resolution change re-sends these, and the decoder must
        // be reconfigured with the new ones rather than the original.
        let mut sets = ParameterSets::default();
        let first = vec![0x67, 0x42, 0xE0, 0x1F];
        let second = vec![0x67, 0x64, 0x00, 0x28];
        let pps = vec![0x68, 0xCE];
        sets.absorb(&[&first[..], &pps[..]]);
        sets.absorb(&[&second[..]]);

        let config = sets.to_avc_decoder_config().unwrap();
        assert_eq!(config[1], 0x64, "should carry the newest SPS");
    }
}
