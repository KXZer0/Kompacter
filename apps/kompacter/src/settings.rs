//! User settings, saved as JSON next to other app data. Same file and format the first
//! test builds used, so choices carry over. Unknown or missing fields fall back to defaults.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Preset id, e.g. "discord", or "custom".
    pub preset: String,
    /// Custom limit in bytes, used when `preset` is "custom".
    pub custom_bytes: u64,
    /// "system" | "light" | "dark" (Kompacter style)
    pub theme: String,
    /// "kompacter" | "discord"
    pub style: String,
    /// "match" | "light" | "ash" | "dark" | "onyx" (Discord style)
    pub discord_theme: String,
    /// CPU drawing and simpler animations, for the least memory.
    pub low_memory: bool,
    pub gpu: bool,
    /// Where results go: "app" | "beside" | "custom" | "replace" (see output.rs).
    pub output: String,
    /// Folder for `output = "custom"`.
    pub output_dir: String,
    /// Hide the flyout when it loses focus (not while a file is dragged over it).
    pub auto_hide: bool,
    pub start_with_windows: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            preset: "discord".into(),
            custom_bytes: 20 * 1024 * 1024,
            theme: "system".into(),
            style: "kompacter".into(),
            discord_theme: "match".into(),
            low_memory: false,
            gpu: true,
            output: "app".into(),
            output_dir: String::new(),
            auto_hide: true,
            start_with_windows: false,
        }
    }
}

fn file() -> Option<PathBuf> {
    let dir = crate::sys::config_dir()?.join("app.kompacter.desktop");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("settings.json"))
}

pub fn load() -> Settings {
    file()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(s: &Settings) {
    let Some(path) = file() else { return };
    let Ok(json) = serde_json::to_vec_pretty(s) else {
        return;
    };
    // Write then rename, so a crash mid-write never leaves a broken file.
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(tmp, path);
    }
}
