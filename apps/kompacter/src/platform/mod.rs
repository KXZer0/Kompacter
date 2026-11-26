//! The few things that talk to the operating system directly: files on the clipboard,
//! the screen corner above the taskbar, the Recycle Bin, start with Windows, making sure
//! only one Kompacter runs, the global shortcut and the paste guard. Windows today; other systems get their own version later.

use std::path::{Path, PathBuf};

mod capabilities;
pub use capabilities::{CAPABILITIES, TRASH_NAME};

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub use linux::{clipboard_files, copy_files, recycle};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{
    clipboard_files, clipboard_seq, copy_files, focus_application, install_activation, recycle,
};

#[cfg(windows)]
mod windows;

#[cfg(windows)]
use windows::preview_audio;
#[cfg(windows)]
pub use windows::*;

#[cfg(not(windows))]
pub use imp::*;

/// Play a preview against the sound device's sample clock. If audio is
/// unavailable, start a monotonic clock when video has its first frame.
pub fn play_preview(
    engine: &kompacter_core::Engine,
    info: &kompacter_core::MediaInfo,
    range: std::ops::Range<f64>,
    stop: &std::sync::atomic::AtomicBool,
    sound: &std::sync::atomic::AtomicBool,
    clock: &std::sync::atomic::AtomicU64,
    on_frame: &mut dyn FnMut(f64, u32, u32, Vec<u8>) -> bool,
) -> kompacter_core::Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    use std::time::{Duration, Instant};

    let ready = AtomicBool::new(false);
    let audio_stop = AtomicBool::new(false);
    let audio_running = AtomicBool::new(false);
    let audio_done = AtomicBool::new(!cfg!(windows) || info.audio.is_none());
    let fps = info.video.as_ref().map_or(24.0, |video| {
        let pixels = u64::from(video.width) * u64::from(video.height);
        let max_fps = if pixels > 1920 * 1080 { 24.0 } else { 30.0 };
        video.fps.clamp(12.0, max_fps)
    });
    let mut last_audio = None;
    let mut fallback = None;
    std::thread::scope(|scope| {
        #[cfg(windows)]
        if info.audio.is_some() {
            scope.spawn(|| {
                preview_audio::play(
                    engine,
                    info,
                    range.clone(),
                    preview_audio::Control {
                        ready: &ready,
                        running: &audio_running,
                        stop: &audio_stop,
                        sound,
                        clock,
                    },
                );
                audio_done.store(true, Relaxed);
            });
        }
        #[cfg(not(windows))]
        let _ = (scope, sound);

        let result = engine.play_preview(
            info,
            range.start,
            range.end,
            // The viewport is 320×150 logical px. Keep headroom for DPI scaling
            // while avoiding a full 360p RGB upload for every playback frame.
            240,
            fps,
            stop,
            &mut || {
                let position = clock.load(Relaxed);
                let (previous, changed) = last_audio.get_or_insert_with(|| {
                    ready.store(true, Relaxed);
                    (position, Instant::now())
                });
                if *previous != position {
                    *previous = position;
                    *changed = Instant::now();
                }
                if !audio_done.load(Relaxed) && fallback.is_none() {
                    if changed.elapsed() < Duration::from_millis(500) {
                        return audio_running.load(Relaxed).then_some(position as f64 / 1e6);
                    }
                    audio_stop.store(true, Relaxed);
                }
                let (base, began) = fallback.get_or_insert_with(|| (position, Instant::now()));
                let position = *base + began.elapsed().as_micros() as u64;
                clock.store(position, Relaxed);
                Some(position as f64 / 1e6)
            },
            on_frame,
        );
        audio_stop.store(true, Relaxed);
        result
    })
}

/// Keyboard events from outside Kompacter (Windows).
#[cfg(windows)]
pub enum InputEvent {
    /// The global shortcut was pressed.
    Shortcut,
    /// Ctrl+V in a chat app while the clipboard holds files over that app's limit. The
    /// paste was held back; `window` is the chat window, to paste into later, and `app`
    /// the chat app's preset id ("discord", "slack", ...).
    Guard {
        files: Vec<PathBuf>,
        window: isize,
        app: &'static str,
    },
    /// Whether the shortcut is taken by another app (true) or registered / off (false).
    ShortcutTaken(bool),
}

/// The chat app a window belongs to, as its preset id: by its program, or for a browser,
/// by the page title (web Discord, Slack, WhatsApp, Telegram). `None`: not a chat app.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn chat_app(exe: &str, title: &str) -> Option<&'static str> {
    let exe = exe.to_ascii_lowercase();
    let exe = exe.strip_suffix(".exe").unwrap_or(&exe);
    match exe {
        // Discord, DiscordPTB, DiscordCanary.
        e if e.starts_with("discord") => return Some("discord"),
        "slack" => return Some("slack"),
        // The Store app and the newer desktop app.
        "whatsapp" | "whatsapp.root" => return Some("whatsapp"),
        "telegram" => return Some("telegram"),
        "signal" => return Some("signal"),
        _ => {}
    }
    const BROWSERS: [&str; 8] = [
        "chrome", "msedge", "firefox", "brave", "opera", "vivaldi", "arc", "zen",
    ];
    if !BROWSERS.contains(&exe) {
        return None;
    }
    // Tab titles like "#general | Server - Discord - Google Chrome".
    [
        ("Discord", "discord"),
        ("Slack", "slack"),
        ("WhatsApp", "whatsapp"),
        ("Telegram", "telegram"),
    ]
    .into_iter()
    .find(|(name, _)| title.contains(name))
    .map(|(_, id)| id)
}

/// Where a second launch leaves the files it was started with ("Kompact" in Explorer,
/// "Open with"), for the running Kompacter to pick up. One path per line.
#[cfg(any(windows, test))]
fn handoff_file() -> PathBuf {
    std::env::temp_dir().join("kompacter-open.txt")
}

/// Leave `files` for the running Kompacter. Appends, because Explorer starts one launch
/// per selected file and they all arrive within a moment of each other.
#[cfg(any(windows, test))]
pub fn leave_handoff(files: &[PathBuf]) {
    use std::io::Write;
    if files.is_empty() {
        return;
    }
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(handoff_file())
    else {
        return;
    };
    let mut text = String::new();
    for p in files {
        text.push_str(&p.to_string_lossy());
        text.push('\n');
    }
    // One write per launch, so lines from two launches never interleave.
    let _ = f.write_all(text.as_bytes());
}

/// Take the files other launches left (empty if none).
#[cfg(any(windows, test))]
pub fn take_handoff() -> Vec<PathBuf> {
    let file = handoff_file();
    // Move it aside first, so a launch that writes right now starts a fresh file.
    let mine = file.with_extension(format!("{}.txt", std::process::id()));
    if std::fs::rename(&file, &mine).is_err() {
        return Vec::new();
    }
    let text = std::fs::read_to_string(&mine).unwrap_or_default();
    let _ = std::fs::remove_file(&mine);
    let mut files: Vec<PathBuf> = Vec::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let p = PathBuf::from(line);
        if !files.contains(&p) {
            files.push(p);
        }
    }
    files
}

/// The files or folders named on a command line (anything that isn't a `--switch` and
/// really exists), as full paths.
#[cfg(any(windows, target_os = "macos", test))]
pub fn file_args(args: impl Iterator<Item = String>) -> Vec<PathBuf> {
    args.skip(1)
        .filter(|a| !a.starts_with("--"))
        .filter_map(|a| std::path::absolute(&a).ok())
        .filter(|p| p.exists())
        .collect()
}

/// Open a folder in the file manager.
pub fn open_folder(dir: &Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(dir).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("/usr/bin/open").arg(dir).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
}

/// Open the file's folder with the file selected.
pub fn show_in_folder(file: &Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer")
        .arg(format!("/select,{}", file.display()))
        .spawn();
    #[cfg(target_os = "macos")]
    macos::show_in_folder(file);
    #[cfg(target_os = "linux")]
    linux::show_in_folder(file);
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
    open_folder(file.parent().unwrap_or(Path::new(".")));
}

/// ffmpeg and ffprobe: next to Kompacter if bundled, otherwise from the PATH.
pub fn ffmpeg() -> kompacter_core::Ffmpeg {
    let beside = |name: &str| {
        let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join(&file)))
            .filter(|p| p.is_file())
    };
    match (beside("ffmpeg"), beside("ffprobe")) {
        (Some(ffmpeg), Some(ffprobe)) => kompacter_core::Ffmpeg { ffmpeg, ffprobe },
        _ => kompacter_core::Ffmpeg::default(),
    }
}

/// Roaming settings folder (%APPDATA% on Windows).
pub fn config_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    return std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    return std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"));
    #[cfg(all(unix, not(target_os = "macos")))]
    return std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
}

/// Local (not roaming) app data folder.
pub fn local_data_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    return std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    return config_dir();
    #[cfg(all(unix, not(target_os = "macos")))]
    return std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
}

#[cfg(not(windows))]
mod imp {
    use std::path::{Path, PathBuf};

    #[cfg(not(target_os = "macos"))]
    #[cfg(not(target_os = "linux"))]
    pub fn clipboard_files() -> Vec<PathBuf> {
        Vec::new()
    }
    #[cfg(not(target_os = "macos"))]
    #[cfg(not(target_os = "linux"))]
    pub fn copy_files(_paths: &[PathBuf]) -> Result<(), String> {
        // Dev builds: the README demos are recorded on Linux and show what Windows does.
        if cfg!(debug_assertions) && std::env::var_os("KOMPACTER_DEMO_COPY").is_some() {
            return Ok(());
        }
        Err("not supported on this system yet".into())
    }
    pub fn corner() -> Option<(i32, i32, f64)> {
        None
    }
    pub fn cursor() -> Option<(i32, i32)> {
        None
    }
    pub fn file_icon(_path: &Path) -> Option<(u32, u32, Vec<u8>)> {
        None
    }
    pub fn style_plain_window(_hwnd: isize) {}
    pub fn animations_enabled() -> bool {
        true
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn recycle(_path: &Path) -> Result<(), String> {
        Err("not supported on this system yet".into())
    }
    pub fn left_mouse_down() -> bool {
        false
    }
    pub fn resize_flyout(_hwnd: isize, _x: i32, _y: i32, _width: i32, _height: i32) -> bool {
        false
    }
    pub fn clip_flyout_input(_hwnd: isize, _height: f32, _scale: f32) -> bool {
        false
    }
    pub fn set_start_with_windows(_on: bool) {}
    pub fn set_explorer_menu(_on: bool) {}
    pub fn tray_pinned() -> Option<bool> {
        None
    }
    pub fn set_tray_pinned(_on: bool) -> bool {
        false
    }
    pub fn open_taskbar_settings() {}
    pub fn starts_with_windows() -> bool {
        false
    }
    pub fn system_dark() -> bool {
        false
    }
    pub fn videos_dir() -> Option<PathBuf> {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Videos"))
    }
    pub fn set_shortcut(_key: Option<(u32, u32)>) {}
    pub fn set_guard(_on: bool, _discord_limit: u64) {}
    pub fn focus_window(_window: isize) {}
    pub fn is_foreground(_window: isize) -> bool {
        true
    }
    pub fn send_paste() {}
    #[cfg(not(target_os = "macos"))]
    pub fn clipboard_seq() -> u32 {
        0
    }
    pub fn fullscreen_in_front() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_chat_apps_by_program_or_tab() {
        use super::chat_app;
        assert_eq!(chat_app("Discord.exe", ""), Some("discord"));
        assert_eq!(chat_app("DiscordPTB.exe", ""), Some("discord"));
        assert_eq!(chat_app("slack.exe", "general - Team"), Some("slack"));
        assert_eq!(chat_app("WhatsApp.Root.exe", ""), Some("whatsapp"));
        assert_eq!(chat_app("Telegram.exe", ""), Some("telegram"));
        assert_eq!(chat_app("Signal.exe", ""), Some("signal"));
        assert_eq!(
            chat_app("chrome.exe", "#general | Server - Discord - Google Chrome"),
            Some("discord")
        );
        assert_eq!(
            chat_app("msedge.exe", "(3) WhatsApp - Microsoft Edge"),
            Some("whatsapp")
        );
        // A document that mentions Discord is not Discord.
        assert_eq!(chat_app("WINWORD.EXE", "Discord notes.docx - Word"), None);
        assert_eq!(chat_app("explorer.exe", ""), None);
    }

    use super::*;

    #[test]
    fn files_left_by_several_launches_arrive_once_each() {
        let _ = take_handoff();
        let (a, b) = (PathBuf::from("/clips/a.mp4"), PathBuf::from("/clips/b.mp4"));
        // Explorer: one launch per selected file, and one file selected twice.
        leave_handoff(std::slice::from_ref(&a));
        leave_handoff(&[b.clone(), a.clone()]);
        leave_handoff(&[]);
        assert_eq!(take_handoff(), vec![a, b]);
        assert!(take_handoff().is_empty());
    }

    #[test]
    fn only_real_paths_count_as_files_to_open() {
        let dir = std::env::temp_dir();
        let args = ["Kompacter", "--tray", "no-such-file.mp4"]
            .into_iter()
            .map(String::from)
            .chain(Some(dir.display().to_string()));
        assert_eq!(file_args(args), vec![dir]);
    }
}
