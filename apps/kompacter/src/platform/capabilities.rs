/// Automatic features for which the app has an operating-system backend.
pub struct Capabilities {
    pub watch_copies: bool,
    pub paste_guard: bool,
    pub watch_folders: bool,
    pub global_shortcut: bool,
    /// Files can go to the Recycle Bin or Trash ("Replace the original" needs it).
    pub trash: bool,
}

pub const CAPABILITIES: Capabilities = Capabilities {
    watch_copies: cfg!(windows),
    paste_guard: cfg!(windows),
    watch_folders: cfg!(any(windows, target_os = "linux", target_os = "macos")),
    global_shortcut: cfg!(windows),
    trash: cfg!(any(windows, target_os = "linux", target_os = "macos")),
};

/// What the system calls the place deleted files go.
pub const TRASH_NAME: &str = if cfg!(windows) {
    "Recycle Bin"
} else {
    "Trash"
};
