/// Automatic features for which the app has an operating-system backend.
pub struct Capabilities {
    pub watch_copies: bool,
    pub paste_guard: bool,
    pub watch_folders: bool,
    pub global_shortcut: bool,
}

pub const CAPABILITIES: Capabilities = Capabilities {
    watch_copies: cfg!(windows),
    paste_guard: cfg!(windows),
    watch_folders: cfg!(any(windows, target_os = "linux", target_os = "macos")),
    global_shortcut: cfg!(windows),
};
