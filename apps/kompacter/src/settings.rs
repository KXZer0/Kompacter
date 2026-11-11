//! User settings, saved as JSON next to other app data. Same file and format the first
//! test builds used, so choices carry over. Unknown or missing fields fall back to defaults.

use std::{io, path::PathBuf};

use kompacter_core::{ArchiveFormat, AudioFormat, ImageFormat, VideoCodec};
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
    pub gpu: bool,
    /// Where results go: "app" | "beside" | "custom" | "replace" (see output.rs).
    pub output: String,
    /// Folder for `output = "custom"`.
    pub output_dir: String,
    /// Hide the flyout when it loses focus (not while a file is dragged over it).
    pub auto_hide: bool,
    pub start_with_windows: bool,
    /// Start with Windows has been switched on once by default. After that the choice is
    /// the user's (Settings or Task Manager) and is never switched back on.
    pub start_default_done: bool,
    /// The one-time "keep the tray icon visible" tip was answered or dismissed.
    pub tray_tip_seen: bool,
    /// Output formats ("h264" | "h265" | "av1", "auto" | "mp3" | "opus",
    /// "auto" | "jpeg" | "webp", "zip" | "7z").
    pub video_codec: VideoCodec,
    pub audio_format: AudioFormat,
    pub image_format: ImageFormat,
    pub archive_format: ArchiveFormat,
    /// Global shortcut for the quick bar: "off" | "ctrl+alt+k" | "ctrl+shift+k" |
    /// "ctrl+shift+space".
    pub shortcut: String,
    /// Catch Ctrl+V in Discord when the copied file is over the limit.
    pub paste_guard: bool,
    /// Offer the quick bar when a file over the limit is copied.
    pub watch_copies: bool,
    /// Shrink new clips that land in `clip_folders` on their own.
    pub watch_folders: bool,
    pub clip_folders: Vec<String>,
}

/// The shortcut choices, in the order Settings shows them.
pub const SHORTCUTS: [(&str, &str); 4] = [
    ("off", "Off"),
    ("ctrl+alt+k", "Ctrl+Alt+K"),
    ("ctrl+shift+k", "Ctrl+Shift+K"),
    ("ctrl+shift+space", "Ctrl+Shift+Space"),
];

/// Windows modifier flags and virtual key for a shortcut id (None: off).
pub fn shortcut_keys(id: &str) -> Option<(u32, u32)> {
    const ALT: u32 = 1;
    const CTRL: u32 = 2;
    const SHIFT: u32 = 4;
    match id {
        "ctrl+alt+k" => Some((CTRL | ALT, 0x4B)),
        "ctrl+shift+k" => Some((CTRL | SHIFT, 0x4B)),
        "ctrl+shift+space" => Some((CTRL | SHIFT, 0x20)),
        _ => None,
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            preset: "discord".into(),
            custom_bytes: 20 * 1024 * 1024,
            theme: "system".into(),
            style: "kompacter".into(),
            discord_theme: "match".into(),
            gpu: true,
            output: "app".into(),
            output_dir: String::new(),
            auto_hide: true,
            start_with_windows: false,
            start_default_done: false,
            tray_tip_seen: false,
            video_codec: VideoCodec::default(),
            audio_format: AudioFormat::default(),
            image_format: ImageFormat::default(),
            archive_format: ArchiveFormat::default(),
            shortcut: "ctrl+alt+k".into(),
            paste_guard: false,
            watch_copies: false,
            watch_folders: false,
            clip_folders: Vec::new(),
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

pub fn save(s: &Settings) -> io::Result<()> {
    let path = file().ok_or_else(|| io::Error::other("Settings folder is unavailable"))?;
    let json = serde_json::to_vec_pretty(s)?;
    // Write then rename, so a crash mid-write never leaves a broken file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(tmp, path)
}
