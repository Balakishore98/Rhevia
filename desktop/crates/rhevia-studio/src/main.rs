//! Rhevia Studio — the live production switcher.

// No console window behind the UI in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio_ui;
mod engine;
mod theme;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rhevia_studio=info,rhevia_pipeline=info".into()),
        )
        .init();

    let engine = engine::start();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1780.0, 1040.0])
            // Below this the two monitors stop being usable side by side.
            .with_min_inner_size([1280.0, 860.0])
            .with_title("Rhevia Studio"),
        ..Default::default()
    };

    eframe::run_native(
        "Rhevia Studio",
        options,
        Box::new(|cc| Ok(Box::new(app::StudioApp::new(cc, engine)))),
    )
}
