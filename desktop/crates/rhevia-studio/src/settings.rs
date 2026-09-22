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

/// Everything remembered between sessions.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub resolution: Resolution,
    /// Whether the programme is played to the operator.
    pub monitor: bool,
    /// Which device it is played through, or None for whatever Windows
    /// currently calls the default.
    ///
    /// Worth remembering by name rather than always following the default:
    /// an operator listening on a desk while Windows points at a headset
    /// hears nothing at all, and cannot tell that from a fault in Rhevia.
    pub monitor_device: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        // Listening by default: someone who adds a video and hears nothing
        // has found a fault, not a preference.
        Self { resolution: Resolution::default(), monitor: true, monitor_device: None }
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
             monitor = {}\n\
             monitor_device = {}\n",
            self.resolution.label(),
            self.monitor,
            self.monitor_device.as_deref().unwrap_or("default")
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
                "monitor" => settings.monitor = value.trim() != "false",
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
                        Settings { resolution, monitor, monitor_device: device.clone() };
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
    fn labels_round_trip() {
        for resolution in Resolution::ALL {
            assert_eq!(Resolution::from_label(resolution.label()), Some(resolution));
        }
        assert_eq!(Resolution::from_label("720"), None);
    }
}
