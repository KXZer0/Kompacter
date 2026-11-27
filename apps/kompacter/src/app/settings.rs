//! User settings, saved as JSON next to other app data. Same file and format the first
//! test builds used, so choices carry over. Unknown or missing fields fall back to defaults.

use std::{io, path::PathBuf};

use kompacter_core::{
    ArchiveFormat, AudioFormat, CompressionPreference, ImageFormat, Options, VideoCodec,
};
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
    /// Windows: draw the window on the GPU (Skia on Direct3D 12) instead of the CPU.
    /// `None` follows the preference: on with Efficiency, off otherwise. Read at start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_drawing: Option<bool>,
    /// Where results go: "app" | "beside" | "custom" | "replace" (see output.rs).
    pub output: String,
    /// Folder for `output = "custom"`.
    pub output_dir: String,
    /// Hide the flyout when it loses focus (not while a file is dragged over it).
    pub auto_hide: bool,
    /// Starts with the system (Windows, macOS login item, Linux autostart). The name
    /// predates macOS and Linux and is kept so existing settings files still load.
    pub start_with_windows: bool,
    /// Start with the system has been switched on once by default. After that the choice is
    /// the user's (Settings or the system's own) and is never switched back on.
    pub start_default_done: bool,
    /// The first-run screen (what Kompacter may do on its own) has been answered.
    pub welcomed: bool,
    /// "Kompact" in Explorer's right-click menu for files and folders.
    pub explorer_menu: bool,
    /// The one-time "keep the tray icon visible" tip was answered or dismissed.
    pub tray_tip_seen: bool,
    pub compression_preference: CompressionPreference,
    // Per-format overrides; `None` follows the preference. Files written before the
    // preference existed are migrated once by `parse` (old defaults become `None`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_codec: Option<VideoCodec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_format: Option<AudioFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_format: Option<ImageFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_format: Option<ArchiveFormat>,
    /// Global shortcut for the quick bar: "off" | "ctrl+alt+k" | "ctrl+shift+k" |
    /// "ctrl+shift+space".
    pub shortcut: String,
    /// Catch Ctrl+V in a chat app (Discord, Slack, WhatsApp, Telegram, Signal) when the
    /// copied file is over that app's limit.
    pub paste_guard: bool,
    /// Offer the quick bar when a file over the limit is copied.
    pub watch_copies: bool,
    /// Shrink new clips that land in `clip_folders` on their own.
    pub watch_folders: bool,
    pub clip_folders: Vec<String>,
    /// Check GitHub for a new version once a day and install it on the next restart.
    pub auto_update: bool,
    /// The last version whose "What's new" was shown (or that was installed fresh).
    pub seen_version: String,
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
            gpu_drawing: None,
            output: "app".into(),
            output_dir: String::new(),
            auto_hide: true,
            start_with_windows: false,
            start_default_done: false,
            welcomed: false,
            explorer_menu: true,
            tray_tip_seen: false,
            compression_preference: CompressionPreference::default(),
            video_codec: None,
            audio_format: None,
            image_format: None,
            archive_format: None,
            shortcut: "ctrl+alt+k".into(),
            paste_guard: false,
            auto_update: true,
            seen_version: String::new(),
            watch_copies: false,
            watch_folders: false,
            clip_folders: Vec::new(),
        }
    }
}

impl Settings {
    /// Changes the preference. Efficiency switches GPU encoding on, but it stays the
    /// user's switch: they can turn it off again and the preference keeps it that way.
    pub fn set_preference(&mut self, preference: CompressionPreference) {
        if preference == CompressionPreference::Efficiency
            && self.compression_preference != preference
        {
            self.gpu = true;
        }
        self.compression_preference = preference;
    }

    /// Whether the window is drawn on the GPU (see `gpu_drawing`).
    pub fn gpu_drawing(&self) -> bool {
        self.gpu_drawing
            .unwrap_or(self.compression_preference == CompressionPreference::Efficiency)
    }

    pub fn options(&self, target: u64) -> Options {
        let mut options = self.compression_preference.options(target);
        options.video_codec = self.video_codec.unwrap_or(options.video_codec);
        options.audio_format = self.audio_format.unwrap_or(options.audio_format);
        options.image_format = self.image_format.unwrap_or(options.image_format);
        options.archive = self.archive_format.unwrap_or(options.archive);
        // The switch decides; choosing Efficiency only turns it on (`set_preference`).
        options.gpu &= self.gpu;
        options
    }
}

fn file() -> Option<PathBuf> {
    let dir = crate::platform::config_dir()?.join("app.kompacter.desktop");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("settings.json"))
}

pub fn load() -> Settings {
    let Some((settings, migrated)) = file()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| parse(&b))
    else {
        return Settings::default();
    };
    if migrated {
        // Persist once, so a later run sees the preference key and keeps the result.
        let _ = save(&settings);
    }
    settings
}

/// Reads saved settings. Builds from before the compression preference wrote every
/// format field, so their old defaults (H.264, Auto, Auto, Zip) were never a choice:
/// those become "follow the preference", other values stay as overrides. The flag says
/// the file was migrated and should be saved again.
fn parse(bytes: &[u8]) -> Option<(Settings, bool)> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let legacy = value
        .as_object()
        .is_some_and(|o| !o.contains_key("compressionPreference"));
    let mut settings: Settings = serde_json::from_value(value).ok()?;
    if legacy {
        settings.video_codec = settings.video_codec.filter(|v| *v != VideoCodec::H264);
        settings.audio_format = settings.audio_format.filter(|v| *v != AudioFormat::Auto);
        settings.image_format = settings.image_format.filter(|v| *v != ImageFormat::Auto);
        settings.archive_format = settings.archive_format.filter(|v| *v != ArchiveFormat::Zip);
    }
    Some((settings, legacy))
}

pub fn save(s: &Settings) -> io::Result<()> {
    let path = file().ok_or_else(|| io::Error::other("Settings folder is unavailable"))?;
    let json = serde_json::to_vec_pretty(s)?;
    // Write then rename, so a crash mid-write never leaves a broken file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kompacter_core::Speed;

    #[test]
    fn preference_changes_defaults_and_round_trips() {
        let mut settings: Settings = serde_json::from_str("{}").unwrap();
        assert!(!settings.welcomed);
        assert_eq!(
            settings.compression_preference,
            CompressionPreference::Balanced
        );
        assert_eq!(settings.options(10).speed, Speed::Balanced);
        assert_eq!(settings.options(10).audio_format, AudioFormat::Mp3);

        settings.compression_preference = CompressionPreference::Compatibility;
        assert_eq!(settings.options(10).image_format, ImageFormat::Jpeg);

        settings.compression_preference = CompressionPreference::Efficiency;
        settings.welcomed = true;
        let saved = serde_json::to_vec(&settings).unwrap();
        let reloaded: Settings = serde_json::from_slice(&saved).unwrap();
        let options = reloaded.options(10);
        assert!(reloaded.welcomed);
        assert_eq!(
            reloaded.compression_preference,
            CompressionPreference::Efficiency
        );
        assert_eq!(options.speed, Speed::Balanced);
        assert_eq!(options.video_codec, VideoCodec::H264);
        assert_eq!(options.audio_format, AudioFormat::Opus);
        assert_eq!(options.image_format, ImageFormat::Webp);
        assert!(options.gpu);
    }

    #[test]
    fn gpu_drawing_follows_efficiency_unless_set() {
        let mut settings: Settings = serde_json::from_str("{}").unwrap();
        assert!(!settings.gpu_drawing());
        settings.compression_preference = CompressionPreference::Efficiency;
        assert!(settings.gpu_drawing());
        settings.gpu_drawing = Some(false);
        assert!(!settings.gpu_drawing());
        settings.compression_preference = CompressionPreference::Balanced;
        settings.gpu_drawing = Some(true);
        assert!(settings.gpu_drawing());
    }

    #[test]
    fn efficiency_turns_gpu_encoding_on_but_never_locks_it() {
        let mut settings: Settings = serde_json::from_str("{}").unwrap();
        settings.gpu = false;
        settings.set_preference(CompressionPreference::Efficiency);
        assert!(settings.options(10).gpu);
        // Still the user's switch.
        settings.gpu = false;
        assert!(!settings.options(10).gpu);
        settings.set_preference(CompressionPreference::Efficiency);
        assert!(!settings.options(10).gpu);
        settings.set_preference(CompressionPreference::Compatibility);
        assert!(!settings.options(10).gpu);
    }

    #[test]
    fn upgrade_preserves_completed_onboarding_and_each_legacy_format() {
        let (mut settings, migrated) = parse(
            br#"{
            "welcomed": true,
            "videoCodec": "h265",
            "audioFormat": "mp3",
            "imageFormat": "jpeg",
            "archiveFormat": "7z"
        }"#,
        )
        .unwrap();
        assert!(migrated);
        assert!(settings.welcomed);
        assert_eq!(
            settings.compression_preference,
            CompressionPreference::Balanced
        );

        settings.compression_preference = CompressionPreference::Efficiency;
        let saved = serde_json::to_vec(&settings).unwrap();
        let (reloaded, migrated) = parse(&saved).unwrap();
        assert!(!migrated);
        let options = reloaded.options(10);
        assert!(reloaded.welcomed);
        assert_eq!(options.video_codec, VideoCodec::H265);
        assert_eq!(options.audio_format, AudioFormat::Mp3);
        assert_eq!(options.image_format, ImageFormat::Jpeg);
        assert_eq!(options.archive, ArchiveFormat::SevenZ);
    }

    #[test]
    fn legacy_defaults_follow_the_preference_after_upgrade() {
        // What every earlier build wrote for a user who never touched the formats.
        let (mut settings, migrated) = parse(
            br#"{
            "welcomed": true,
            "videoCodec": "h264",
            "audioFormat": "auto",
            "imageFormat": "auto",
            "archiveFormat": "zip"
        }"#,
        )
        .unwrap();
        assert!(migrated);
        assert_eq!(settings.video_codec, None);
        assert_eq!(settings.audio_format, None);
        assert_eq!(settings.image_format, None);
        assert_eq!(settings.archive_format, None);

        // So the preference now decides.
        settings.compression_preference = CompressionPreference::Efficiency;
        assert_eq!(settings.options(10).audio_format, AudioFormat::Opus);
        assert_eq!(settings.options(10).image_format, ImageFormat::Webp);
        settings.compression_preference = CompressionPreference::Compatibility;
        assert_eq!(settings.options(10).image_format, ImageFormat::Jpeg);

        // Mixed: defaults drop, real choices stay.
        let (settings, _) = parse(
            br#"{"videoCodec": "av1", "audioFormat": "auto", "imageFormat": "webp", "archiveFormat": "zip"}"#,
        )
        .unwrap();
        assert_eq!(settings.video_codec, Some(VideoCodec::Av1));
        assert_eq!(settings.audio_format, None);
        assert_eq!(settings.image_format, Some(ImageFormat::Webp));
        assert_eq!(settings.archive_format, None);

        // After the migration, an explicit default chosen in Settings is kept.
        let (settings, migrated) = parse(
            br#"{"compressionPreference": "efficiency", "videoCodec": "h264", "audioFormat": "auto"}"#,
        )
        .unwrap();
        assert!(!migrated);
        assert_eq!(settings.video_codec, Some(VideoCodec::H264));
        assert_eq!(settings.audio_format, Some(AudioFormat::Auto));
    }
}
