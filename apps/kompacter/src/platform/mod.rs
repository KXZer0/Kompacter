//! The few things that talk to the operating system directly: files on the clipboard,
//! the screen corner above the taskbar, the Recycle Bin, start with Windows, making sure
//! only one Kompacter runs, the global shortcut and the paste guard. Each system has its
//! own module; what one system lacks has a stub in `imp`.

use std::path::{Path, PathBuf};

mod capabilities;
pub use capabilities::{watch_copies_support, CAPABILITIES, TRASH_NAME};

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub use linux::{clipboard_files, copy_files, recycle};

#[cfg(target_os = "linux")]
#[path = "linux/autostart.rs"]
mod linux_autostart;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod macos_login;
#[cfg(target_os = "macos")]
pub use macos::{
    clipboard_files, clipboard_seq, copy_files, focus_application, install_activation, recycle,
    style_panel,
};
#[cfg(target_os = "macos")]
pub use macos_login::{activate as activate_instance, another_instance};
#[cfg(target_os = "macos")]
pub mod macos_watch;

#[cfg(any(target_os = "macos", test))]
mod macos_input;
#[cfg(target_os = "macos")]
pub use macos_input::{mac_shortcut_label, send_paste, set_guard, set_shortcut, start_input};

#[cfg(target_os = "linux")]
pub use linux::{set_shortcut, start_input};

#[cfg(windows)]
mod windows;

#[cfg(windows)]
use windows::preview_audio;
#[cfg(windows)]
pub use windows::*;

#[cfg(not(windows))]
pub use imp::*;

/// Where the panel opens: the free screen area beside the tray or Dock (left, top,
/// right, bottom, in physical pixels), the monitor's scale (1.0 = 100%), and which side
/// of it the panel sits against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    pub free: (i32, i32, i32, i32),
    pub scale: f64,
    pub at: At,
}

// Each system uses only the placements its taskbar or Dock can produce.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum At {
    /// Rising from a taskbar at the bottom right (Windows, most Linux panels).
    BottomRight,
    /// Hanging from a top bar at the top right (GNOME, Xfce).
    TopRight,
    /// Right above a Dock icon at the bottom, centred on this x.
    Above(i32),
    /// Beside a Dock icon on the left or right edge, centred on this y.
    RightOf(i32),
    LeftOf(i32),
}

/// The panel's anchor, or `None` where the system places windows itself (Wayland, macOS).
pub fn anchor() -> Option<Anchor> {
    #[cfg(target_os = "linux")]
    return linux::anchor();
    #[cfg(target_os = "macos")]
    return macos::anchor();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return corner().map(|(right, bottom, scale)| Anchor {
        free: (i32::MIN / 2, i32::MIN / 2, right, bottom),
        scale,
        at: At::BottomRight,
    });
}

/// Kompacter's entry among the programs the system starts at login.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartEntry {
    /// No entry.
    Absent,
    /// An entry the user switched off in the system's own settings: left that way.
    Disabled,
    /// An entry that starts another copy of Kompacter (or this one before it moved).
    Elsewhere,
    /// An entry that starts this copy.
    Here,
}

/// Start Kompacter quietly (in the tray) when the user logs in, or stop doing so: the
/// Run key on Windows, a login item on macOS, an XDG autostart entry on Linux.
pub fn set_start_with_system(on: bool) {
    #[cfg(windows)]
    windows::set_start_with_windows(on);
    #[cfg(target_os = "macos")]
    macos_login::set(on);
    #[cfg(target_os = "linux")]
    linux_autostart::set(on);
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    let _ = on;
}

/// Kompacter's start-at-login entry as it is now (it can be changed in the system's
/// settings: Task Manager, System Settings, the desktop's startup apps).
pub fn start_entry() -> StartEntry {
    #[cfg(windows)]
    return if windows::starts_with_windows() {
        StartEntry::Here
    } else {
        StartEntry::Absent
    };
    #[cfg(target_os = "macos")]
    return macos_login::state();
    #[cfg(target_os = "linux")]
    return linux_autostart::state();
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    return StartEntry::Absent;
}

/// Whether this copy can start with the system: macOS only registers the app bundle.
pub fn start_with_system_supported() -> bool {
    #[cfg(target_os = "macos")]
    return CAPABILITIES.start_with_system && macos_login::supported();
    #[cfg(not(target_os = "macos"))]
    return CAPABILITIES.start_with_system;
}

/// What the system calls starting with it, for the Settings switch.
pub const START_WITH_SYSTEM_NAME: &str = if cfg!(windows) {
    "Start with Windows"
} else if cfg!(target_os = "macos") {
    "Open at login"
} else {
    "Start with the system"
};

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

/// Keyboard events from outside Kompacter: the global shortcut (Windows, macOS, Linux
/// through the desktop portal) and the paste guard (Windows, macOS).
#[derive(Debug, PartialEq)]
pub enum InputEvent {
    /// The global shortcut was pressed.
    Shortcut,
    /// Ctrl+V (Cmd+V on macOS) in a chat app while the clipboard holds files over that
    /// app's limit. The paste was held back; `target` is the chat app, to paste into
    /// later, and `app` the chat app's preset id ("discord", "slack", ...).
    #[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
    Guard {
        files: Vec<PathBuf>,
        target: PasteTarget,
        app: &'static str,
    },
    /// Whether the shortcut is taken by another app (true) or registered / off (false).
    ShortcutTaken(bool),
    /// This desktop can't give Kompacter a global shortcut (Linux without the global
    /// shortcuts portal): the user sets one up in the system settings instead.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    ShortcutUnsupported,
    /// The paste guard is on but waits for a permission (macOS: Accessibility), or
    /// no longer does.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    GuardNeedsPermission(bool),
}

/// Where a held-back paste goes once the files are small enough: the chat app that was
/// in front when the paste guard caught the paste.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteTarget {
    /// A window handle (an HWND on Windows).
    #[cfg_attr(not(windows), allow(dead_code))]
    Window(isize),
    /// An application, by process id (macOS).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    App(i32),
}

/// The paste target in front right now (None: unknown on this system).
pub fn front_target() -> Option<PasteTarget> {
    #[cfg(windows)]
    return Some(PasteTarget::Window(windows::foreground_window()));
    #[cfg(target_os = "macos")]
    return macos_input::front_app().map(PasteTarget::App);
    #[cfg(not(any(windows, target_os = "macos")))]
    return None;
}

/// Bring the paste target back to the front.
pub fn focus_target(target: PasteTarget) {
    match target {
        #[cfg(windows)]
        PasteTarget::Window(w) => windows::focus_window(w),
        #[cfg(target_os = "macos")]
        PasteTarget::App(pid) => macos_input::activate_app(pid),
        #[allow(unreachable_patterns)]
        _ => {}
    }
}

/// `target` is in front, so a paste goes there and nowhere else.
pub fn is_front(target: PasteTarget) -> bool {
    front_target() == Some(target)
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
    chat_tab(title)
}

/// The web chat app a browser tab belongs to, by its window title. Titles look like
/// "#general | Server - Discord - Google Chrome".
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub fn chat_tab(title: &str) -> Option<&'static str> {
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
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
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
    pub fn set_explorer_menu(_on: bool) {}
    pub fn tray_pinned() -> Option<bool> {
        None
    }
    pub fn set_tray_pinned(_on: bool) -> bool {
        false
    }
    pub fn open_taskbar_settings() {}
    pub fn system_dark() -> bool {
        false
    }
    pub fn videos_dir() -> Option<PathBuf> {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Videos"))
    }
    #[cfg(not(target_os = "macos"))]
    pub fn set_guard(_on: bool, _discord_limit: u64) {}
    #[cfg(not(target_os = "macos"))]
    pub fn send_paste() {}
    // macOS and Linux: the watch reads copies from their own watchers instead.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
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
    fn a_held_paste_goes_back_only_to_its_own_target() {
        // A window handle and a process id never stand for each other.
        assert_ne!(PasteTarget::Window(42), PasteTarget::App(42));
        assert_eq!(PasteTarget::App(7), PasteTarget::App(7));
        // Where the system can't tell what's in front, nothing is pasted anywhere.
        if cfg!(not(any(windows, target_os = "macos"))) {
            assert_eq!(front_target(), None);
            assert!(!is_front(PasteTarget::Window(1)));
            assert!(!is_front(PasteTarget::App(1)));
        }
        let held = InputEvent::Guard {
            files: vec![PathBuf::from("/clips/big.mp4")],
            target: PasteTarget::App(501),
            app: "discord",
        };
        assert!(matches!(
            held,
            InputEvent::Guard {
                target: PasteTarget::App(501),
                app: "discord",
                ..
            }
        ));
    }

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
