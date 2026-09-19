//! RTP → Annex-B for H.264 (RFC 6184).
//!
//! WebRTC delivers H.264 chopped to fit the MTU. Reassembling it is what turns
//! received packets into frames that can be published, decoded or recorded.
//!
//! The mode that matters is FU-A fragmentation. On a LAN, frames are small
//! enough that a naive implementation handling only single-NAL packets appears
//! to work — and then fails the moment a real camera sends a keyframe over a
//! mobile link, because fragmentation only starts past the MTU.

/// Four-byte start code. Three-byte is legal, but four keeps alignment tidy
/// and every decoder accepts it.
const START_CODE: [u8; 4] = [0, 0, 0, 1];

const FU_A: u8 = 28;
const STAP_A: u8 = 24;

/// One reassembled frame, ready for FLV muxing or a decoder.
#[derive(Debug, Clone)]
pub struct AccessUnit {
    /// Annex-B: start-code delimited NAL units.
    pub annexb: Vec<u8>,
    /// RTP timestamp, 90 kHz for video.
    pub rtp_timestamp: u32,
    /// True if this access unit contains an IDR.
    pub keyframe: bool,
}

impl AccessUnit {
    /// Presentation time in milliseconds, relative to `base`.
    ///
    /// Uses wrapping arithmetic because RTP timestamps are u32 and genuinely
    /// wrap during a long show — about every 13 hours at 90 kHz.
    pub fn millis_since(&self, base: u32) -> u32 {
        self.rtp_timestamp.wrapping_sub(base) / 90
    }
}

/// Reassembles RTP payloads into access units.
#[derive(Debug, Default)]
pub struct H264Depacketizer {
    /// NAL units of the access unit being built, already Annex-B framed.
    pending: Vec<u8>,
    /// Partially received FU-A fragment.
    fragment: Vec<u8>,
    /// True once a fragment has been dropped, so the rest of it is discarded
    /// rather than emitted as a corrupt NAL.
    fragment_broken: bool,
    pending_timestamp: Option<u32>,
    pending_keyframe: bool,
    /// Counts packets we could not use, for diagnostics.
    pub discarded_packets: u64,
}

impl H264Depacketizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one RTP payload.
    ///
    /// `marker` is the RTP marker bit, which for H.264 means "last packet of
    /// this access unit". Returns a frame once one is complete.
    pub fn push(&mut self, payload: &[u8], timestamp: u32, marker: bool) -> Option<AccessUnit> {
        if payload.is_empty() {
            self.discarded_packets += 1;
            return None;
        }

        // A timestamp change also ends an access unit. Some senders never set
        // the marker bit, so relying on it alone loses the last frame of every
        // group.
        if let Some(pending_ts) = self.pending_timestamp {
            if pending_ts != timestamp && !self.pending.is_empty() {
                let finished = self.take(pending_ts);
                self.absorb(payload, timestamp);
                return finished.or_else(|| marker.then(|| self.take(timestamp)).flatten());
            }
        }

        self.absorb(payload, timestamp);

        if marker {
            return self.take(timestamp);
        }
        None
    }

    /// Emits whatever is buffered. Call at end of stream so the final frame is
    /// not stranded.
    pub fn flush(&mut self) -> Option<AccessUnit> {
        let ts = self.pending_timestamp?;
        self.take(ts)
    }

    fn absorb(&mut self, payload: &[u8], timestamp: u32) {
        self.pending_timestamp = Some(timestamp);
        let header = payload[0];
        let kind = header & 0x1F;

        match kind {
            1..=23 => self.push_nal(payload),
            STAP_A => self.absorb_stap_a(payload),
            FU_A => self.absorb_fu_a(payload),
            // STAP-B, MTAP16, MTAP24 and FU-B exist but are not used by any
            // WebRTC sender; counting them is more useful than pretending.
            _ => {
                tracing::debug!(nal_kind = kind, "unsupported RTP H.264 packetisation mode");
                self.discarded_packets += 1;
            }
        }
    }

    /// STAP-A: several whole NAL units in one packet, each 2-byte length prefixed.
    fn absorb_stap_a(&mut self, payload: &[u8]) {
        let mut offset = 1; // skip the STAP-A header
        while offset + 2 <= payload.len() {
            let size = u16::from_be_bytes([payload[offset], payload[offset + 1]]) as usize;
            offset += 2;
            if size == 0 || offset + size > payload.len() {
                // Truncated aggregate: the rest cannot be trusted.
                self.discarded_packets += 1;
                return;
            }
            let nal = &payload[offset..offset + size].to_vec();
            self.push_nal(nal);
            offset += size;
        }
    }

    /// FU-A: one NAL unit spread across several packets.
    fn absorb_fu_a(&mut self, payload: &[u8]) {
        if payload.len() < 3 {
            self.discarded_packets += 1;
            return;
        }

        let indicator = payload[0];
        let fu_header = payload[1];
        let start = fu_header & 0x80 != 0;
        let end = fu_header & 0x40 != 0;
        let nal_kind = fu_header & 0x1F;

        if start {
            // The original NAL header is rebuilt from the indicator's F/NRI
            // bits and the fragment header's type.
            self.fragment.clear();
            self.fragment_broken = false;
            self.fragment.push((indicator & 0xE0) | nal_kind);
        } else if self.fragment.is_empty() {
            // Joined mid-fragment, or the start packet was lost. Everything up
            // to the next start is unusable.
            self.fragment_broken = true;
        }

        if self.fragment_broken {
            self.discarded_packets += 1;
            if end {
                self.fragment.clear();
                self.fragment_broken = false;
            }
            return;
        }

        self.fragment.extend_from_slice(&payload[2..]);

        if end {
            let nal = std::mem::take(&mut self.fragment);
            self.push_nal(&nal);
        }
    }

    fn push_nal(&mut self, nal: &[u8]) {
        let Some(&header) = nal.first() else { return };
        if header & 0x1F == 5 {
            self.pending_keyframe = true;
        }
        self.pending.extend_from_slice(&START_CODE);
        self.pending.extend_from_slice(nal);
    }

    fn take(&mut self, timestamp: u32) -> Option<AccessUnit> {
        if self.pending.is_empty() {
            return None;
        }
        let unit = AccessUnit {
            annexb: std::mem::take(&mut self.pending),
            rtp_timestamp: timestamp,
            keyframe: self.pending_keyframe,
        };
        self.pending_keyframe = false;
        self.pending_timestamp = None;
        Some(unit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nal(kind: u8, len: usize) -> Vec<u8> {
        let mut v = vec![kind & 0x1F];
        v.extend(std::iter::repeat(0xAB).take(len));
        v
    }

    fn nals_in(annexb: &[u8]) -> Vec<u8> {
        // Returns the NAL type of each start-code delimited unit.
        let mut kinds = Vec::new();
        let mut i = 0;
        while i + 4 < annexb.len() {
            if annexb[i..i + 4] == START_CODE {
                kinds.push(annexb[i + 4] & 0x1F);
                i += 4;
            } else {
                i += 1;
            }
        }
        kinds
    }

    #[test]
    fn a_single_nal_packet_becomes_one_annexb_unit() {
        let mut d = H264Depacketizer::new();
        let unit = d.push(&nal(5, 16), 9000, true).expect("a complete frame");
        assert!(unit.keyframe, "NAL type 5 is an IDR");
        assert_eq!(&unit.annexb[..4], &START_CODE);
        assert_eq!(nals_in(&unit.annexb), vec![5]);
    }

    #[test]
    fn fu_a_fragments_reassemble_into_the_original_nal() {
        // The case that works on a LAN and fails on a phone: keyframes exceed
        // the MTU and arrive in pieces.
        let mut d = H264Depacketizer::new();
        let payload: Vec<u8> = (0..300).map(|i| (i % 251) as u8).collect();

        // FU indicator: F=0, NRI=3, type=28. Original NAL type is 5 (IDR).
        let indicator = 0x60 | FU_A;
        let chunks: Vec<&[u8]> = payload.chunks(100).collect();

        for (i, chunk) in chunks.iter().enumerate() {
            let start = i == 0;
            let end = i == chunks.len() - 1;
            let mut pkt = vec![indicator, (u8::from(start) << 7) | (u8::from(end) << 6) | 5];
            pkt.extend_from_slice(chunk);
            let out = d.push(&pkt, 9000, end);
            if !end {
                assert!(out.is_none(), "a frame must not complete mid-fragment");
            } else {
                let unit = out.expect("frame completes on the last fragment");
                assert!(unit.keyframe);
                // Rebuilt NAL: header byte plus the full payload.
                assert_eq!(unit.annexb.len(), 4 + 1 + payload.len());
                assert_eq!(unit.annexb[4] & 0x1F, 5, "NAL type restored from the FU header");
                assert_eq!(unit.annexb[4] & 0xE0, 0x60, "NRI bits restored from the indicator");
                assert_eq!(&unit.annexb[5..], &payload[..]);
            }
        }
    }

    #[test]
    fn a_fragment_missing_its_start_is_discarded_not_emitted_corrupt() {
        // Publishing a half NAL is worse than dropping it: decoders resync
        // from a gap, but garbage can wedge them.
        let mut d = H264Depacketizer::new();
        let indicator = 0x60 | FU_A;

        // Middle fragment first — the start packet was lost.
        let mut middle = vec![indicator, 5];
        middle.extend_from_slice(&[0xAA; 50]);
        assert!(d.push(&middle, 9000, false).is_none());

        let mut end = vec![indicator, 0x40 | 5];
        end.extend_from_slice(&[0xBB; 50]);
        assert!(d.push(&end, 9000, true).is_none(), "a broken fragment must not be emitted");
        assert!(d.discarded_packets >= 2);

        // And the next complete frame must still work.
        let unit = d.push(&nal(5, 16), 12000, true).expect("recovers on the next frame");
        assert_eq!(nals_in(&unit.annexb), vec![5]);
    }

    #[test]
    fn stap_a_unpacks_every_aggregated_nal() {
        // Senders bundle SPS and PPS this way, so losing it means never
        // learning the stream parameters.
        let sps = nal(7, 8);
        let pps = nal(8, 4);
        let mut pkt = vec![0x60 | STAP_A];
        for n in [&sps, &pps] {
            pkt.extend_from_slice(&(n.len() as u16).to_be_bytes());
            pkt.extend_from_slice(n);
        }

        let mut d = H264Depacketizer::new();
        let unit = d.push(&pkt, 9000, true).expect("a frame");
        assert_eq!(nals_in(&unit.annexb), vec![7, 8]);
    }

    #[test]
    fn a_truncated_stap_a_is_rejected_rather_than_read_out_of_bounds() {
        let mut pkt = vec![0x60 | STAP_A];
        pkt.extend_from_slice(&500u16.to_be_bytes()); // claims 500 bytes
        pkt.extend_from_slice(&[0xAA; 10]); // supplies 10

        let mut d = H264Depacketizer::new();
        let out = d.push(&pkt, 9000, true);
        assert!(out.is_none(), "nothing usable in a truncated aggregate");
        assert!(d.discarded_packets >= 1);
    }

    #[test]
    fn a_timestamp_change_ends_an_access_unit_without_a_marker_bit() {
        // Some senders never set the marker bit. Relying on it alone loses a
        // frame every time.
        let mut d = H264Depacketizer::new();
        assert!(d.push(&nal(1, 10), 9000, false).is_none());
        let unit = d
            .push(&nal(1, 10), 12000, false)
            .expect("the timestamp change should close the previous frame");
        assert_eq!(unit.rtp_timestamp, 9000);
        assert!(!unit.keyframe);
    }

    #[test]
    fn several_nals_at_one_timestamp_form_a_single_access_unit() {
        let mut d = H264Depacketizer::new();
        assert!(d.push(&nal(7, 6), 9000, false).is_none());
        assert!(d.push(&nal(8, 3), 9000, false).is_none());
        let unit = d.push(&nal(5, 20), 9000, true).expect("one frame");
        assert_eq!(nals_in(&unit.annexb), vec![7, 8, 5]);
        assert!(unit.keyframe);
    }

    #[test]
    fn flush_releases_a_frame_that_never_got_a_marker() {
        let mut d = H264Depacketizer::new();
        assert!(d.push(&nal(1, 10), 9000, false).is_none());
        let unit = d.flush().expect("flush should release the buffered frame");
        assert_eq!(unit.rtp_timestamp, 9000);
        assert!(d.flush().is_none(), "flushing twice must not duplicate");
    }

    #[test]
    fn timestamps_convert_to_milliseconds_and_survive_wrapping() {
        let unit = AccessUnit {
            annexb: vec![],
            rtp_timestamp: 90_000,
            keyframe: false,
        };
        assert_eq!(unit.millis_since(0), 1000, "90 kHz clock");

        // RTP timestamps wrap roughly every 13 hours; a show can outlast that.
        let wrapped = AccessUnit {
            annexb: vec![],
            rtp_timestamp: 89_999,
            keyframe: false,
        };
        assert_eq!(wrapped.millis_since(u32::MAX), 1000);
    }

    #[test]
    fn empty_payloads_are_counted_not_crashed_on() {
        let mut d = H264Depacketizer::new();
        assert!(d.push(&[], 9000, true).is_none());
        assert_eq!(d.discarded_packets, 1);
    }
}
