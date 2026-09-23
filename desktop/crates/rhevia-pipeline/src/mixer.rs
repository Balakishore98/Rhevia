//! The full path: decode every input, composite them, encode the result.
//!
//! This is the difference between Rhevia and a relay. Passthrough copies one
//! camera to one destination; this takes many sources, puts them on one canvas,
//! and produces a single new stream — which is what a switcher is.

use rhevia_engine::{
    transition, CodecError, Compositor, EncoderSettings, Frame, H264Decoder, H264Encoder, Scene,
    Transition,
};

/// How many inputs a mixer holds. Not a licence limit — a preallocation, and
/// `add_input` grows it on demand.
const INITIAL_INPUTS: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum MixError {
    #[error("codec: {0}")]
    Codec(#[from] CodecError),
    #[error("no input at index {0}")]
    NoSuchInput(usize),
}

/// Statistics the operator's UI shows.
#[derive(Debug, Default, Clone, Copy)]
pub struct MixStats {
    pub frames_rendered: u64,
    pub frames_encoded: u64,
    pub bytes_encoded: u64,
    /// Inputs currently holding a decoded picture.
    pub live_inputs: usize,
}

/// One decoded source.
struct Input {
    decoder: H264Decoder,
    /// Most recent decoded picture, held until replaced.
    ///
    /// Held rather than consumed because the compositor runs on its own clock:
    /// a source at 25 fps composited into a 30 fps program must show its last
    /// frame again rather than flickering to black.
    current: Option<Frame>,
    name: String,
}

pub struct Mixer {
    inputs: Vec<Input>,
    compositor: Compositor,
    encoder: H264Encoder,
    /// The programme scaled to whatever the stream is leaving at.
    ///
    /// The production and the stream are different sizes on purpose: a hall
    /// can run 1080p for the projector and the recording while sending 720p
    /// to the internet. When the two match this is the same picture and
    /// scaling into it is a copy.
    for_encoder: Frame,
    stats: MixStats,
    /// Scratch for the outgoing and incoming pictures during a transition, and
    /// the blended result. Kept here so a transition allocates nothing per
    /// frame -- it runs at the worst possible moment to start churning memory.
    outgoing: Frame,
    incoming: Frame,
    blended: Frame,
    /// True while `blended` holds the newest picture rather than the
    /// compositor canvas.
    blended_is_current: bool,
}

impl Mixer {
    pub fn new(settings: EncoderSettings) -> Result<Self, MixError> {
        let mut inputs = Vec::with_capacity(INITIAL_INPUTS);
        for i in 0..INITIAL_INPUTS {
            inputs.push(Input {
                decoder: H264Decoder::new()?,
                current: None,
                name: format!("Input {}", i + 1),
            });
        }

        Ok(Self {
            inputs,
            compositor: Compositor::new(settings.width, settings.height),
            encoder: H264Encoder::new(settings)?,
            for_encoder: Frame::new(settings.width, settings.height),
            stats: MixStats::default(),
            outgoing: Frame::new(settings.width, settings.height),
            incoming: Frame::new(settings.width, settings.height),
            blended: Frame::new(settings.width, settings.height),
            blended_is_current: false,
        })
    }

    /// Rebuilds the encoder at a new size or bitrate.
    ///
    /// Everything already sent stays as it was; what follows is encoded the
    /// new way. A viewer joining mid-show gets the new settings, and one
    /// already watching sees a new keyframe.
    pub fn set_encoder(&mut self, settings: EncoderSettings) -> Result<(), MixError> {
        self.encoder = H264Encoder::new(settings)?;
        self.for_encoder = Frame::new(settings.width, settings.height);
        Ok(())
    }

    /// What size the stream is leaving at.
    pub fn encoder_size(&self) -> (usize, usize) {
        (self.for_encoder.width, self.for_encoder.height)
    }

    pub fn input_count(&self) -> usize {
        self.inputs.len()
    }

    pub fn input_name(&self, index: usize) -> Option<&str> {
        self.inputs.get(index).map(|i| i.name.as_str())
    }

    pub fn set_input_name(&mut self, index: usize, name: impl Into<String>) {
        if let Some(input) = self.inputs.get_mut(index) {
            input.name = name.into();
        }
    }

    /// Adds an input and returns its index.
    pub fn add_input(&mut self, name: impl Into<String>) -> Result<usize, MixError> {
        self.inputs.push(Input {
            decoder: H264Decoder::new()?,
            current: None,
            name: name.into(),
        });
        Ok(self.inputs.len() - 1)
    }

    /// Removes an input, freeing its decoder and its held frame.
    ///
    /// Every index above it shifts down by one, so callers holding input
    /// indices must remap. Leaving removed inputs in place instead would keep
    /// a decoder and a full frame alive for every source ever opened.
    pub fn remove_input(&mut self, index: usize) -> bool {
        if index >= self.inputs.len() {
            return false;
        }
        self.inputs.remove(index);
        true
    }

    /// Feeds one encoded access unit to an input.
    ///
    /// Decode failures are reported but do not poison the input: a corrupt
    /// packet from a flaky link should cost one frame, not the whole source.
    pub fn push_encoded(&mut self, index: usize, annexb: &[u8]) -> Result<bool, MixError> {
        let input = self.inputs.get_mut(index).ok_or(MixError::NoSuchInput(index))?;
        match input.decoder.decode(annexb) {
            Ok(Some(frame)) => {
                input.current = Some(frame);
                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(e) => {
                tracing::debug!(input = index, error = %e, "dropping an undecodable frame");
                Ok(false)
            }
        }
    }

    /// Supplies a picture directly, for sources that are not H.264 — a colour
    /// generator, a title, a screen capture.
    pub fn push_frame(&mut self, index: usize, frame: Frame) -> Result<(), MixError> {
        let input = self.inputs.get_mut(index).ok_or(MixError::NoSuchInput(index))?;
        input.current = Some(frame);
        Ok(())
    }

    /// The most recent picture from an input, for thumbnails and preview.
    pub fn input_frame(&self, index: usize) -> Option<&Frame> {
        self.inputs.get(index).and_then(|i| i.current.as_ref())
    }

    /// Composites `scene` and returns the program picture.
    pub fn render(&mut self, scene: &Scene) -> &Frame {
        let frames: Vec<Option<&Frame>> = self.inputs.iter().map(|i| i.current.as_ref()).collect();
        self.stats.frames_rendered += 1;
        self.stats.live_inputs = frames.iter().filter(|f| f.is_some()).count();
        self.blended_is_current = false;
        self.compositor.render(scene, &frames)
    }

    /// Composites and encodes in one step. Returns Annex-B, which may be empty
    /// for frames the encoder chooses not to emit.
    pub fn render_and_encode(&mut self, scene: &Scene) -> Result<Vec<u8>, MixError> {
        // Rendered into the compositor's own canvas, then cloned for the
        // encoder because the borrow checker cannot see that the two do not
        // overlap. One frame copy per output; the GPU path removes it.
        let program = self.render(scene).clone();
        program.scale_into(&mut self.for_encoder);
        let bitstream = self.encoder.encode(&self.for_encoder)?;
        if !bitstream.is_empty() {
            self.stats.frames_encoded += 1;
            self.stats.bytes_encoded += bitstream.len() as u64;
        }
        Ok(bitstream)
    }

    /// Composites two scenes and blends them with `kind` at `progress`.
    ///
    /// Both sides are composited in full before blending, so a transition
    /// works between any two arrangements -- a quad layout dissolving into a
    /// picture-in-picture, not merely one source fading into another.
    pub fn render_transition(
        &mut self,
        from: &Scene,
        to: &Scene,
        kind: Transition,
        progress: f32,
        stinger_input: Option<usize>,
    ) -> &Frame {
        self.stats.frames_rendered += 1;

        {
            let frames: Vec<Option<&Frame>> =
                self.inputs.iter().map(|i| i.current.as_ref()).collect();
            self.stats.live_inputs = frames.iter().filter(|f| f.is_some()).count();
            let rendered = self.compositor.render(from, &frames);
            self.outgoing.resize(rendered.width, rendered.height);
            self.outgoing.data.copy_from_slice(&rendered.data);
        }
        {
            let frames: Vec<Option<&Frame>> =
                self.inputs.iter().map(|i| i.current.as_ref()).collect();
            let rendered = self.compositor.render(to, &frames);
            self.incoming.resize(rendered.width, rendered.height);
            self.incoming.data.copy_from_slice(&rendered.data);
        }

        // The covering picture for a stinger. Cloned because the borrow
        // checker cannot see that the source input and the scratch buffers do
        // not overlap; only stingers pay for it.
        let via = stinger_input
            .and_then(|i| self.inputs.get(i))
            .and_then(|i| i.current.clone());

        transition::render(
            kind,
            &self.outgoing,
            &self.incoming,
            via.as_ref(),
            progress,
            &mut self.blended,
        );
        self.blended_is_current = true;
        &self.blended
    }

    /// Composites a transition and encodes the result.
    pub fn render_transition_and_encode(
        &mut self,
        from: &Scene,
        to: &Scene,
        kind: Transition,
        progress: f32,
        stinger_input: Option<usize>,
    ) -> Result<Vec<u8>, MixError> {
        let program = self
            .render_transition(from, to, kind, progress, stinger_input)
            .clone();
        program.scale_into(&mut self.for_encoder);
        let bitstream = self.encoder.encode(&self.for_encoder)?;
        if !bitstream.is_empty() {
            self.stats.frames_encoded += 1;
            self.stats.bytes_encoded += bitstream.len() as u64;
        }
        Ok(bitstream)
    }

    /// The last composited picture, for the Program monitor.
    ///
    /// During a transition this is the blended result rather than either side
    /// of it, so the monitor shows exactly what is going to air.
    pub fn program(&self) -> &Frame {
        if self.blended_is_current {
            &self.blended
        } else {
            self.compositor.output()
        }
    }

    /// Forces the next encoded frame to be a keyframe.
    pub fn request_keyframe(&mut self) {
        self.encoder.request_keyframe();
    }

    pub fn stats(&self) -> MixStats {
        self.stats
    }

    pub fn output_size(&self) -> (usize, usize) {
        self.compositor.size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhevia_engine::{Layer, Rect};

    fn settings() -> EncoderSettings {
        EncoderSettings::new(320, 240)
    }

    #[test]
    fn mixes_two_sources_onto_one_canvas_and_encodes_the_result() {
        let mut mixer = Mixer::new(settings()).expect("mixer");
        mixer
            .push_frame(0, Frame::filled(320, 240, [200, 0, 0]))
            .unwrap();
        mixer
            .push_frame(1, Frame::filled(160, 120, [0, 0, 200]))
            .unwrap();

        // Red full-screen, blue inset over the top right.
        let mut scene = Scene::single(0, 320, 240);
        scene.push(Layer {
            preserve_aspect: false,
            ..Layer::new(1, Rect::new(160.0, 0.0, 160.0, 120.0))
        });

        let program = mixer.render(&scene);
        assert_eq!(program.pixel(20, 200).unwrap()[0], 200, "background is red");
        assert_eq!(program.pixel(240, 60).unwrap()[2], 200, "inset is blue");

        let bitstream = mixer.render_and_encode(&scene).expect("encode");
        assert!(!bitstream.is_empty(), "the first frame must produce output");
        assert_eq!(mixer.stats().live_inputs, 2);
    }

    #[test]
    fn an_encoded_input_decodes_and_reaches_the_program() {
        // The full path: encode a source, feed it in as bytes, decode,
        // composite, and read the result back.
        let mut source = H264Encoder::new(EncoderSettings::new(320, 240)).expect("source encoder");
        let green = Frame::filled(320, 240, [0, 200, 0]);

        let mut mixer = Mixer::new(settings()).expect("mixer");
        let mut arrived = false;
        for _ in 0..5 {
            let bitstream = source.encode(&green).expect("encode");
            if bitstream.is_empty() {
                continue;
            }
            if mixer.push_encoded(0, &bitstream).expect("push") {
                arrived = true;
                break;
            }
        }
        assert!(arrived, "the input should decode within a few frames");

        let scene = Scene::single(0, 320, 240);
        let program = mixer.render(&scene);
        let px = program.pixel(160, 120).unwrap();
        assert!(
            px[1] > 150 && px[0] < 100,
            "the decoded source should reach the program green, got {px:?}"
        );
    }

    #[test]
    fn an_input_holds_its_last_frame_rather_than_flickering_to_black() {
        // A 25 fps source composited into a 30 fps program has no new picture
        // on some ticks. It must keep showing the previous one.
        let mut mixer = Mixer::new(settings()).expect("mixer");
        mixer
            .push_frame(0, Frame::filled(320, 240, [0, 0, 200]))
            .unwrap();
        let scene = Scene::single(0, 320, 240);

        for _ in 0..3 {
            let program = mixer.render(&scene);
            assert_eq!(
                program.pixel(160, 120).unwrap()[2],
                200,
                "the held frame should persist across renders"
            );
        }
    }

    #[test]
    fn a_corrupt_packet_costs_one_frame_not_the_input() {
        let mut mixer = Mixer::new(settings()).expect("mixer");
        assert!(!mixer.push_encoded(0, &[0xFF; 40]).expect("must not error"));

        // And the input still works afterwards.
        let mut source = H264Encoder::new(EncoderSettings::new(320, 240)).unwrap();
        let mut recovered = false;
        for _ in 0..5 {
            let bs = source.encode(&Frame::filled(320, 240, [10, 10, 220])).unwrap();
            if !bs.is_empty() && mixer.push_encoded(0, &bs).unwrap() {
                recovered = true;
                break;
            }
        }
        assert!(recovered, "the input should recover after bad data");
    }

    #[test]
    fn inputs_can_be_added_at_runtime() {
        let mut mixer = Mixer::new(settings()).expect("mixer");
        let before = mixer.input_count();
        let index = mixer.add_input("Phone").expect("add");
        assert_eq!(index, before);
        assert_eq!(mixer.input_name(index), Some("Phone"));
        assert!(mixer.push_frame(index, Frame::filled(64, 64, [1, 2, 3])).is_ok());
    }

    #[test]
    fn removing_an_input_frees_it_and_shifts_the_rest_down() {
        let mut mixer = Mixer::new(settings()).expect("mixer");
        mixer.set_input_name(0, "A");
        mixer.set_input_name(1, "B");
        mixer.set_input_name(2, "C");
        let before = mixer.input_count();

        assert!(mixer.remove_input(1));
        assert_eq!(mixer.input_count(), before - 1);
        assert_eq!(mixer.input_name(1), Some("C"), "C should have moved down");
    }

    #[test]
    fn removing_an_input_that_does_not_exist_is_refused_not_a_panic() {
        let mut mixer = Mixer::new(settings()).expect("mixer");
        assert!(!mixer.remove_input(999));
    }

    #[test]
    fn a_removed_input_no_longer_holds_its_picture() {
        // The whole point: the frame and decoder have to go with it.
        let mut mixer = Mixer::new(settings()).expect("mixer");
        mixer.push_frame(1, Frame::filled(64, 64, [1, 2, 3])).unwrap();
        assert!(mixer.input_frame(1).is_some());

        mixer.remove_input(1);
        // Index 1 is now the old index 2, which never had a picture.
        assert!(mixer.input_frame(1).is_none());
    }

    #[test]
    fn pushing_to_an_unknown_input_is_an_error_not_a_panic() {
        let mut mixer = Mixer::new(settings()).expect("mixer");
        assert!(matches!(
            mixer.push_frame(999, Frame::new(2, 2)),
            Err(MixError::NoSuchInput(999))
        ));
    }
}
