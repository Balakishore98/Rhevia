//! MPEG-TS muxing, for SRT delivery.
//!
//! RTMP carries FLV; SRT carries transport stream. The difference is not
//! cosmetic — TS is packetised into fixed 188-byte cells with its own program
//! tables and clock reference — so this is a second muxer rather than a
//! reshaping of the first.
//!
//! Everything here is a pure function of the frames it is given, so the whole
//! muxer can be checked against a real demuxer without a network.

use crate::h264;

/// Every transport stream packet is this long. The number is not negotiable;
/// it is what every demuxer on earth looks for.
pub const PACKET_SIZE: usize = 188;
const SYNC_BYTE: u8 = 0x47;

pub const PAT_PID: u16 = 0x0000;
pub const PMT_PID: u16 = 0x1000;
pub const VIDEO_PID: u16 = 0x0100;
pub const AUDIO_PID: u16 = 0x0101;

/// Presentation timestamps run at 90 kHz.
pub const CLOCK_HZ: u64 = 90_000;

/// H.264 in a program map.
const STREAM_TYPE_H264: u8 = 0x1b;
/// AAC with ADTS framing. The other AAC code, 0x11, is LATM, which is not
/// what this writes.
const STREAM_TYPE_AAC_ADTS: u8 = 0x0f;

/// How often the program tables are repeated.
///
/// A player that joins mid-stream — which, with a live stream, is every player
/// — cannot decode anything until it has seen both tables, so they go out
/// often enough that joining is quick.
const PSI_INTERVAL_PACKETS: u32 = 40;

/// Builds a transport stream from encoded frames.
pub struct TsMuxer {
    video_cc: u8,
    audio_cc: u8,
    pat_cc: u8,
    pmt_cc: u8,
    has_audio: bool,
    packets_since_psi: u32,
    /// Set once the first table has been written, so the very first frame is
    /// preceded by a program map rather than following one.
    started: bool,
}

impl TsMuxer {
    pub fn new(has_audio: bool) -> Self {
        Self {
            video_cc: 0,
            audio_cc: 0,
            pat_cc: 0,
            pmt_cc: 0,
            has_audio,
            packets_since_psi: PSI_INTERVAL_PACKETS,
            started: false,
        }
    }

    /// Writes one access unit of H.264, given as Annex-B.
    ///
    /// `pts` and `dts` are in 90 kHz units. They differ only when B-frames are
    /// in use; passing the same value for both is correct otherwise.
    pub fn video(&mut self, annexb: &[u8], pts: u64, dts: u64, keyframe: bool, out: &mut Vec<u8>) {
        self.maybe_psi(out);

        // An access unit delimiter in front of each frame. Some demuxers use
        // it to find frame boundaries, and it costs six bytes.
        let mut payload = Vec::with_capacity(annexb.len() + 32);
        payload.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x09, 0xf0]);
        payload.extend_from_slice(annexb);

        let pes = pes_packet(0xe0, &payload, Some(pts), Some(dts), false);
        // The clock reference rides on the video packets, which is where a
        // demuxer expects to find it.
        self.write_pes(VIDEO_PID, &pes, keyframe, Some(dts), true, out);
    }

    /// Writes one AAC frame, which must already carry an ADTS header.
    pub fn audio(&mut self, adts: &[u8], pts: u64, out: &mut Vec<u8>) {
        self.maybe_psi(out);
        // Audio has no reordering, so there is nothing for a DTS to say that
        // the PTS does not.
        let pes = pes_packet(0xc0, adts, Some(pts), None, true);
        self.write_pes(AUDIO_PID, &pes, false, None, false, out);
    }

    /// Writes the program tables now, whatever the interval.
    pub fn write_tables(&mut self, out: &mut Vec<u8>) {
        out.extend_from_slice(&section_packet(PAT_PID, &pat_section(), &mut self.pat_cc));
        out.extend_from_slice(&section_packet(
            PMT_PID,
            &pmt_section(self.has_audio),
            &mut self.pmt_cc,
        ));
        self.packets_since_psi = 0;
        self.started = true;
    }

    fn maybe_psi(&mut self, out: &mut Vec<u8>) {
        if !self.started || self.packets_since_psi >= PSI_INTERVAL_PACKETS {
            self.write_tables(out);
        }
    }

    /// Splits a PES packet across as many transport packets as it needs.
    fn write_pes(
        &mut self,
        pid: u16,
        pes: &[u8],
        random_access: bool,
        pcr: Option<u64>,
        is_video: bool,
        out: &mut Vec<u8>,
    ) {
        let mut offset = 0;
        let mut first = true;

        while offset < pes.len() {
            let counter = if is_video { &mut self.video_cc } else { &mut self.audio_cc };
            let cc = *counter;
            *counter = (*counter + 1) & 0x0f;

            // Only the first packet of a frame carries the adaptation field
            // that holds the clock and the random-access flag.
            let adaptation = if first {
                let mut field = Vec::new();
                let mut flags = 0u8;
                if random_access {
                    flags |= 0x40;
                }
                if pcr.is_some() {
                    flags |= 0x10;
                }
                field.push(flags);
                if let Some(pcr) = pcr {
                    field.extend_from_slice(&encode_pcr(pcr));
                }
                Some(field)
            } else {
                None
            };

            let remaining = pes.len() - offset;
            let packet = ts_packet(
                pid,
                first,
                cc,
                adaptation,
                &pes[offset..],
                remaining,
            );
            offset += packet.consumed;
            out.extend_from_slice(&packet.bytes);
            self.packets_since_psi += 1;
            first = false;
        }
    }
}

struct BuiltPacket {
    bytes: [u8; PACKET_SIZE],
    consumed: usize,
}

/// Assembles one 188-byte packet, padding with an adaptation field when the
/// payload does not fill it.
///
/// The padding matters: a short final packet would desynchronise everything
/// after it, because a demuxer finds packets by counting 188 bytes from the
/// last sync byte.
fn ts_packet(
    pid: u16,
    payload_start: bool,
    continuity: u8,
    adaptation: Option<Vec<u8>>,
    payload: &[u8],
    payload_len: usize,
) -> BuiltPacket {
    let mut bytes = [0xffu8; PACKET_SIZE];
    bytes[0] = SYNC_BYTE;
    bytes[1] = ((payload_start as u8) << 6) | ((pid >> 8) as u8 & 0x1f);
    bytes[2] = (pid & 0xff) as u8;

    let mut adaptation = adaptation;

    // Space left for payload once the four-byte header is accounted for.
    let mut available = PACKET_SIZE - 4;
    if let Some(field) = &adaptation {
        // One length byte plus the field itself.
        available -= 1 + field.len();
    }

    // When what is left of the frame would leave the packet short, the gap is
    // filled by growing the adaptation field rather than by trailing rubbish.
    let take = payload_len.min(available);
    let stuffing = available - take;

    if stuffing > 0 && adaptation.is_none() {
        // A field has to exist before it can be padded. One byte of flags,
        // then stuffing; the single-byte case has no flags at all.
        adaptation = Some(if stuffing == 1 { Vec::new() } else { vec![0u8] });
    }

    let mut index = 4;
    match &adaptation {
        Some(field) => {
            bytes[3] = 0x30 | (continuity & 0x0f); // adaptation field and payload
            let total_field_len = PACKET_SIZE - 4 - 1 - take;
            bytes[index] = total_field_len as u8;
            index += 1;
            if total_field_len > 0 {
                let copy = field.len().min(total_field_len);
                bytes[index..index + copy].copy_from_slice(&field[..copy]);
                // The rest is stuffing, already 0xff from the fill above.
                index += total_field_len;
            }
        }
        None => {
            bytes[3] = 0x10 | (continuity & 0x0f); // payload only
        }
    }

    bytes[index..index + take].copy_from_slice(&payload[..take]);

    BuiltPacket { bytes, consumed: take }
}

/// Wraps a PSI section in a single transport packet.
///
/// Both tables written here fit comfortably, so the multi-packet case does not
/// arise and is not handled.
fn section_packet(pid: u16, section: &[u8], continuity: &mut u8) -> [u8; PACKET_SIZE] {
    let mut bytes = [0xffu8; PACKET_SIZE];
    bytes[0] = SYNC_BYTE;
    bytes[1] = 0x40 | ((pid >> 8) as u8 & 0x1f); // payload starts here
    bytes[2] = (pid & 0xff) as u8;
    bytes[3] = 0x10 | (*continuity & 0x0f);
    *continuity = (*continuity + 1) & 0x0f;

    // The pointer field: how far past it the section begins. Always zero here.
    bytes[4] = 0x00;
    bytes[5..5 + section.len()].copy_from_slice(section);
    bytes
}

/// The program association table: one program, pointing at the program map.
fn pat_section() -> Vec<u8> {
    let mut body = Vec::new();
    body.push(0x00); // table_id
    body.extend_from_slice(&[0x00, 0x00]); // length, filled in below
    body.extend_from_slice(&[0x00, 0x01]); // transport_stream_id
    body.push(0xc1); // current, version 0
    body.push(0x00); // section_number
    body.push(0x00); // last_section_number
    body.extend_from_slice(&[0x00, 0x01]); // program_number 1
    body.extend_from_slice(&[(0xe0 | (PMT_PID >> 8) as u8), (PMT_PID & 0xff) as u8]);

    finish_section(body)
}

/// The program map: which PID carries which kind of stream.
fn pmt_section(has_audio: bool) -> Vec<u8> {
    let mut body = Vec::new();
    body.push(0x02); // table_id
    body.extend_from_slice(&[0x00, 0x00]); // length
    body.extend_from_slice(&[0x00, 0x01]); // program_number
    body.push(0xc1); // current, version 0
    body.push(0x00);
    body.push(0x00);
    body.extend_from_slice(&[(0xe0 | (VIDEO_PID >> 8) as u8), (VIDEO_PID & 0xff) as u8]);
    body.extend_from_slice(&[0xf0, 0x00]); // no program descriptors

    body.push(STREAM_TYPE_H264);
    body.extend_from_slice(&[(0xe0 | (VIDEO_PID >> 8) as u8), (VIDEO_PID & 0xff) as u8]);
    body.extend_from_slice(&[0xf0, 0x00]);

    if has_audio {
        body.push(STREAM_TYPE_AAC_ADTS);
        body.extend_from_slice(&[(0xe0 | (AUDIO_PID >> 8) as u8), (AUDIO_PID & 0xff) as u8]);
        body.extend_from_slice(&[0xf0, 0x00]);
    }

    finish_section(body)
}

/// Fills in a section's length field and appends its CRC.
fn finish_section(mut body: Vec<u8>) -> Vec<u8> {
    // Length covers everything after the length field itself, plus the CRC.
    let length = body.len() - 3 + 4;
    body[1] = 0xb0 | ((length >> 8) as u8 & 0x0f);
    body[2] = (length & 0xff) as u8;

    let crc = mpeg_crc32(&body);
    body.extend_from_slice(&crc.to_be_bytes());
    body
}

/// Builds a PES packet around one frame.
fn pes_packet(
    stream_id: u8,
    payload: &[u8],
    pts: Option<u64>,
    dts: Option<u64>,
    known_length: bool,
) -> Vec<u8> {
    let mut header = Vec::new();
    let mut flags = 0u8;
    if pts.is_some() {
        flags |= 0x80;
    }
    if dts.is_some() {
        flags |= 0x40;
    }

    if let Some(pts) = pts {
        // The marker nibble differs depending on whether a DTS follows: 0x3
        // when it does, 0x2 when the PTS stands alone.
        header.extend_from_slice(&encode_timestamp(if dts.is_some() { 0x3 } else { 0x2 }, pts));
    }
    if let Some(dts) = dts {
        header.extend_from_slice(&encode_timestamp(0x1, dts));
    }

    let mut pes = Vec::with_capacity(payload.len() + 32);
    pes.extend_from_slice(&[0x00, 0x00, 0x01, stream_id]);

    // Video frames routinely exceed 65535 bytes, and the field is 16 bits.
    // Zero means "until the next start", which is legal for video only — so
    // audio, which must state its length, is never given a zero here.
    let declared = if known_length {
        (3 + header.len() + payload.len()) as u16
    } else {
        let total = 3 + header.len() + payload.len();
        if total <= u16::MAX as usize {
            total as u16
        } else {
            0
        }
    };
    pes.extend_from_slice(&declared.to_be_bytes());

    pes.push(0x80); // marker bits, no scrambling
    pes.push(flags);
    pes.push(header.len() as u8);
    pes.extend_from_slice(&header);
    pes.extend_from_slice(payload);
    pes
}

/// A 33-bit timestamp, split across five bytes with marker bits between the
/// pieces — which is why it cannot simply be written as an integer.
fn encode_timestamp(prefix: u8, value: u64) -> [u8; 5] {
    let v = value & 0x1_ffff_ffff;
    [
        (prefix << 4) | (((v >> 30) as u8) & 0x07) << 1 | 0x01,
        ((v >> 22) & 0xff) as u8,
        ((((v >> 15) & 0x7f) as u8) << 1) | 0x01,
        ((v >> 7) & 0xff) as u8,
        ((((v) & 0x7f) as u8) << 1) | 0x01,
    ]
}

/// The program clock reference: a 33-bit base at 90 kHz and a 9-bit extension
/// at 27 MHz.
fn encode_pcr(base_90k: u64) -> [u8; 6] {
    let base = base_90k & 0x1_ffff_ffff;
    let ext: u64 = 0;
    [
        ((base >> 25) & 0xff) as u8,
        ((base >> 17) & 0xff) as u8,
        ((base >> 9) & 0xff) as u8,
        ((base >> 1) & 0xff) as u8,
        ((((base & 0x01) as u8) << 7) | 0x7e) | ((ext >> 8) as u8 & 0x01),
        (ext & 0xff) as u8,
    ]
}

/// CRC-32/MPEG-2: no input or output reflection, no final xor. A reflected
/// variant here produces tables every demuxer silently discards.
pub fn mpeg_crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= (byte as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04c1_1db7 } else { crc << 1 };
        }
    }
    crc
}

/// Wraps a raw AAC frame in the ADTS header transport stream expects.
///
/// The encoder produces bare frames for FLV, which carries the configuration
/// separately. TS does not, so each frame has to describe itself.
pub fn adts_wrap(frame: &[u8], sample_rate: u32, channels: usize) -> Vec<u8> {
    let index = adts_sample_rate_index(sample_rate);
    let channel_config = channels.clamp(1, 7) as u8;
    let length = frame.len() + 7;

    let mut out = Vec::with_capacity(length);
    out.push(0xff);
    // MPEG-4, layer 0, no CRC — so the header is seven bytes, not nine.
    out.push(0xf1);
    // Profile 1 (AAC-LC, stored as object type minus one), rate index, channels.
    out.push((1 << 6) | (index << 2) | ((channel_config >> 2) & 0x01));
    out.push(((channel_config & 0x03) << 6) | ((length >> 11) & 0x03) as u8);
    out.push(((length >> 3) & 0xff) as u8);
    out.push((((length & 0x07) << 5) | 0x1f) as u8);
    out.push(0xfc);
    out.extend_from_slice(frame);
    out
}

/// The sampling frequency index ADTS uses. Falls back to 48 kHz, which is
/// what everything upstream runs at.
fn adts_sample_rate_index(rate: u32) -> u8 {
    const RATES: [u32; 13] = [
        96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025,
        8_000, 7_350,
    ];
    RATES.iter().position(|&r| r == rate).unwrap_or(3) as u8
}

/// Turns Annex-B access units into a transport stream, with the parameter sets
/// repeated before every keyframe.
///
/// A player joining a live stream cannot decode until it has seen the
/// parameter sets, so sending them once at the start is not enough.
pub fn prepare_video(units: &[&[u8]], sets: &h264::ParameterSets) -> Vec<u8> {
    let keyframe = h264::is_keyframe(units);
    let mut out = Vec::new();

    if keyframe {
        if let Some(sps) = &sets.sps {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(sps);
        }
        if let Some(pps) = &sets.pps {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(pps);
        }
    }
    for unit in units {
        // The parameter sets have just been written; writing them twice
        // confuses nothing but wastes bitrate on every keyframe.
        let kind = unit.first().map(|b| h264::nal_type(*b)).unwrap_or(0);
        if keyframe && (kind == 7 || kind == 8) {
            continue;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(unit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packets(stream: &[u8]) -> Vec<&[u8]> {
        stream.chunks_exact(PACKET_SIZE).collect()
    }

    fn pid_of(packet: &[u8]) -> u16 {
        (((packet[1] & 0x1f) as u16) << 8) | packet[2] as u16
    }

    #[test]
    fn every_packet_is_exactly_one_hundred_and_eighty_eight_bytes() {
        // A short packet desynchronises everything after it, because a
        // demuxer finds the next one by counting from the last sync byte.
        let mut muxer = TsMuxer::new(true);
        let mut out = Vec::new();
        muxer.video(&vec![0x65; 5000], 0, 0, true, &mut out);
        muxer.audio(&adts_wrap(&[0x21; 200], 48_000, 2), 0, &mut out);

        assert!(!out.is_empty());
        assert_eq!(out.len() % PACKET_SIZE, 0, "the stream is not a whole number of packets");
    }

    #[test]
    fn every_packet_starts_with_the_sync_byte() {
        let mut muxer = TsMuxer::new(true);
        let mut out = Vec::new();
        for n in 0..10 {
            muxer.video(&vec![0x41; 900], n * 3000, n * 3000, n == 0, &mut out);
        }
        for (index, packet) in packets(&out).iter().enumerate() {
            assert_eq!(packet[0], SYNC_BYTE, "packet {index} lost sync");
        }
    }

    #[test]
    fn the_tables_come_before_any_frame() {
        // A player joining the stream cannot decode a frame it has no program
        // map for, so the very first packets have to be the tables.
        let mut muxer = TsMuxer::new(true);
        let mut out = Vec::new();
        muxer.video(&vec![0x65; 100], 0, 0, true, &mut out);

        let list = packets(&out);
        assert_eq!(pid_of(list[0]), PAT_PID);
        assert_eq!(pid_of(list[1]), PMT_PID);
        assert_eq!(pid_of(list[2]), VIDEO_PID);
    }

    #[test]
    fn the_tables_are_repeated_so_a_late_joiner_can_start() {
        let mut muxer = TsMuxer::new(true);
        let mut out = Vec::new();
        for n in 0..60 {
            muxer.video(&vec![0x41; 1200], n * 3000, n * 3000, n == 0, &mut out);
        }
        let pats = packets(&out).iter().filter(|p| pid_of(p) == PAT_PID).count();
        assert!(pats > 1, "the program tables were sent only once");
    }

    #[test]
    fn continuity_counters_advance_by_one_and_wrap() {
        let mut muxer = TsMuxer::new(false);
        let mut out = Vec::new();
        // Enough payload to need many packets on the video PID.
        for n in 0..20 {
            muxer.video(&vec![0x41; 4000], n * 3000, n * 3000, n == 0, &mut out);
        }

        let video: Vec<u8> = packets(&out)
            .iter()
            .filter(|p| pid_of(p) == VIDEO_PID)
            .map(|p| p[3] & 0x0f)
            .collect();

        assert!(video.len() > 20, "expected plenty of video packets");
        for pair in video.windows(2) {
            assert_eq!(
                pair[1],
                (pair[0] + 1) & 0x0f,
                "continuity jumped from {} to {} — a demuxer reports that as loss",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn only_the_first_packet_of_a_frame_says_the_payload_starts() {
        let mut muxer = TsMuxer::new(false);
        let mut out = Vec::new();
        muxer.video(&vec![0x41; 3000], 0, 0, true, &mut out);

        let starts: Vec<bool> = packets(&out)
            .iter()
            .filter(|p| pid_of(p) == VIDEO_PID)
            .map(|p| p[1] & 0x40 != 0)
            .collect();

        assert!(starts.len() > 1, "expected the frame to span several packets");
        assert!(starts[0], "the first packet must mark the start");
        assert!(
            starts[1..].iter().all(|&s| !s),
            "a continuation packet claimed to start a new frame"
        );
    }

    #[test]
    fn a_keyframe_is_flagged_for_random_access() {
        let mut muxer = TsMuxer::new(false);
        let mut key = Vec::new();
        muxer.video(&vec![0x65; 400], 0, 0, true, &mut key);

        let packet = packets(&key).into_iter().find(|p| pid_of(p) == VIDEO_PID).unwrap();
        // Adaptation field present, and its first flag byte has bit 6 set.
        assert_ne!(packet[3] & 0x20, 0, "no adaptation field on a keyframe");
        assert_ne!(packet[5] & 0x40, 0, "random access was not flagged");
    }

    #[test]
    fn the_section_crc_matches_the_published_test_vector() {
        // CRC-32/MPEG-2 of "123456789" is 0x0376E6E7. A reflected variant —
        // the easy mistake — gives a different answer, and every demuxer
        // silently drops tables whose CRC does not match.
        assert_eq!(mpeg_crc32(b"123456789"), 0x0376_E6E7);
    }

    #[test]
    fn the_tables_carry_a_crc_that_checks_out() {
        // A section including its own CRC checksums to zero.
        for section in [pat_section(), pmt_section(true)] {
            assert_eq!(
                mpeg_crc32(&section),
                0,
                "a table would be discarded by every demuxer"
            );
        }
    }

    #[test]
    fn the_program_map_lists_audio_only_when_there_is_audio() {
        let with = pmt_section(true);
        let without = pmt_section(false);
        assert!(with.contains(&STREAM_TYPE_AAC_ADTS));
        assert!(!without.contains(&STREAM_TYPE_AAC_ADTS));
        assert!(with.contains(&STREAM_TYPE_H264) && without.contains(&STREAM_TYPE_H264));
    }

    #[test]
    fn a_timestamp_round_trips_through_its_marker_bits() {
        fn decode(bytes: [u8; 5]) -> u64 {
            (((bytes[0] as u64 >> 1) & 0x07) << 30)
                | ((bytes[1] as u64) << 22)
                | (((bytes[2] as u64) >> 1) << 15)
                | ((bytes[3] as u64) << 7)
                | ((bytes[4] as u64) >> 1)
        }
        for value in [0u64, 1, 90_000, 1_000_000, 0x1_ffff_ffff] {
            assert_eq!(decode(encode_timestamp(0x2, value)), value, "{value} did not survive");
        }
    }

    #[test]
    fn a_timestamp_marker_says_whether_a_dts_follows() {
        // Getting this nibble wrong makes a player read the DTS as part of
        // the payload, which looks like corruption rather than a timing bug.
        let with_dts = encode_timestamp(0x3, 90_000);
        let alone = encode_timestamp(0x2, 90_000);
        assert_eq!(with_dts[0] >> 4, 0x3);
        assert_eq!(alone[0] >> 4, 0x2);
    }

    #[test]
    fn the_clock_reference_round_trips() {
        fn decode(b: [u8; 6]) -> u64 {
            ((b[0] as u64) << 25)
                | ((b[1] as u64) << 17)
                | ((b[2] as u64) << 9)
                | ((b[3] as u64) << 1)
                | ((b[4] as u64) >> 7)
        }
        for value in [0u64, 90_000, 1_234_567] {
            assert_eq!(decode(encode_pcr(value)), value);
        }
    }

    #[test]
    fn an_adts_header_states_the_right_length_and_rate() {
        let frame = vec![0x21u8; 300];
        let wrapped = adts_wrap(&frame, 48_000, 2);

        assert_eq!(wrapped.len(), frame.len() + 7);
        assert_eq!(wrapped[0], 0xff);
        assert_eq!(wrapped[1] & 0xf0, 0xf0, "sync word is not intact");

        // The length is spread over three bytes with bits either side of it.
        let declared = (((wrapped[3] & 0x03) as usize) << 11)
            | ((wrapped[4] as usize) << 3)
            | ((wrapped[5] as usize) >> 5);
        assert_eq!(declared, wrapped.len(), "a decoder would read the wrong frame length");

        // 48 kHz is index 3, and stereo is channel configuration 2.
        let rate_index = (wrapped[2] >> 2) & 0x0f;
        assert_eq!(rate_index, 3);
        let channels = ((wrapped[2] & 0x01) << 2) | (wrapped[3] >> 6);
        assert_eq!(channels, 2);
    }

    #[test]
    fn adts_handles_mono_and_other_rates() {
        let mono = adts_wrap(&[0x21; 100], 44_100, 1);
        assert_eq!((mono[2] >> 2) & 0x0f, 4, "44.1 kHz is index 4");
        assert_eq!(((mono[2] & 0x01) << 2) | (mono[3] >> 6), 1);

        // An unknown rate falls back rather than producing an invalid index.
        let odd = adts_wrap(&[0x21; 100], 37_000, 2);
        assert_eq!((odd[2] >> 2) & 0x0f, 3);
    }

    #[test]
    fn parameter_sets_are_repeated_before_every_keyframe() {
        // Without this a player that joins after the first keyframe has a
        // stream it can never decode.
        let sps: Vec<u8> = vec![0x67, 0x42, 0x00, 0x1e];
        let pps: Vec<u8> = vec![0x68, 0xce, 0x38, 0x80];
        let sets = h264::ParameterSets { sps: Some(sps.clone()), pps: Some(pps.clone()) };

        let idr: Vec<u8> = vec![0x65, 0x88, 0x84];
        let out = prepare_video(&[&idr], &sets);

        assert!(
            out.windows(sps.len()).any(|w| w == sps.as_slice()),
            "the keyframe went out without its SPS"
        );
        assert!(out.windows(pps.len()).any(|w| w == pps.as_slice()));

        // A non-keyframe does not repeat them; that would be pure waste.
        let inter: Vec<u8> = vec![0x41, 0x9a, 0x00];
        let plain = prepare_video(&[&inter], &sets);
        assert!(!plain.windows(sps.len()).any(|w| w == sps.as_slice()));
    }

    #[test]
    fn parameter_sets_are_not_written_twice_when_the_frame_already_has_them() {
        let sps: Vec<u8> = vec![0x67, 0x42, 0x00, 0x1e];
        let pps: Vec<u8> = vec![0x68, 0xce, 0x38, 0x80];
        let sets = h264::ParameterSets { sps: Some(sps.clone()), pps: Some(pps.clone()) };
        let idr: Vec<u8> = vec![0x65, 0x88];

        let out = prepare_video(&[&sps, &pps, &idr], &sets);
        let occurrences = out.windows(sps.len()).filter(|w| *w == sps.as_slice()).count();
        assert_eq!(occurrences, 1, "the SPS was written {occurrences} times");
    }

    #[test]
    fn a_frame_that_exactly_fills_a_packet_does_not_produce_an_empty_one() {
        // The boundary case: a payload that lands exactly on the packet size
        // must not be followed by a packet carrying nothing.
        let mut muxer = TsMuxer::new(false);
        let mut out = Vec::new();
        muxer.write_tables(&mut out);
        let before = out.len();

        // The budget, spelled out. A video packet's first cell carries four
        // bytes of header, the adaptation field length byte, and the field
        // itself — flags plus a six-byte clock reference.
        const CELL: usize = PACKET_SIZE - 4 - 1 - 1 - 6;
        // The PES header: start code and stream id, the length field, three
        // flag bytes, and ten bytes of PTS and DTS.
        const PES_OVERHEAD: usize = 4 + 2 + 3 + 10;
        // Each frame is prefixed with a six-byte access unit delimiter.
        const DELIMITER: usize = 6;

        muxer.video(&vec![0x41; CELL - PES_OVERHEAD - DELIMITER], 0, 0, false, &mut out);
        let produced = (out.len() - before) / PACKET_SIZE;
        assert_eq!(produced, 1, "expected one packet, got {produced}");
    }

    #[test]
    fn audio_states_its_length_rather_than_leaving_it_open() {
        // Zero length is legal for video only. An audio PES with zero length
        // is read as running to the end of the stream.
        let pes = pes_packet(0xc0, &[0x21; 300], Some(0), None, true);
        let declared = u16::from_be_bytes([pes[4], pes[5]]);
        assert_ne!(declared, 0);
        assert_eq!(declared as usize, pes.len() - 6);
    }

    #[test]
    fn a_large_video_frame_declares_zero_length_rather_than_wrapping() {
        // The field is sixteen bits and a 1080p keyframe is bigger than that.
        // Wrapping would tell the demuxer a huge frame is a tiny one.
        let pes = pes_packet(0xe0, &vec![0x41; 80_000], Some(0), Some(0), false);
        assert_eq!(u16::from_be_bytes([pes[4], pes[5]]), 0);
    }
}
