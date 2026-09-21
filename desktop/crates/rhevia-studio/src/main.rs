//! Rhevia Studio — the live production switcher.

// No console window behind the UI in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio_ui;
mod engine;
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

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rhevia_studio=info,rhevia_pipeline=info".into()),
        )
        .init();

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
