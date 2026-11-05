//! User settings, saved as JSON in the app's config folder. Unknown or missing fields fall
//! back to defaults, so old files keep working after updates.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Preset id from `presets()`, e.g. "discord".
    pub preset: String,
    /// Custom limit in bytes, used when `preset` is "custom".
    pub custom_bytes: u64,
    /// "kompacter" | "discord"
    pub style: String,
    /// "system" | "light" | "dark" (Kompacter style)
    pub theme: String,
    /// "match" | "light" | "ash" | "dark" | "onyx" (Discord style)
    pub discord_theme: String,
    pub gpu: bool,
    pub paste_guard: bool,
    pub start_with_windows: bool,
    pub hotkey: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            preset: "discord".into(),
            custom_bytes: 20 * 1024 * 1024,
            style: "kompacter".into(),
            theme: "system".into(),
            discord_theme: "match".into(),
            gpu: true,
            paste_guard: false,
            start_with_windows: false,
            hotkey: "Ctrl+Shift+K".into(),
        }
    }
}

fn file(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("settings.json"))
}

pub fn load(app: &AppHandle) -> Settings {
    file(app)
        .ok()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(app: &AppHandle, s: &Settings) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
    // Write then rename, so a crash mid-write never leaves a broken file.
    let path = file(app)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    std::fs::rename(tmp, path).map_err(|e| e.to_string())
}
