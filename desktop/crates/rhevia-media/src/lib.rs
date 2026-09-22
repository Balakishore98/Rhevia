//! Media file playback.
//!
//! A switcher has to play whatever someone drops on it — an MP4 from a phone,
//! a MOV from an editor, an MKV from a download, an MP3 of walk-in music.
//! Writing demuxers and decoders for all of that is years of work that has
//! already been done, so this drives ffmpeg as a decoding subprocess and reads
//! raw frames and samples back over a pipe.
//!
//! The consequence is honest and worth stating plainly: media playback needs
//! ffmpeg on the machine. Everything else in Rhevia — capture, mixing,
//! encoding, delivery — is native and needs nothing installed.
//!
//! Video and audio are decoded by two separate processes. One process cannot
//! write two raw streams to one pipe without a container to interleave them,
//! and demuxing that container again here would be the very work this avoids.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use rhevia_engine::Frame;

/// Sample rate everything downstream runs at.
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("ffmpeg is not installed, so media files cannot be played")]
    NoFfmpeg,
    #[error("{0} does not exist")]
    Missing(String),
    #[error("{0} has neither video nor audio that can be read")]
    Unreadable(String),
    #[error("could not start decoding {0}: {1}")]
    Start(String, String),
}

/// What a file turned out to contain.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaInfo {
    pub has_video: bool,
    pub has_audio: bool,
    pub width: u32,
    pub height: u32,
    /// Zero when the file has no duration ffprobe could determine, which is
    /// normal for a stream.
    pub duration_seconds: f64,
    pub video_codec: String,
    pub audio_codec: String,
}

impl MediaInfo {
    /// A one-line description for the input list.
    pub fn summary(&self) -> String {
        match (self.has_video, self.has_audio) {
            (true, true) => format!(
                "{}x{} {} · {}",
                self.width, self.height, self.video_codec, self.audio_codec
            ),
            (true, false) => format!("{}x{} {} · silent", self.width, self.height, self.video_codec),
            (false, true) => format!("audio only · {}", self.audio_codec),
            (false, false) => "empty".to_string(),
        }
    }
}

/// Whether ffmpeg has been looked for yet, and what was found.
///
/// 0 not yet asked, 1 present, 2 absent.
static FFMPEG: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// True when ffmpeg can be run.
///
/// The answer is remembered. Finding out costs a whole process, and this is
/// asked from interface code that runs on every repaint — sixty times a
/// second, which starts sixty processes a second until Windows refuses to
/// start any more and reports `0xc0000142` against a program that is
/// perfectly fine.
pub fn available() -> bool {
    use std::sync::atomic::Ordering;

    match FFMPEG.load(Ordering::Relaxed) {
        1 => return true,
        2 => return false,
        _ => {}
    }

    let found = quietly("ffmpeg")
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    FFMPEG.store(if found { 1 } else { 2 }, Ordering::Relaxed);
    found
}

/// Starts a helper without letting Windows open a console for it.
///
/// ffmpeg and ffprobe are console programs. Started from a windowed program
/// with no flag, Windows gives each one its own console, which flashes up
/// over the interface — during a show, in front of whatever is on screen.
/// Rhevia starts one of these for every file that is opened, every probe and
/// every check that ffmpeg is installed, so without this the window blinks
/// constantly. There is nothing to see in those consoles: their output is
/// already piped or discarded.
fn quietly(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Forgets whether ffmpeg was found, so the next call looks again.
///
/// For the case where someone installs it while Rhevia is running and presses
/// refresh rather than restarting.
pub fn forget_availability() {
    FFMPEG.store(0, std::sync::atomic::Ordering::Relaxed);
}

/// Asks ffprobe what is in a file.
pub fn probe(path: &str) -> Result<MediaInfo, MediaError> {
    if !std::path::Path::new(path).exists() {
        return Err(MediaError::Missing(path.to_string()));
    }

    let output = quietly("ffprobe")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-show_entries",
            "stream=codec_type,codec_name,width,height:format=duration",
            "-of",
            "default",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|_| MediaError::NoFfmpeg)?;

    let text = String::from_utf8_lossy(&output.stdout);
    Ok(parse_probe(&text))
}

/// Reads ffprobe's sectioned output into what the rest of this needs.
///
/// Parsed a whole `[STREAM]` block at a time rather than field by field.
/// ffprobe prints `codec_type` before `width`, so reading fields in order and
/// deciding at `codec_type` sees every stream as having no size — which makes
/// every video file look like audio.
///
/// Separated from running the process so the parsing, which is where the
/// mistakes are, can be tested without a file.
pub fn parse_probe(text: &str) -> MediaInfo {
    let mut info = MediaInfo::default();

    for block in sections(text, "STREAM") {
        let kind = field(&block, "codec_type").unwrap_or_default();
        let codec = field(&block, "codec_name").unwrap_or_default();
        let width: u32 = field(&block, "width").and_then(|v| v.parse().ok()).unwrap_or(0);
        let height: u32 = field(&block, "height").and_then(|v| v.parse().ok()).unwrap_or(0);

        match kind.as_str() {
            // An attached cover image is a video stream on paper. Treating an
            // MP3 with artwork as a video input would put a still picture on
            // air where sound was wanted, so plain artwork is not counted.
            "video" if width > 0 && height > 0 && !is_cover_art(&codec) => {
                info.has_video = true;
                info.width = width;
                info.height = height;
                info.video_codec = codec;
            }
            "audio" => {
                info.has_audio = true;
                info.audio_codec = codec;
            }
            _ => {}
        }
    }

    for block in sections(text, "FORMAT") {
        if let Some(duration) = field(&block, "duration") {
            info.duration_seconds = duration.parse().unwrap_or(0.0);
        }
    }

    info
}

/// The still-image codecs that appear as a video stream inside an audio file.
fn is_cover_art(codec: &str) -> bool {
    matches!(codec, "mjpeg" | "png" | "bmp" | "gif" | "webp")
}

/// Every `[NAME] … [/NAME]` block in ffprobe's output.
fn sections(text: &str, name: &str) -> Vec<String> {
    let open = format!("[{name}]");
    let close = format!("[/{name}]");
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;

    for line in text.lines() {
        let line = line.trim();
        if line == open {
            current = Some(String::new());
        } else if line == close {
            if let Some(block) = current.take() {
                blocks.push(block);
            }
        } else if let Some(block) = current.as_mut() {
            block.push_str(line);
            block.push('\n');
        }
    }
    blocks
}

/// One `key=value` from a block.
fn field(block: &str, key: &str) -> Option<String> {
    block.lines().find_map(|line| {
        line.split_once('=')
            .filter(|(k, _)| k.trim() == key)
            .map(|(_, v)| v.trim().to_string())
    })
}

/// Whether a path looks like something worth trying to open.
///
/// Used to decide what a dropped file is. Deliberately generous: ffmpeg reads
/// far more than this, and anything not listed is still attempted rather than
/// refused, so the list is a hint and not a gate.
pub fn looks_like_media(path: &str) -> bool {
    const EXTENSIONS: [&str; 24] = [
        "mp4", "mov", "mkv", "avi", "webm", "flv", "wmv", "mpg", "mpeg", "m4v", "ts", "m2ts",
        "3gp", "ogv", "mp3", "wav", "flac", "aac", "m4a", "ogg", "opus", "wma", "aiff", "h264",
    ];
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// A playing media file.
///
/// Video frames and audio samples are produced by background threads and
/// collected without waiting, the same as every other source: a decoder that
/// stalls costs a repeated frame, not the show.
/// How many decoded pictures are held waiting for the compositor.
///
/// The decoder and the compositor both run at thirty a second and neither
/// takes its clock from the other, so they drift in and out of phase. With
/// room for only one picture, every time two are produced between two ticks
/// one is thrown away and the next tick shows the previous one twice — which
/// measured at four ticks in a hundred on an idle machine and is exactly what
/// judder looks like. Three is enough to absorb that drift; it costs about a
/// tenth of a second of delay on a file, which is nothing, and the queue
/// filling up pushes back through the pipe so the decoder cannot run away.
const QUEUED_FRAMES: usize = 3;

pub struct MediaSource {
    pending: Arc<Mutex<VecDeque<Frame>>>,
    audio: Arc<Mutex<Vec<f32>>>,
    running: Arc<AtomicBool>,
    /// Counts what the audio thread has produced, so a caller can tell a
    /// silent file from a stalled one.
    audio_produced: Arc<AtomicU64>,
    /// Counts pictures, which is how far through the file we are.
    frames_produced: Arc<AtomicU64>,
    /// Set while the operator has it held.
    ///
    /// The reading threads stop taking from the pipe, ffmpeg fills it and
    /// blocks, and the clip stands still where it is. Nothing is thrown away
    /// and nothing has to be restarted to carry on.
    paused: Arc<AtomicBool>,
    children: Vec<Child>,
    /// Where the current decoders were started from, in seconds. A skip
    /// restarts them further in, and the position is that plus what has been
    /// decoded since.
    offset_seconds: f32,
    /// What the decoders were opened at, kept so a skip can reopen them the
    /// same way.
    size: (u32, u32),
    fps: f32,
    pub path: String,
    pub info: MediaInfo,
}

impl MediaSource {
    /// Opens `path` and starts playing it on a loop.
    ///
    /// `width` and `height` are what the frames are scaled to, which is the
    /// programme size: scaling in the decoder is far cheaper than scaling
    /// every frame in the compositor afterwards.
    pub fn open(path: &str, width: u32, height: u32, fps: f32) -> Result<Self, MediaError> {
        if !available() {
            return Err(MediaError::NoFfmpeg);
        }
        let info = probe(path)?;
        if !info.has_video && !info.has_audio {
            return Err(MediaError::Unreadable(path.to_string()));
        }

        let pending: Arc<Mutex<VecDeque<Frame>>> = Arc::new(Mutex::new(VecDeque::new()));
        let audio: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let running = Arc::new(AtomicBool::new(true));
        let audio_produced = Arc::new(AtomicU64::new(0));
        let frames_produced = Arc::new(AtomicU64::new(0));
        let paused = Arc::new(AtomicBool::new(false));

        let mut source = Self {
            pending,
            audio,
            running,
            audio_produced,
            frames_produced,
            paused,
            children: Vec::new(),
            offset_seconds: 0.0,
            size: (width, height),
            fps,
            path: path.to_string(),
            info,
        };
        source.start_decoders(0.0)?;
        Ok(source)
    }

    /// Starts the decoders at `from` seconds into the file.
    ///
    /// Seeking means restarting them: ffmpeg is being driven as a pipe, and a
    /// pipe cannot be told to go back. That costs the moment it takes to open
    /// the file again, which is why skipping is offered in fixed steps rather
    /// than as a scrub.
    fn start_decoders(&mut self, from: f32) -> Result<(), MediaError> {
        let (width, height) = self.size;
        let from = from.max(0.0);
        let mut children = Vec::new();

        if self.info.has_video {
            children.push(spawn_video(
                &self.path,
                width,
                height,
                self.fps,
                from,
                Arc::clone(&self.pending),
                Arc::clone(&self.running),
                Arc::clone(&self.paused),
                Arc::clone(&self.frames_produced),
            )?);
        }
        if self.info.has_audio {
            children.push(spawn_audio(
                &self.path,
                from,
                Arc::clone(&self.audio),
                Arc::clone(&self.running),
                Arc::clone(&self.paused),
                Arc::clone(&self.audio_produced),
            )?);
        }

        self.children = children;
        self.offset_seconds = from;
        Ok(())
    }

    /// Stops the current decoders and the threads reading them.
    ///
    /// The threads watch `running`, so it is lowered to let them finish and
    /// raised again for the ones that follow. Without that the old threads
    /// would keep writing frames from the old position into the same slot as
    /// the new ones.
    fn stop_decoders(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        // Unpaused first, or a thread parked in the pause loop never notices.
        self.paused.store(false, Ordering::Relaxed);
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.children.clear();
        // Long enough for a reader blocked on a now-dead pipe to come back
        // and see that it should stop.
        std::thread::sleep(std::time::Duration::from_millis(30));
        self.running.store(true, Ordering::Relaxed);

        if let Ok(mut buffer) = self.audio.lock() {
            buffer.clear();
        }
        // Pictures from before the skip must not be shown after it.
        if let Ok(mut queue) = self.pending.lock() {
            queue.clear();
        }
        self.audio_produced.store(0, Ordering::Relaxed);
        self.frames_produced.store(0, Ordering::Relaxed);
    }

    /// Whether the clip is standing still.
    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// Holds the clip where it is, or lets it carry on.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
        if paused {
            // Whatever was already decoded ahead is dropped, so letting go
            // does not start with a burst of sound from before the pause.
            // The pictures stay: the last one drawn is what should remain on
            // screen, and the queued ones are the moments right after it.
            if let Ok(mut buffer) = self.audio.lock() {
                buffer.clear();
            }
        }
    }

    /// How far into the file the clip has played, in seconds.
    ///
    /// Counted from pictures where there are pictures and from samples where
    /// there are not, because an audio-only file has no frames to count.
    pub fn position_seconds(&self) -> f32 {
        let since = if self.info.has_video && self.fps > 0.0 {
            self.frames_produced.load(Ordering::Relaxed) as f32 / self.fps
        } else {
            self.audio_produced.load(Ordering::Relaxed) as f32 / SAMPLE_RATE as f32
        };
        let position = self.offset_seconds + since;
        // The file loops, so the position does too rather than counting past
        // the end for as long as the clip is left running.
        match self.duration_seconds() {
            Some(d) => position % d,
            None => position,
        }
    }

    /// How long the file is, when ffprobe could say.
    ///
    /// A live stream has no duration, and neither does a file ffprobe could
    /// not measure; both come back as nothing rather than as zero, so a
    /// caller cannot accidentally divide by it.
    pub fn duration_seconds(&self) -> Option<f32> {
        match self.info.duration_seconds {
            d if d > 0.0 => Some(d as f32),
            _ => None,
        }
    }

    /// Jumps to a point in the file and carries on playing from there.
    ///
    /// Past the end wraps to the start and before the start clamps to it,
    /// which is what skipping back five seconds at two seconds in should do.
    pub fn seek(&mut self, to: f32) -> Result<(), MediaError> {
        let to = match self.duration_seconds() {
            Some(d) => to.rem_euclid(d),
            None => to.max(0.0),
        };
        let was_paused = self.paused();
        self.stop_decoders();
        self.start_decoders(to)?;
        self.paused.store(was_paused, Ordering::Relaxed);
        Ok(())
    }

    /// Moves by `delta` seconds from where the clip is now.
    pub fn skip(&mut self, delta: f32) -> Result<(), MediaError> {
        let to = self.position_seconds() + delta;
        self.seek(to)
    }

    /// Back to the beginning and held there, the way a stop button behaves on
    /// a player: the clip is cued, not unloaded.
    pub fn stop(&mut self) -> Result<(), MediaError> {
        self.stop_decoders();
        self.start_decoders(0.0)?;
        self.paused.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// The next picture to show, if one is waiting.
    ///
    /// Oldest first: a queue, not a mailbox. Taking the newest and dropping
    /// the rest would put the judder straight back.
    pub fn take_frame(&self) -> Option<Frame> {
        self.pending.lock().ok().and_then(|mut queue| queue.pop_front())
    }

    /// How many pictures are waiting.
    ///
    /// Sitting at zero means the decoder is not keeping up and the
    /// compositor is repeating pictures; sitting at the limit means it is
    /// ahead and being held back, which is the healthy state.
    pub fn queued(&self) -> usize {
        self.pending.lock().map(|q| q.len()).unwrap_or(0)
    }

    /// Takes up to `frames` of interleaved stereo audio.
    ///
    /// Short reads are padded with silence rather than returning less than
    /// asked for: the mixer works in fixed blocks, and a short block would
    /// shift everything after it.
    pub fn take_audio(&self, frames: usize) -> Vec<f32> {
        let wanted = frames * CHANNELS;
        let mut out = Vec::with_capacity(wanted);

        if let Ok(mut buffer) = self.audio.lock() {
            let take = wanted.min(buffer.len());
            out.extend(buffer.drain(..take));

            // A decoder that has run far ahead is trimmed back. Left alone it
            // would grow without limit on a long file and add latency that
            // never comes back.
            const MAX_BUFFERED: usize = SAMPLE_RATE as usize * CHANNELS; // one second
            if buffer.len() > MAX_BUFFERED {
                let excess = buffer.len() - MAX_BUFFERED;
                buffer.drain(..excess);
            }
        }
        out.resize(wanted, 0.0);
        out
    }

    /// Samples the audio thread has produced since opening.
    pub fn audio_produced(&self) -> u64 {
        self.audio_produced.load(Ordering::Relaxed)
    }

    /// Process ids of the decoders this source started.
    ///
    /// Exposed so a test can prove they are gone afterwards. ffmpeg is looping
    /// the file forever and will never exit by itself, so a source that fails
    /// to kill its decoders leaks a process for every clip ever opened.
    pub fn decoder_pids(&self) -> Vec<u32> {
        self.children.iter().map(|c| c.id()).collect()
    }
}

impl Drop for MediaSource {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        // Killed rather than waited on: ffmpeg is looping the file forever and
        // will never exit on its own.
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Decodes video to raw RGBA at the programme size, paced at real time.
#[allow(clippy::too_many_arguments)]
fn spawn_video(
    path: &str,
    width: u32,
    height: u32,
    fps: f32,
    from: f32,
    pending: Arc<Mutex<VecDeque<Frame>>>,
    running: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    produced: Arc<AtomicU64>,
) -> Result<Child, MediaError> {
    let mut child = quietly("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            // Loops the file, which is what a holding clip or a sting wants.
            "-stream_loop",
            "-1",
            // Paced at real time. Without this ffmpeg decodes as fast as it
            // can and the clip plays at several hundred frames a second.
            "-re",
            // Before -i, so ffmpeg seeks the file rather than decoding
            // everything up to the point and throwing it away. It lands on
            // the nearest key frame, which is close enough for a skip and an
            // order of magnitude faster than being exact.
            "-ss",
            &format!("{from:.3}"),
            "-i",
        ])
        .arg(path)
        .args([
            "-an",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            // Scaled here rather than in the compositor: the decoder does it
            // with optimised code, once, instead of per composite.
            //
            // Lanczos rather than ffmpeg's default bicubic. Any scaling at
            // all is where a source visibly loses its edges, and this is the
            // only place in Rhevia where a whole picture is resampled, so it
            // is the one place worth paying for. Accurate rounding and full
            // chroma interpolation stop the 4:2:0 a camera or a file arrives
            // in from smearing colour across the edges on the way to RGB.
            "-sws_flags",
            "lanczos+accurate_rnd+full_chroma_int",
            "-s",
            &format!("{width}x{height}"),
            "-r",
            &format!("{fps:.3}"),
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| MediaError::Start(path.to_string(), e.to_string()))?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| MediaError::Start(path.to_string(), "no output pipe".into()))?;

    let frame_bytes = width as usize * height as usize * 4;
    std::thread::Builder::new()
        .name("rhevia-media-video".into())
        .spawn(move || {
            let mut buffer = vec![0u8; frame_bytes];
            while running.load(Ordering::Relaxed) {
                // Paused by not reading. ffmpeg fills the pipe, blocks, and
                // the clip stands exactly where it was; the last picture
                // stays on screen because nothing replaces it.
                //
                // The same applies with the queue full: not reading is how
                // the decoder is told to wait, and it costs nothing because
                // ffmpeg is playing the file at real time anyway.
                let wait = paused.load(Ordering::Relaxed)
                    || pending.lock().map(|q| q.len() >= QUEUED_FRAMES).unwrap_or(false);
                if wait {
                    std::thread::sleep(std::time::Duration::from_millis(4));
                    continue;
                }
                // read_exact, not read: a pipe hands over whatever is ready,
                // and a partial frame drawn as a whole one tears diagonally.
                if stdout.read_exact(&mut buffer).is_err() {
                    break;
                }
                produced.fetch_add(1, Ordering::Relaxed);
                if let Ok(mut queue) = pending.lock() {
                    queue.push_back(Frame {
                        width: width as usize,
                        height: height as usize,
                        data: buffer.clone(),
                    });
                    // Belt and braces: the check above races with the
                    // compositor, and an unbounded queue on a long clip is
                    // memory that never comes back.
                    while queue.len() > QUEUED_FRAMES {
                        queue.pop_front();
                    }
                }
            }
        })
        .map_err(|e| MediaError::Start(path.to_string(), e.to_string()))?;

    Ok(child)
}

/// Decodes audio to raw 48 kHz stereo floats, paced at real time.
fn spawn_audio(
    path: &str,
    from: f32,
    audio: Arc<Mutex<Vec<f32>>>,
    running: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    produced: Arc<AtomicU64>,
) -> Result<Child, MediaError> {
    let mut child = quietly("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-stream_loop", "-1", "-re"])
        .args(["-ss", &format!("{from:.3}")])
        .arg("-i")
        .arg(path)
        .args([
            "-vn",
            "-f",
            "f32le",
            "-ar",
            &SAMPLE_RATE.to_string(),
            "-ac",
            &CHANNELS.to_string(),
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| MediaError::Start(path.to_string(), e.to_string()))?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| MediaError::Start(path.to_string(), "no output pipe".into()))?;

    std::thread::Builder::new()
        .name("rhevia-media-audio".into())
        .spawn(move || {
            // A tenth of a second at a time: small enough to stay responsive,
            // large enough not to lock the buffer constantly.
            const BLOCK: usize = (SAMPLE_RATE as usize / 10) * CHANNELS;
            let mut raw = vec![0u8; BLOCK * 4];

            while running.load(Ordering::Relaxed) {
                // Held by not reading, the same as the picture, so the two
                // stand still together and stay in step when let go.
                if paused.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    continue;
                }
                if stdout.read_exact(&mut raw).is_err() {
                    break;
                }
                let samples: Vec<f32> = raw
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect();
                // Frames, not samples: this is read as a position as well
                // as a sign of life, and a stereo frame is two samples.
                produced.fetch_add((samples.len() / CHANNELS) as u64, Ordering::Relaxed);

                if let Ok(mut buffer) = audio.lock() {
                    buffer.extend_from_slice(&samples);
                }
            }
        })
        .map_err(|e| MediaError::Start(path.to_string(), e.to_string()))?;

    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source with no decoders behind it, for checking the buffer handling
    /// on its own.
    ///
    /// Built here rather than spelled out at each call site so that adding a
    /// field to `MediaSource` does not break three tests that do not care
    /// about it.
    fn detached(buffered: Vec<f32>) -> MediaSource {
        MediaSource {
            pending: Arc::new(Mutex::new(VecDeque::new())),
            audio: Arc::new(Mutex::new(buffered)),
            running: Arc::new(AtomicBool::new(false)),
            audio_produced: Arc::new(AtomicU64::new(0)),
            frames_produced: Arc::new(AtomicU64::new(0)),
            paused: Arc::new(AtomicBool::new(false)),
            children: Vec::new(),
            offset_seconds: 0.0,
            size: (0, 0),
            fps: 30.0,
            path: String::new(),
            info: MediaInfo::default(),
        }
    }

    #[test]
    fn a_video_file_is_described_from_its_streams() {
        let text = "[STREAM]\ncodec_name=h264\ncodec_type=video\nwidth=1920\nheight=1080\n[/STREAM]\n\
                    [STREAM]\ncodec_name=aac\ncodec_type=audio\n[/STREAM]\n\
                    [FORMAT]\nduration=12.500000\n[/FORMAT]\n";
        let info = parse_probe(text);

        assert!(info.has_video && info.has_audio);
        assert_eq!(info.width, 1920);
        assert_eq!(info.height, 1080);
        assert_eq!(info.video_codec, "h264");
        assert_eq!(info.audio_codec, "aac");
        assert!((info.duration_seconds - 12.5).abs() < 0.001);
    }

    #[test]
    fn an_audio_only_file_is_not_mistaken_for_video() {
        let text = "[STREAM]\ncodec_name=mp3\ncodec_type=audio\n[/STREAM]\n\
                    [FORMAT]\nduration=185.0\n[/FORMAT]\n";
        let info = parse_probe(text);

        assert!(info.has_audio);
        assert!(!info.has_video, "an audio file must not become a video input");
        assert_eq!(info.summary(), "audio only · mp3");
    }

    #[test]
    fn cover_art_does_not_turn_a_song_into_a_video_input() {
        // An MP3 with artwork reports a video stream. Putting that on air as
        // a still picture, when the operator asked for music, is the bug this
        // guards against.
        let text = "[STREAM]\ncodec_name=mjpeg\ncodec_type=video\nwidth=600\nheight=600\n[/STREAM]\n\
                    [STREAM]\ncodec_name=mp3\ncodec_type=audio\n[/STREAM]\n\
                    [FORMAT]\nduration=200.0\n[/FORMAT]\n";
        let info = parse_probe(text);

        assert!(!info.has_video, "cover art was treated as video");
        assert!(info.has_audio);
    }

    #[test]
    fn a_silent_video_is_described_as_silent() {
        let text = "[STREAM]\ncodec_name=h264\ncodec_type=video\nwidth=1280\nheight=720\n[/STREAM]\n\
                    [FORMAT]\nduration=4.0\n[/FORMAT]\n";
        let info = parse_probe(text);

        assert!(info.has_video && !info.has_audio);
        assert_eq!(info.summary(), "1280x720 h264 · silent");
    }

    #[test]
    fn an_empty_probe_describes_nothing_rather_than_panicking() {
        let info = parse_probe("");
        assert!(!info.has_video && !info.has_audio);
        assert_eq!(info.summary(), "empty");
    }

    #[test]
    fn a_stream_with_no_duration_reads_as_zero() {
        // Live sources have no duration. That is not an error.
        let text = "[STREAM]\ncodec_name=h264\ncodec_type=video\nwidth=640\nheight=360\n[/STREAM]\n\
                    [FORMAT]\nduration=N/A\n[/FORMAT]\n";
        let info = parse_probe(text);
        assert_eq!(info.duration_seconds, 0.0);
        assert!(info.has_video);
    }

    #[test]
    fn several_streams_keep_their_own_codecs() {
        // A file with two audio tracks must not attribute one codec to the
        // other stream.
        let text = "[STREAM]\ncodec_name=hevc\ncodec_type=video\nwidth=3840\nheight=2160\n[/STREAM]\n\
                    [STREAM]\ncodec_name=ac3\ncodec_type=audio\n[/STREAM]\n\
                    [STREAM]\ncodec_name=aac\ncodec_type=audio\n[/STREAM]\n\
                    [FORMAT]\nduration=60.0\n[/FORMAT]\n";
        let info = parse_probe(text);
        assert_eq!(info.video_codec, "hevc");
        assert_eq!(info.width, 3840);
        // The last audio stream seen wins, which is what ffmpeg will pick.
        assert_eq!(info.audio_codec, "aac");
    }

    #[test]
    fn common_media_extensions_are_recognised() {
        for path in [
            r"C:\clips\opener.mp4",
            "/home/user/show.MKV",
            "walk-in.mp3",
            "sting.mov",
            "bed.flac",
        ] {
            assert!(looks_like_media(path), "{path} should look like media");
        }
    }

    #[test]
    fn things_that_are_not_media_are_not_recognised() {
        for path in ["notes.txt", "slide.png", "archive.zip", "noextension"] {
            assert!(!looks_like_media(path), "{path} should not look like media");
        }
    }

    #[test]
    fn asking_whether_ffmpeg_is_there_costs_one_process_not_one_per_call() {
        // Called from interface code that repaints sixty times a second. Left
        // uncached it starts sixty processes a second until Windows refuses,
        // which surfaces as 0xc0000142 against ffmpeg itself.
        forget_availability();
        let first = available();

        let started = std::time::Instant::now();
        for _ in 0..500 {
            assert_eq!(available(), first);
        }
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(50),
            "500 calls took {elapsed:?}, so they are still starting processes"
        );
    }

    #[test]
    fn forgetting_makes_the_next_call_look_again() {
        let first = available();
        forget_availability();
        // The answer is the same, but it was found afresh rather than read
        // from the cache.
        assert_eq!(available(), first);
    }

    #[test]
    fn opening_a_file_that_is_not_there_says_so() {
        if !available() {
            eprintln!("SKIP: ffmpeg not installed");
            return;
        }
        let err = MediaSource::open("no-such-file-12345.mp4", 1280, 720, 30.0);
        assert!(matches!(err, Err(MediaError::Missing(_))), "expected a missing-file error");
    }

    #[test]
    fn taking_audio_before_any_arrives_gives_silence_of_the_right_length() {
        // The mixer works in fixed blocks; a short one would shift everything
        // after it. This is the path taken for the first few ticks of every
        // clip, so it has to be right.
        let source = detached(Vec::new());

        let block = source.take_audio(512);
        assert_eq!(block.len(), 512 * CHANNELS);
        assert!(block.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn a_partly_filled_buffer_is_padded_rather_than_truncated() {
        let source = detached(vec![0.5; 100]);

        let block = source.take_audio(512);
        assert_eq!(block.len(), 512 * CHANNELS);
        assert_eq!(block[0], 0.5);
        assert_eq!(block[99], 0.5);
        assert_eq!(block[100], 0.0, "the rest should be silence");
    }

    #[test]
    fn a_decoder_running_ahead_is_trimmed_so_latency_does_not_grow() {
        // Left alone the buffer grows for the length of the file and every
        // sample of it is delay the operator cannot get rid of.
        let source = detached(vec![0.25; SAMPLE_RATE as usize * CHANNELS * 5]);

        source.take_audio(512);
        let left = source.audio.lock().unwrap().len();
        assert!(
            left <= SAMPLE_RATE as usize * CHANNELS,
            "the buffer kept {left} samples, which is more than a second of latency"
        );
    }
}
