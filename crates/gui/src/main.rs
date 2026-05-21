#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod pipeline;
mod theme;

use eframe::egui;
use yabcompiler_core::config;

/// PNG used for the window decoration and taskbar icon. Decoded once at
/// startup; the embedded `.exe` icon (see `build.rs`) covers Explorer.
const ICON_PNG: &[u8] = include_bytes!("../../../icons/icon.png");

fn main() -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1080.0, 880.0])
        .with_min_inner_size([820.0, 720.0])
        // Full name lives in the OS title bar; the in-app top bar no
        // longer repeats it.
        .with_title(config::APP_TITLE);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(ICON_PNG) {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        // App id for persistence storage — keep stable so saved theme
        // survives across versions; the visible title is set above.
        "YABCompiler",
        options,
        Box::new(|cc| {
            // Restore the saved theme (if any) before the first frame so
            // there's no flash of the default flavor.
            let app = app::YabApp::new(cc.storage);
            cc.egui_ctx.set_visuals(theme::visuals(app.flavor()));
            Ok(Box::new(app))
        }),
    )
}
