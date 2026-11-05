// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod clickthrough;
mod clipboard;
mod commands;
mod settings;
mod tray;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use kompacter_core::{Engine, Ffmpeg, MediaInfo};
use tauri::Manager;

/// Shared by all commands. The engine caches GPU detection, so it lives for the whole run.
pub struct AppState {
    pub engine: Arc<Engine>,
    pub cancel: Arc<AtomicBool>,
    /// ffprobe results keyed by path + modified time, so trimmer drags don't re-probe.
    pub probes: Mutex<HashMap<(PathBuf, Option<SystemTime>), MediaInfo>>,
    /// Card rectangle reported by the page: [x, y, w, h] in logical px.
    pub hit: Arc<Mutex<[f64; 4]>>,
}

/// Installed builds ship ffmpeg next to the app (Tauri sidecar). Dev builds use the PATH.
fn bundled_ffmpeg() -> Ffmpeg {
    let exe = |name: &str| {
        let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join(&file)))
            .filter(|p| p.is_file())
    };
    match (exe("ffmpeg"), exe("ffprobe")) {
        (Some(ffmpeg), Some(ffprobe)) => Ffmpeg { ffmpeg, ffprobe },
        _ => Ffmpeg::default(),
    }
}

fn main() {
    tauri::Builder::default()
        .manage(AppState {
            engine: Arc::new(Engine::new(bundled_ffmpeg())),
            cancel: Arc::new(AtomicBool::new(false)),
            probes: Mutex::new(HashMap::new()),
            hit: Arc::new(Mutex::new([0.0; 4])),
        })
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::presets,
            commands::inspect,
            commands::compress,
            commands::cancel,
            commands::hide,
            commands::pick_file,
            commands::clipboard_files,
            commands::show_in_folder,
            commands::encoders,
            commands::thumbnails,
            commands::pick_folder,
            commands::set_hit_rect,
            commands::open_outputs,
            commands::get_settings,
            commands::set_settings,
        ])
        .setup(|app| {
            tray::create(app.handle())?;
            clickthrough::start(app.handle(), app.state::<AppState>().hit.clone());
            commands::clean_old_outputs(app.handle(), None);
            // Detect GPU encoders in the background so the first compress doesn't wait.
            let engine = app.state::<AppState>().engine.clone();
            std::thread::spawn(move || {
                engine.hw_encoders();
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to start Kompacter");
}
