//! The few things that talk to the operating system directly: files on the clipboard,
//! the screen corner above the taskbar, the Recycle Bin, start with Windows, and making
//! sure only one Kompacter runs. Windows today; other systems get their own version later.

use std::path::{Path, PathBuf};

pub use imp::*;

/// Open a folder in the file manager.
pub fn open_folder(dir: &Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(dir).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(dir).spawn();
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
    let _ = std::process::Command::new("open")
        .arg("-R")
        .arg(file)
        .spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
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
    #[cfg(not(windows))]
    return config_dir();
}

#[cfg(windows)]
mod imp {
    use std::ffi::OsStr;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, GlobalFree, ERROR_ALREADY_EXISTS, POINT, RECT,
    };
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
        OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT,
    };
    use windows_sys::Win32::System::Registry::{
        RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, CreateMutexW, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE,
        INFINITE,
    };
    use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows_sys::Win32::UI::Shell::{
        DragQueryFileW, SHFileOperationW, DROPFILES, FOF_ALLOWUNDO, FOF_NOCONFIRMATION,
        FOF_NOERRORUI, FOF_SILENT, FO_DELETE, HDROP, SHFILEOPSTRUCTW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

    const CF_HDROP: u32 = 15;
    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
        s.as_ref().encode_wide().chain(Some(0)).collect()
    }

    /// The clipboard can be briefly held by another app; retry a few times.
    fn open_clipboard() -> bool {
        for _ in 0..10 {
            if unsafe { OpenClipboard(null_mut()) } != 0 {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        false
    }

    /// Files copied in Explorer (empty if the clipboard holds no files).
    pub fn clipboard_files() -> Vec<PathBuf> {
        let mut out = Vec::new();
        unsafe {
            if IsClipboardFormatAvailable(CF_HDROP) == 0 || !open_clipboard() {
                return out;
            }
            let h = GetClipboardData(CF_HDROP) as HDROP;
            if !h.is_null() {
                let n = DragQueryFileW(h, u32::MAX, null_mut(), 0);
                for i in 0..n {
                    let len = DragQueryFileW(h, i, null_mut(), 0) as usize;
                    let mut buf = vec![0u16; len + 1];
                    DragQueryFileW(h, i, buf.as_mut_ptr(), buf.len() as u32);
                    buf.truncate(len);
                    out.push(PathBuf::from(std::ffi::OsString::from_wide(&buf)));
                }
            }
            CloseClipboard();
        }
        out
    }

    /// Put files on the clipboard the way Explorer does, so Ctrl+V pastes the file itself
    /// (in Discord, a chat, or a folder).
    pub fn copy_files(paths: &[PathBuf]) -> Result<(), String> {
        let mut list: Vec<u16> = Vec::new();
        for p in paths {
            list.extend(p.as_os_str().encode_wide());
            list.push(0);
        }
        list.push(0);
        let header = std::mem::size_of::<DROPFILES>();
        unsafe {
            let h = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, header + list.len() * 2);
            if h.is_null() {
                return Err("out of memory".into());
            }
            let p = GlobalLock(h) as *mut u8;
            let df = p as *mut DROPFILES;
            (*df).pFiles = header as u32;
            (*df).fWide = 1;
            std::ptr::copy_nonoverlapping(list.as_ptr(), p.add(header) as *mut u16, list.len());
            GlobalUnlock(h);

            // "Copy, not move", for apps that paste files.
            let effect = GlobalAlloc(GMEM_MOVEABLE, 4);
            if !effect.is_null() {
                *(GlobalLock(effect) as *mut u32) = 1; // DROPEFFECT_COPY
                GlobalUnlock(effect);
            }

            if !open_clipboard() {
                GlobalFree(h);
                return Err("the clipboard is busy".into());
            }
            EmptyClipboard();
            let ok = !SetClipboardData(CF_HDROP, h).is_null();
            if !ok {
                GlobalFree(h);
            }
            let fmt = RegisterClipboardFormatW(wide("Preferred DropEffect").as_ptr());
            if !effect.is_null() && (fmt == 0 || SetClipboardData(fmt, effect).is_null()) {
                GlobalFree(effect);
            }
            CloseClipboard();
            if ok {
                Ok(())
            } else {
                Err("couldn't put the file on the clipboard".into())
            }
        }
    }

    /// Bottom-right corner of the free screen area (screen minus taskbar) on the monitor
    /// under the mouse, and that monitor's scale (1.0 = 100%). An auto-hiding taskbar
    /// doesn't count in the work area, so its space is kept free here too: the flyout
    /// always sits where it would sit above a visible taskbar.
    pub fn corner() -> Option<(i32, i32, f64)> {
        use windows_sys::Win32::UI::Shell::{
            SHAppBarMessage, ABE_BOTTOM, ABE_RIGHT, ABM_GETSTATE, ABM_GETTASKBARPOS, ABS_AUTOHIDE,
            APPBARDATA,
        };
        unsafe {
            let mut pt = POINT { x: 0, y: 0 };
            GetCursorPos(&mut pt);
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(mon, &mut info) == 0 {
                return None;
            }
            let RECT {
                mut right,
                mut bottom,
                ..
            } = info.rcWork;
            let (mut dx, mut dy) = (96u32, 96u32);
            GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            let scale = dx as f64 / 96.0;

            let mut abd: APPBARDATA = std::mem::zeroed();
            abd.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
            let auto = SHAppBarMessage(ABM_GETSTATE, &mut abd) as u32 & ABS_AUTOHIDE != 0;
            if auto && SHAppBarMessage(ABM_GETTASKBARPOS, &mut abd) != 0 {
                let r = abd.rc;
                let on_this = MonitorFromPoint(
                    POINT {
                        x: (r.left + r.right) / 2,
                        y: (r.top + r.bottom) / 2,
                    },
                    MONITOR_DEFAULTTONEAREST,
                ) == mon;
                let fallback = (48.0 * scale) as i32;
                if on_this && abd.uEdge == ABE_BOTTOM {
                    let h = (r.bottom - r.top).max(0);
                    let h = if h < 20 { fallback } else { h };
                    bottom = bottom.min(info.rcMonitor.bottom - h);
                } else if on_this && abd.uEdge == ABE_RIGHT {
                    let w = (r.right - r.left).max(0);
                    let w = if w < 20 { fallback } else { w };
                    right = right.min(info.rcMonitor.right - w);
                }
            }
            Some((right, bottom, scale))
        }
    }

    /// Mouse position on screen, in physical pixels.
    pub fn cursor() -> Option<(i32, i32)> {
        let mut pt = POINT { x: 0, y: 0 };
        (unsafe { GetCursorPos(&mut pt) } != 0).then_some((pt.x, pt.y))
    }

    /// Window see-through level (0 hidden … 255 solid), for the slide-in of the plain
    /// window. Makes the window "layered" the first time.
    pub fn set_alpha(hwnd: isize, alpha: u8) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowLongPtrW, GWL_EXSTYLE,
            LWA_ALPHA, WS_EX_LAYERED,
        };
        let h = hwnd as _;
        unsafe {
            let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
            if ex & WS_EX_LAYERED as isize == 0 {
                SetWindowLongPtrW(h, GWL_EXSTYLE, ex | WS_EX_LAYERED as isize);
            }
            SetLayeredWindowAttributes(h, 0, alpha, LWA_ALPHA);
        }
    }

    /// Windows 11 rounded corners for the plain (not floating) window, and no extra
    /// Windows show/hide animation on top of ours.
    pub fn style_plain_window(hwnd: isize) {
        use windows_sys::Win32::Graphics::Dwm::{
            DwmSetWindowAttribute, DWMWA_TRANSITIONS_FORCEDISABLED, DWMWA_WINDOW_CORNER_PREFERENCE,
            DWMWCP_ROUND,
        };
        unsafe {
            let round = DWMWCP_ROUND;
            DwmSetWindowAttribute(
                hwnd as _,
                DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                &round as *const _ as *const _,
                4,
            );
            let off: i32 = 1;
            DwmSetWindowAttribute(
                hwnd as _,
                DWMWA_TRANSITIONS_FORCEDISABLED as u32,
                &off as *const _ as *const _,
                4,
            );
        }
    }

    /// False when "Animation effects" is turned off in Windows settings.
    pub fn animations_enabled() -> bool {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION,
        };
        let mut on: i32 = 1;
        unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                &mut on as *mut i32 as *mut _,
                0,
            );
        }
        on != 0
    }

    /// Wait (up to a few seconds) for another process to exit; used when Kompacter
    /// restarts itself.
    pub fn wait_for_exit(pid: u32) {
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
        unsafe {
            let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
            if !h.is_null() {
                WaitForSingleObject(h, 5000);
                CloseHandle(h);
            }
        }
    }

    /// Move a file or folder to the Recycle Bin.
    pub fn recycle(path: &Path) -> Result<(), String> {
        let mut from = wide(path);
        from.push(0); // the API takes a double-terminated list
        let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
        op.wFunc = FO_DELETE;
        op.pFrom = from.as_ptr();
        op.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI) as u16;
        match unsafe { SHFileOperationW(&mut op) } {
            0 if op.fAnyOperationsAborted == 0 => Ok(()),
            code => Err(format!("error {code}")),
        }
    }

    /// Start Kompacter (quietly, in the tray) when Windows starts.
    pub fn set_start_with_windows(on: bool) {
        let key = wide(RUN_KEY);
        let name = wide("Kompacter");
        unsafe {
            if on {
                let Ok(exe) = std::env::current_exe() else {
                    return;
                };
                let cmd = wide(format!("\"{}\" --tray", exe.display()));
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    key.as_ptr(),
                    name.as_ptr(),
                    REG_SZ,
                    cmd.as_ptr() as *const _,
                    (cmd.len() * 2) as u32,
                );
            } else {
                RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr());
            }
        }
    }

    /// Whether the start-with-Windows entry exists (it can be removed in Task Manager).
    pub fn starts_with_windows() -> bool {
        let key = wide(RUN_KEY);
        let name = wide("Kompacter");
        unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                null_mut(),
                null_mut(),
                null_mut(),
            ) == 0
        }
    }

    /// Windows app theme: true when apps are set to dark.
    pub fn system_dark() -> bool {
        let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
        let name = wide("AppsUseLightTheme");
        let mut value: u32 = 1;
        let mut size = 4u32;
        unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                0x0000_0010, // RRF_RT_REG_DWORD
                null_mut(),
                &mut value as *mut u32 as *mut _,
                &mut size,
            );
        }
        value == 0
    }

    /// Videos folder (follows OneDrive or other redirection).
    pub fn videos_dir() -> Option<PathBuf> {
        use windows_sys::Win32::System::Com::CoTaskMemFree;
        use windows_sys::Win32::UI::Shell::{FOLDERID_Videos, SHGetKnownFolderPath};
        unsafe {
            let mut p: *mut u16 = null_mut();
            if SHGetKnownFolderPath(&FOLDERID_Videos, 0, null_mut(), &mut p) != 0 || p.is_null() {
                return None;
            }
            let len = (0..).take_while(|&i| *p.add(i) != 0).count();
            let path = PathBuf::from(std::ffi::OsString::from_wide(std::slice::from_raw_parts(
                p, len,
            )));
            CoTaskMemFree(p as *const _);
            Some(path)
        }
    }

    const SHOW_EVENT: &str = "Local\\KompacterShow";

    /// True if this is the only Kompacter running. If another one is already running,
    /// asks it to open its window and returns false.
    pub fn first_instance() -> bool {
        unsafe {
            let m = CreateMutexW(null(), 0, wide("Local\\KompacterRunning").as_ptr());
            if !m.is_null() && GetLastError() == ERROR_ALREADY_EXISTS {
                let e = OpenEventW(EVENT_MODIFY_STATE, 0, wide(SHOW_EVENT).as_ptr());
                if !e.is_null() {
                    SetEvent(e);
                    CloseHandle(e);
                }
                return false;
            }
            // The mutex stays open for the life of the process.
            true
        }
    }

    /// Calls `on_show` whenever a second launch asks this one to open its window.
    pub fn listen_for_show(on_show: impl Fn() + Send + 'static) {
        let e = unsafe { CreateEventW(null(), 0, 0, wide(SHOW_EVENT).as_ptr()) };
        if e.is_null() {
            return;
        }
        let e = e as usize;
        std::thread::spawn(move || loop {
            if unsafe { WaitForSingleObject(e as _, INFINITE) } != 0 {
                return;
            }
            on_show();
        });
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::{Path, PathBuf};

    pub fn clipboard_files() -> Vec<PathBuf> {
        Vec::new()
    }
    pub fn copy_files(_paths: &[PathBuf]) -> Result<(), String> {
        Err("not supported on this system yet".into())
    }
    pub fn corner() -> Option<(i32, i32, f64)> {
        None
    }
    pub fn cursor() -> Option<(i32, i32)> {
        None
    }
    pub fn set_alpha(_hwnd: isize, _alpha: u8) {}
    pub fn style_plain_window(_hwnd: isize) {}
    pub fn animations_enabled() -> bool {
        true
    }
    pub fn wait_for_exit(_pid: u32) {
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    pub fn recycle(_path: &Path) -> Result<(), String> {
        Err("not supported on this system yet".into())
    }
    pub fn set_start_with_windows(_on: bool) {}
    pub fn starts_with_windows() -> bool {
        false
    }
    pub fn system_dark() -> bool {
        false
    }
    pub fn videos_dir() -> Option<PathBuf> {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Videos"))
    }
    pub fn first_instance() -> bool {
        true
    }
    pub fn listen_for_show(_on_show: impl Fn() + Send + 'static) {}
}
