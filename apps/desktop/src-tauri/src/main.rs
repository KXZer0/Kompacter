// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod clipboard;
mod commands;
mod settings;
mod tray;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use kompacter_core::{Engine, MediaInfo};
use tauri::Manager;

/// Shared by all commands. The engine caches GPU detection, so it lives for the whole run.
pub struct AppState {
    pub engine: Arc<Engine>,
    pub cancel: Arc<AtomicBool>,
    /// ffprobe results keyed by path + modified time, so trimmer drags don't re-probe.
    pub probes: Mutex<HashMap<(PathBuf, Option<SystemTime>), MediaInfo>>,
}

fn main() {
    tauri::Builder::default()
        .manage(AppState {
            engine: Arc::new(Engine::default()),
            cancel: Arc::new(AtomicBool::new(false)),
            probes: Mutex::new(HashMap::new()),
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
            commands::get_settings,
            commands::set_settings,
        ])
        .setup(|app| {
            tray::create(app.handle())?;
            commands::clean_old_outputs(app.handle());
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
