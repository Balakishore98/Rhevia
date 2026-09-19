//! Ingest joined to delivery: a remote camera, streamed live.
//!
//! This is the passthrough path. Frames arriving from a camera are remuxed
//! straight to RTMP without being decoded or re-encoded, which means no GPU
//! work, no quality loss, and latency limited only by the network.
//!
//! When the compositor exists it slots into the middle of this without the
//! delivery side changing: decode here, composite, encode, and hand the
//! encoder's output to the same publisher.

pub mod mixer;

pub use mixer::{MixError, MixStats, Mixer};

use std::time::Duration;

use rhevia_link::CameraReceiver;
use rhevia_output::h264::{self, ParameterSets};
use rhevia_output::{flv, RtmpPublisher};

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("delivery failed: {0}")]
    Delivery(#[from] rhevia_output::RtmpError),
    #[error("the camera sent no video before the deadline")]
    NoVideo,
}

/// What a passthrough run produced, for the operator's stats panel.
#[derive(Debug, Default, Clone, Copy)]
pub struct PassthroughStats {
    pub frames_published: u64,
    pub keyframes: u64,
    pub bytes_published: u64,
    /// Frames dropped because the decoder configuration was not known yet.
    pub frames_before_config: u64,
}

/// Streams frames from `receiver` to `publisher` until the camera stops.
///
/// `idle_timeout` bounds how long to wait for the next frame. A live show needs
/// this: a camera that silently stops should end the run rather than hang, so
/// the caller can reconnect or switch away.
pub async fn passthrough(
    receiver: &CameraReceiver,
    publisher: &mut RtmpPublisher,
    idle_timeout: Duration,
) -> Result<PassthroughStats, PipelineError> {
    let mut stats = PassthroughStats::default();
    let mut sets = ParameterSets::default();
    let mut sent_config = false;
    let mut base_timestamp: Option<u32> = None;

    loop {
        let Ok(frame) = tokio::time::timeout(idle_timeout, receiver.next_video_frame()).await
        else {
            break; // camera went quiet
        };
        let Some(frame) = frame else { break }; // track ended

        let units = h264::split_annexb(&frame.annexb);
        if units.is_empty() {
            continue;
        }
        sets.absorb(&units);

        // The decoder configuration must reach the server before any frame.
        // Until the parameter sets arrive, frames are undecodable, so they are
        // counted and dropped rather than published as something unplayable.
        if !sent_config {
            match sets.to_avc_decoder_config() {
                Some(config) => {
                    publisher
                        .send_video(flv::avc_sequence_header(&config), 0, false)
                        .await?;
                    sent_config = true;
                }
                None => {
                    stats.frames_before_config += 1;
                    continue;
                }
            }
        }

        let avcc = h264::annexb_to_avcc(&units);
        if avcc.is_empty() {
            continue; // parameter sets only, no picture
        }

        // Timestamps are relative to the first frame so the stream starts at
        // zero regardless of where the camera's RTP clock happened to be.
        let base = *base_timestamp.get_or_insert(frame.rtp_timestamp);
        let timestamp_ms = frame.millis_since(base);

        let keyframe = frame.keyframe || h264::is_keyframe(&units);
        if keyframe {
            stats.keyframes += 1;
        }

        stats.bytes_published += avcc.len() as u64;
        stats.frames_published += 1;
        publisher
            .send_video(flv::avc_frame(&avcc, keyframe, 0), timestamp_ms, false)
            .await?;
    }

    if stats.frames_published == 0 {
        return Err(PipelineError::NoVideo);
    }
    Ok(stats)
}
