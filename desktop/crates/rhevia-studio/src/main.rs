//! Rhevia Studio — the live production switcher.

// No console window behind the UI in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio_ui;
mod engine;
mod runtime;
mod settings;
mod theme;

/// The mark, drawn by the window manager.
///
/// Separate from the icon compiled into the executable: that one is what
/// Explorer and the Start Menu show, this one is what the taskbar and the
/// window's own corner show while it is running. Both are needed, and only
/// having the first leaves a blank square on the taskbar.
fn window_icon() -> Option<eframe::egui::IconData> {
    let png = include_bytes!("../../../assets/icon-256.png");
    let image = image::load_from_memory(png).ok()?.into_rgba8();
    let (width, height) = image.dimensions();
    Some(eframe::egui::IconData { rgba: image.into_raw(), width, height })
}

/// Where a crash is written down.
///
/// A live production tool that disappears mid-show leaves an operator with
/// nothing to report and nothing to fix. This costs nothing when all is well
/// and is the difference between a bug that can be found and one that can
/// only be guessed at.
fn crash_log_path() -> Option<std::path::PathBuf> {
    let base = std::env::var("LOCALAPPDATA").ok()?;
    let directory = std::path::PathBuf::from(base).join("Rhevia");
    std::fs::create_dir_all(&directory).ok()?;
    Some(directory.join("crash.log"))
}

/// Writes any panic to a file before the program goes.
fn record_crashes() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(path) = crash_log_path() {
            use std::io::Write;

            let when = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let where_ = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_else(|| "unknown".into());

            // Appended rather than replaced: a crash that happens twice is
            // worth more than either one alone.
            if let Ok(mut file) =
                std::fs::OpenOptions::new().create(true).append(true).open(&path)
            {
                let _ = writeln!(
                    file,
                    "
--- Rhevia {} crashed at unix {when} ---
{where_}
{info}
{}",
                    env!("CARGO_PKG_VERSION"),
                    std::backtrace::Backtrace::force_capture()
                );
            }
        }
        previous(info);
    }));
}

fn main() -> eframe::Result<()> {
    record_crashes();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rhevia_studio=info,rhevia_pipeline=info".into()),
        )
        .init();

    // Lays out the ffmpeg this build carries, if it carries one. Done before
    // anything asks whether ffmpeg exists, so the answer is about the copy
    // Rhevia brought rather than about the machine.
    runtime::unpack();

    let engine = engine::start();

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([1780.0, 1040.0])
        // Below this the two monitors stop being usable side by side.
        .with_min_inner_size([1280.0, 860.0])
        .with_title("Rhevia Studio");

    // A mark that will not decode is not worth refusing to start over.
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    } else {
        tracing::warn!("the window icon could not be decoded");
    }

    let options = eframe::NativeOptions { viewport, ..Default::default() };

    eframe::run_native(
        "Rhevia Studio",
        options,
        Box::new(|cc| Ok(Box::new(app::StudioApp::new(cc, engine)))),
    )
}
