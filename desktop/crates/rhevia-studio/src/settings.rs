//! What the production is set up as, remembered between sessions.
//!
//! The resolution everything runs at is the one decision that cannot be
//! changed while a show is running: the encoder, the compositor and every
//! decoder are built around it. So it is chosen here, written down, and read
//! at startup — the same arrangement vMix uses, and for the same reason.

use std::path::PathBuf;

/// What the programme is composited, encoded and streamed at.
///
/// Everything an input produces is scaled to this on the way in, so a 1080p
/// file in a 720p production is a 720p file from that moment on. Choosing
/// below the source is throwing quality away before anything else happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Resolution {
    /// For an older machine, or a show that will only ever be watched small.
    Hd720,
    /// What almost every production wants, and what almost every camera and
    /// file already is.
    #[default]
    Hd1080,
    /// Worth it only with the processing to spare.
    Qhd1440,
    /// Software compositing at this size is roughly nine times the work of
    /// 720p. Offered because some productions genuinely need it.
    Uhd2160,
}

impl Resolution {
    pub const ALL: [Resolution; 4] =
        [Resolution::Hd720, Resolution::Hd1080, Resolution::Qhd1440, Resolution::Uhd2160];

    pub fn size(self) -> (usize, usize) {
        match self {
            Resolution::Hd720 => (1280, 720),
            Resolution::Hd1080 => (1920, 1080),
            Resolution::Qhd1440 => (2560, 1440),
            Resolution::Uhd2160 => (3840, 2160),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Resolution::Hd720 => "720p",
            Resolution::Hd1080 => "1080p",
            Resolution::Qhd1440 => "1440p",
            Resolution::Uhd2160 => "2160p",
        }
    }

    /// What a stream at this size needs to look like itself.
    ///
    /// A 1080p picture at a 720p bitrate looks worse than 720p did, because
    /// the encoder is given more detail to carry and no more room to carry
    /// it in — which is how raising the resolution can make things worse.
    pub fn bitrate(self) -> u32 {
        match self {
            Resolution::Hd720 => 4_500_000,
            Resolution::Hd1080 => 9_000_000,
            Resolution::Qhd1440 => 16_000_000,
            Resolution::Uhd2160 => 30_000_000,
        }
    }

    fn from_label(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.label() == text)
    }
}

/// How many pictures a second the production runs at.
///
/// Not fixed at thirty. Thirty is what broadcast in this part of the world
/// settled on and it is the right default, but a machine with cores to spare
/// can run a production at fifty or sixty and it will look materially
/// smoother on anything that moves -- and a projector fed from Rhevia shows
/// that difference plainly.
///
/// Like the resolution, this is decided before anything opens: every decoder
/// is started at this rate and the encoder is built around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FrameRate {
    /// Film. For a production that wants to look like one.
    P24,
    /// What Europe and India broadcast at.
    P25,
    #[default]
    P30,
    P50,
    /// Twice broadcast. Worth it for anything with movement in it, on a
    /// machine that can hold the rate.
    P60,
}

impl FrameRate {
    pub const ALL: [FrameRate; 5] =
        [FrameRate::P24, FrameRate::P25, FrameRate::P30, FrameRate::P50, FrameRate::P60];

    pub fn fps(self) -> f32 {
        match self {
            FrameRate::P24 => 24.0,
            FrameRate::P25 => 25.0,
            FrameRate::P30 => 30.0,
            FrameRate::P50 => 50.0,
            FrameRate::P60 => 60.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FrameRate::P24 => "24",
            FrameRate::P25 => "25",
            FrameRate::P30 => "30",
            FrameRate::P50 => "50",
            FrameRate::P60 => "60",
        }
    }

    /// What it costs, relative to thirty.
    ///
    /// Everything per-picture happens this many times more often: the
    /// compositing, the two monitor pictures and the encoding. Said plainly
    /// because choosing sixty on a machine that cannot hold it is worse than
    /// choosing thirty on one that can.
    pub fn cost(self) -> f32 {
        self.fps() / 30.0
    }

    fn from_label(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.label() == text)
    }
}

/// What size the stream leaves at.
///
/// Separate from the production size on purpose. A church hall with a slow
/// upload can run the production at 1080p — so the projector and the
/// recording are full quality — and still send 720p to the internet, which
/// is what most viewers are watching on a phone anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamSize {
    /// For a connection that will not carry anything more. Watchable, and
    /// far better than a 1080p stream that keeps stalling.
    P360,
    P480,
    /// What most church streams should be. Half the upload of 1080p and
    /// almost indistinguishable on a phone.
    P720,
    #[default]
    P1080,
    P1440,
    /// Very few viewers can receive this and almost none can tell. Offered
    /// because some productions genuinely need it.
    P2160,
}

impl StreamSize {
    pub const ALL: [StreamSize; 6] = [
        StreamSize::P360,
        StreamSize::P480,
        StreamSize::P720,
        StreamSize::P1080,
        StreamSize::P1440,
        StreamSize::P2160,
    ];

    pub fn size(self) -> (usize, usize) {
        match self {
            StreamSize::P360 => (640, 360),
            StreamSize::P480 => (854, 480),
            StreamSize::P720 => (1280, 720),
            StreamSize::P1080 => (1920, 1080),
            StreamSize::P1440 => (2560, 1440),
            StreamSize::P2160 => (3840, 2160),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            StreamSize::P360 => "360p",
            StreamSize::P480 => "480p",
            StreamSize::P720 => "720p",
            StreamSize::P1080 => "1080p",
            StreamSize::P1440 => "1440p",
            StreamSize::P2160 => "2160p",
        }
    }

    /// What the platforms ask for at this size, in kilobits a second.
    ///
    /// YouTube and Facebook publish ranges rather than numbers; these sit in
    /// the middle of both, which is where a stream looks right without
    /// wasting upload that a hall's connection may not have.
    pub fn suggested_kbps(self) -> u32 {
        match self {
            StreamSize::P360 => 800,
            StreamSize::P480 => 1500,
            StreamSize::P720 => 3000,
            StreamSize::P1080 => 6000,
            StreamSize::P1440 => 12_000,
            StreamSize::P2160 => 24_000,
        }
    }

    /// The range worth offering. Below the floor the picture falls apart;
    /// above the ceiling nothing improves and the upload is wasted.
    pub fn kbps_range(self) -> (u32, u32) {
        let suggested = self.suggested_kbps();
        (suggested / 3, suggested * 5 / 2)
    }

    fn from_label(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.label() == text)
    }
}

/// What the sound is encoded at.
///
/// 128 is transparent enough for speech and music together and is what most
/// platforms re-encode to anyway; 64 is for a connection that needs every
/// kilobit for the picture; 256 is for music that matters more than the
/// picture does.
pub const AUDIO_KBPS_CHOICES: [u32; 4] = [64, 96, 128, 192];

/// Everything remembered between sessions.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub resolution: Resolution,
    pub frame_rate: FrameRate,
    /// Whether the programme is played to the operator.
    pub monitor: bool,
    /// Which device it is played through, or None for whatever Windows
    /// currently calls the default.
    ///
    /// Worth remembering by name rather than always following the default:
    /// an operator listening on a desk while Windows points at a headset
    /// hears nothing at all, and cannot tell that from a fault in Rhevia.
    pub monitor_device: Option<String>,
    /// What size the stream leaves at, and what it is allowed to use.
    /// Which display the programme is thrown onto full screen, by name.
    ///
    /// The projector in a hall is a second display with nothing on it but
    /// the programme. Remembered by name so the same projector comes back
    /// next Sunday without being chosen again.
    pub output_display: Option<String>,
    pub stream_size: StreamSize,
    pub stream_kbps: u32,
    pub audio_kbps: u32,
}

impl Default for Settings {
    fn default() -> Self {
        // Listening by default: someone who adds a video and hears nothing
        // has found a fault, not a preference.
        Self {
            resolution: Resolution::default(),
            frame_rate: FrameRate::default(),
            monitor: true,
            monitor_device: None,
            output_display: None,
            stream_size: StreamSize::default(),
            stream_kbps: StreamSize::default().suggested_kbps(),
            audio_kbps: 128,
        }
    }
}

fn path() -> Option<PathBuf> {
    let base = std::env::var("LOCALAPPDATA")
        .or_else(|_| std::env::var("HOME"))
        .ok()?;
    let directory = PathBuf::from(base).join("Rhevia");
    std::fs::create_dir_all(&directory).ok()?;
    Some(directory.join("settings.txt"))
}

impl Settings {
    /// Reads what was chosen last time, or the defaults.
    pub fn load() -> Self {
        let Some(path) = path() else { return Self::default() };
        let Ok(text) = std::fs::read_to_string(path) else { return Self::default() };
        Self::parse(&text)
    }

    /// Writes the choice down. A failure here costs the setting next time,
    /// which is not worth interrupting anyone over.
    pub fn save(&self) {
        if let Some(path) = path() {
            let _ = std::fs::write(path, self.to_text());
        }
    }

    /// Plain `key = value` lines, so the file can be read and fixed by hand
    /// when something goes wrong with it.
    pub fn to_text(&self) -> String {
        format!(
            "# Rhevia. Resolution takes effect when Rhevia is restarted.\n\
             resolution = {}\n\
             frame_rate = {}\n\
             monitor = {}\n\
             monitor_device = {}\n\
             output_display = {}\n\
             stream_size = {}\n\
             stream_kbps = {}\n\
             audio_kbps = {}\n",
            self.resolution.label(),
            self.frame_rate.label(),
            self.monitor,
            self.monitor_device.as_deref().unwrap_or("default"),
            self.output_display.as_deref().unwrap_or("none"),
            self.stream_size.label(),
            self.stream_kbps,
            self.audio_kbps
        )
    }

    /// Reads that back. Anything unrecognised keeps its default rather than
    /// refusing the whole file: a setting added in a later version must not
    /// stop an earlier one from starting.
    pub fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            match key.trim() {
                "resolution" => {
                    if let Some(resolution) = Resolution::from_label(value.trim()) {
                        settings.resolution = resolution;
                    }
                }
                "frame_rate" => {
                    if let Some(rate) = FrameRate::from_label(value.trim()) {
                        settings.frame_rate = rate;
                    }
                }
                "monitor" => settings.monitor = value.trim() != "false",
                "output_display" => {
                    settings.output_display = match value.trim() {
                        "" | "none" => None,
                        name => Some(name.to_string()),
                    };
                }
                "stream_size" => {
                    if let Some(size) = StreamSize::from_label(value.trim()) {
                        settings.stream_size = size;
                    }
                }
                "stream_kbps" => {
                    if let Ok(kbps) = value.trim().parse() {
                        settings.stream_kbps = kbps;
                    }
                }
                "audio_kbps" => {
                    if let Ok(kbps) = value.trim().parse() {
                        settings.audio_kbps = kbps;
                    }
                }
                "monitor_device" => {
                    // Taken whole: Windows names devices things like
                    // "Speakers (2- USB Audio = Device)", and splitting on
                    // every equals sign would lose half of that.
                    settings.monitor_device = match value.trim() {
                        "" | "default" => None,
                        name => Some(name.to_string()),
                    };
                }
                _ => {}
            }
        }
        settings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_production_is_1080p_and_audible() {
        // Both defaults are the answer to a complaint: a 720p production made
        // a 1080p file look soft, and a monitor that was off made a video
        // seem silent.
        let settings = Settings::default();
        assert_eq!(settings.resolution, Resolution::Hd1080);
        assert_eq!(settings.resolution.size(), (1920, 1080));
        assert!(settings.monitor);
    }

    #[test]
    fn every_resolution_is_even_in_both_directions() {
        // H.264 encodes in sixteen-pixel macroblocks and refuses odd sizes.
        for resolution in Resolution::ALL {
            let (w, h) = resolution.size();
            assert_eq!(w % 2, 0, "{} is odd", resolution.label());
            assert_eq!(h % 2, 0, "{} is odd", resolution.label());
        }
    }

    #[test]
    fn bitrate_rises_with_resolution() {
        // A bigger picture at the same bitrate looks worse, not better, which
        // is the trap in offering a resolution choice at all.
        let mut previous = 0;
        for resolution in Resolution::ALL {
            assert!(
                resolution.bitrate() > previous,
                "{} is not given more room than the size below it",
                resolution.label()
            );
            previous = resolution.bitrate();
        }
    }

    #[test]
    fn a_choice_survives_being_written_and_read_back() {
        for resolution in Resolution::ALL {
            for monitor in [true, false] {
                for device in [None, Some("Speakers (Realtek(R) Audio)".to_string())] {
                    let settings =
                        Settings {
                            resolution,
                            monitor,
                            monitor_device: device.clone(),
                            ..Settings::default()
                        };
                    assert_eq!(Settings::parse(&settings.to_text()), settings);
                }
            }
        }
    }

    #[test]
    fn an_empty_or_missing_file_gives_the_defaults() {
        assert_eq!(Settings::parse(""), Settings::default());
        assert_eq!(Settings::parse("# only a comment\n"), Settings::default());
    }

    #[test]
    fn a_setting_from_a_later_version_does_not_stop_this_one_starting() {
        let text = "resolution = 1440p\nsomething_new = 42\nmonitor = false\n";
        let settings = Settings::parse(text);
        assert_eq!(settings.resolution, Resolution::Qhd1440);
        assert!(!settings.monitor);
    }

    #[test]
    fn nonsense_keeps_the_default_rather_than_refusing_the_file() {
        let settings = Settings::parse("resolution = 8000p\nmonitor = yes please\n");
        assert_eq!(settings.resolution, Resolution::Hd1080);
        // Anything but the word false means listening, so a typo leaves the
        // operator able to hear rather than silently deaf.
        assert!(settings.monitor);
    }

    #[test]
    fn a_device_name_with_awkward_characters_survives() {
        let name = "Speakers (2- USB Audio = Device)";
        let settings = Settings { monitor_device: Some(name.into()), ..Settings::default() };
        assert_eq!(Settings::parse(&settings.to_text()).monitor_device, Some(name.to_string()));
    }

    #[test]
    fn the_word_default_means_whatever_windows_is_pointing_at() {
        // So that unplugging the named device leaves the operator hearing
        // something rather than nothing.
        assert_eq!(Settings::parse("monitor_device = default\n").monitor_device, None);
        assert_eq!(Settings::parse("monitor_device =\n").monitor_device, None);
    }

    #[test]
    fn a_stream_setting_survives_being_written_and_read_back() {
        for size in StreamSize::ALL {
            for audio in AUDIO_KBPS_CHOICES {
                let settings = Settings {
                    stream_size: size,
                    stream_kbps: size.suggested_kbps(),
                    audio_kbps: audio,
                    ..Settings::default()
                };
                assert_eq!(Settings::parse(&settings.to_text()), settings);
            }
        }
    }

    #[test]
    fn every_stream_size_is_even_and_gets_more_room_than_the_one_below() {
        // H.264 encodes in macroblocks and refuses odd sizes, and a bigger
        // picture at the same bitrate looks worse than the smaller one did.
        let mut previous = 0;
        for size in StreamSize::ALL {
            let (w, h) = size.size();
            assert_eq!(w % 2, 0, "{} is odd", size.label());
            assert_eq!(h % 2, 0, "{} is odd", size.label());
            assert!(
                size.suggested_kbps() > previous,
                "{} is not given more room than the size below it",
                size.label()
            );
            previous = size.suggested_kbps();
        }
    }

    #[test]
    fn the_suggested_bitrate_is_inside_the_range_offered() {
        for size in StreamSize::ALL {
            let (low, high) = size.kbps_range();
            let suggested = size.suggested_kbps();
            assert!(
                low < suggested && suggested < high,
                "{}: {suggested} is not inside {low}..{high}",
                size.label()
            );
        }
    }

    #[test]
    fn a_frame_rate_survives_being_written_and_read_back() {
        for rate in FrameRate::ALL {
            let settings = Settings { frame_rate: rate, ..Settings::default() };
            assert_eq!(Settings::parse(&settings.to_text()).frame_rate, rate);
        }
    }

    #[test]
    fn the_frame_rates_are_the_ones_broadcast_uses_and_cost_what_they_say() {
        // A rate nobody broadcasts at is a rate every platform will re-encode.
        let mut previous = 0.0;
        for rate in FrameRate::ALL {
            assert!(rate.fps() > previous, "{} is not above the one below", rate.label());
            previous = rate.fps();
            assert!(
                (rate.cost() - rate.fps() / 30.0).abs() < 1e-6,
                "{} does not cost what it says",
                rate.label()
            );
        }
        assert_eq!(FrameRate::default().fps(), 30.0);
    }

    #[test]
    fn labels_round_trip() {
        for resolution in Resolution::ALL {
            assert_eq!(Resolution::from_label(resolution.label()), Some(resolution));
        }
        assert_eq!(Resolution::from_label("720"), None);
    }
}
