//! The ffmpeg Rhevia carries with it, in the single-file build.
//!
//! Everything Rhevia does itself is native and needs nothing installed.
//! Media playback is the one exception: it drives ffmpeg as a subprocess to
//! decode whatever someone drops on it, because writing demuxers and decoders
//! for every container a church has on a memory stick is years of work that
//! has already been done.
//!
//! "Install ffmpeg first" is not an answer for someone who has been handed a
//! single file and told it works. So the `packed` build carries its own copy
//! inside the executable and lays it out beside the settings on first run.
//!
//! The copy is LGPL and is run as a separate program, never linked, and is
//! carried unmodified. Its licence travels with it and is written out beside
//! it.
//!
//! Without the `packed` feature this does nothing at all and Rhevia looks for
//! ffmpeg on PATH as it always has.

#[cfg(not(feature = "packed"))]
pub fn unpack() {}

#[cfg(feature = "packed")]
pub use packed::unpack;

#[cfg(feature = "packed")]
mod packed {
    use std::io::Write;
    use std::path::{Path, PathBuf};

    /// Both tools, compressed together.
    ///
    /// Together on purpose: they are built from the same libraries and are
    /// very nearly the same bytes, so one window wide enough to span both
    /// turns 268 MB into 44 -- the second becomes little more than a
    /// reference to the first.
    const TOOLS: &[u8] = include_bytes!("../../../vendor/ffmpeg/tools.xz");
    const LICENCE: &[u8] = include_bytes!("../../../vendor/ffmpeg/LICENSE.txt");
    const MANIFEST: &str = include_str!("../../../vendor/ffmpeg/manifest.json");

    /// Lays the tools out, and tells the media crate to use them.
    ///
    /// Done once and remembered: unpacking is a quarter of a gigabyte of
    /// writing and there is no reason to repeat it every time Rhevia starts.
    /// A stamp file records which build is already there, so a Rhevia
    /// carrying a newer ffmpeg replaces it and one carrying the same leaves
    /// it alone.
    pub fn unpack() {
        let Some(home) = directory() else {
            tracing::warn!("nowhere to unpack ffmpeg; looking on PATH instead");
            return;
        };

        let stamp = home.join("release.txt");
        let wanted = release();
        let ready = std::fs::read_to_string(&stamp).map(|s| s.trim() == wanted).unwrap_or(false)
            && home.join("ffmpeg.exe").exists()
            && home.join("ffprobe.exe").exists();

        if !ready {
            if let Err(e) = lay_out(&home) {
                tracing::error!(error = %e, "could not unpack ffmpeg; looking on PATH instead");
                return;
            }
            let _ = std::fs::write(&stamp, wanted);
            tracing::info!(path = %home.display(), "unpacked the ffmpeg Rhevia carries");
        }

        rhevia_media::use_own_ffmpeg(home);
    }

    fn directory() -> Option<PathBuf> {
        let base = std::env::var("LOCALAPPDATA").or_else(|_| std::env::var("HOME")).ok()?;
        let home = PathBuf::from(base).join("Rhevia").join("runtime");
        std::fs::create_dir_all(&home).ok()?;
        Some(home)
    }

    /// Which build is inside this executable.
    fn release() -> &'static str {
        MANIFEST
            .lines()
            .find_map(|line| {
                let line = line.trim();
                let rest = line.strip_prefix("\"release\":")?;
                Some(rest.trim().trim_matches(|c| c == '"' || c == ',').trim())
            })
            .unwrap_or("unknown")
    }

    /// How long each tool is, so the one blob can be cut in two.
    fn lengths() -> (usize, usize) {
        let number = |key: &str| -> usize {
            MANIFEST
                .lines()
                .find_map(|line| {
                    let rest = line.trim().strip_prefix(&format!("\"{key}\":"))?;
                    rest.trim().trim_end_matches(',').trim().parse().ok()
                })
                .unwrap_or(0)
        };
        (number("ffmpeg_bytes"), number("ffprobe_bytes"))
    }

    fn lay_out(home: &Path) -> std::io::Result<()> {
        let (ffmpeg_len, ffprobe_len) = lengths();
        if ffmpeg_len == 0 || ffprobe_len == 0 {
            return Err(std::io::Error::other("the packed manifest is unreadable"));
        }

        let mut both = Vec::with_capacity(ffmpeg_len + ffprobe_len);
        lzma_rs::xz_decompress(&mut std::io::Cursor::new(TOOLS), &mut both)
            .map_err(|e| std::io::Error::other(format!("{e:?}")))?;

        if both.len() != ffmpeg_len + ffprobe_len {
            return Err(std::io::Error::other(format!(
                "unpacked {} bytes, expected {}",
                both.len(),
                ffmpeg_len + ffprobe_len
            )));
        }

        // Written beside their destination and renamed into place, so a
        // Rhevia that is stopped half way through leaves nothing that looks
        // like a working tool but is not one.
        write_atomically(&home.join("ffmpeg.exe"), &both[..ffmpeg_len])?;
        write_atomically(&home.join("ffprobe.exe"), &both[ffmpeg_len..])?;
        write_atomically(&home.join("LICENSE.txt"), LICENCE)?;
        write_atomically(&home.join("manifest.json"), MANIFEST.as_bytes())?;
        Ok(())
    }

    fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        let partial = path.with_extension("partial");
        {
            let mut file = std::fs::File::create(&partial)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        // Windows will not rename onto an existing file.
        let _ = std::fs::remove_file(path);
        std::fs::rename(&partial, path)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_manifest_describes_what_is_packed() {
            // The blob is one piece of bytes with two programs in it, and
            // cutting it in the wrong place produces two files that look like
            // executables and are not.
            let (ffmpeg, ffprobe) = lengths();
            assert!(ffmpeg > 1_000_000, "ffmpeg is described as {ffmpeg} bytes");
            assert!(ffprobe > 1_000_000, "ffprobe is described as {ffprobe} bytes");
            assert_ne!(release(), "unknown", "the packed build does not say what it is");
        }

        #[test]
        fn what_is_packed_unpacks_to_two_real_programs() {
            // Slow, but the one thing worth being certain of: the whole point
            // of the packed build is a machine with nothing on it.
            let mut both = Vec::new();
            lzma_rs::xz_decompress(&mut std::io::Cursor::new(TOOLS), &mut both)
                .expect("the packed tools should decompress");

            let (ffmpeg_len, ffprobe_len) = lengths();
            assert_eq!(both.len(), ffmpeg_len + ffprobe_len);

            // Both halves have to be Windows executables, or the split is
            // in the wrong place.
            assert_eq!(&both[..2], b"MZ", "the first half is not a program");
            assert_eq!(
                &both[ffmpeg_len..ffmpeg_len + 2],
                b"MZ",
                "the second half is not a program, so the split is wrong"
            );
        }
    }
}
