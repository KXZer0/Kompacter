/// Automatic features for which the app has an operating-system backend.
pub struct Capabilities {
    pub watch_copies: bool,
    pub paste_guard: bool,
    pub watch_folders: bool,
    pub global_shortcut: bool,
    /// Files can go to the Recycle Bin or Trash ("Replace the original" needs it).
    pub trash: bool,
    /// Start at login (Run key, login item, XDG autostart).
    pub start_with_system: bool,
}

pub const CAPABILITIES: Capabilities = Capabilities {
    watch_copies: cfg!(any(windows, target_os = "linux", target_os = "macos")),
    // macOS: only once Accessibility is allowed (Settings says so). Wayland has no way to
    // see or hold back a paste in another app.
    paste_guard: cfg!(any(windows, target_os = "macos")),
    watch_folders: cfg!(any(windows, target_os = "linux", target_os = "macos")),
    // Linux: through the desktop's global shortcuts portal, checked when the app starts;
    // without it `InputEvent::ShortcutUnsupported` makes Settings explain the alternative.
    global_shortcut: cfg!(any(windows, target_os = "macos", target_os = "linux")),
    trash: cfg!(any(windows, target_os = "linux", target_os = "macos")),
    start_with_system: cfg!(any(windows, target_os = "linux", target_os = "macos")),
};

/// Whether copies can be noticed in this session (Linux: only on Wayland desktops with a
/// data-control protocol). `Err` says why not, in a few words, for the settings.
pub fn watch_copies_support() -> Result<(), &'static str> {
    #[cfg(target_os = "linux")]
    return super::linux::clipboard_watch::support();
    #[cfg(not(target_os = "linux"))]
    return if CAPABILITIES.watch_copies {
        Ok(())
    } else {
        Err("Not available on this system")
    };
}

/// What the system calls the place deleted files go.
pub const TRASH_NAME: &str = if cfg!(windows) {
    "Recycle Bin"
} else {
    "Trash"
};
