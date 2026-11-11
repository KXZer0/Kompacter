//! The few things that talk to the operating system directly: files on the clipboard,
//! the screen corner above the taskbar, the Recycle Bin, start with Windows, making sure
//! only one Kompacter runs, the global shortcut and the paste guard. Windows today; other systems get their own version later.

use std::path::{Path, PathBuf};

#[cfg(windows)]
mod preview_audio;

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
pub enum InputEvent {
    /// The global shortcut was pressed.
    Shortcut,
    /// Ctrl+V in Discord while the clipboard holds files over the limit. The paste was
    /// held back; `window` is the Discord window, to paste into later.
    Guard { files: Vec<PathBuf>, window: isize },
    /// Whether the shortcut is taken by another app (true) or registered / off (false).
    ShortcutTaken(bool),
}

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

    /// The icon Windows shows for this file or folder in Explorer (48 px), as straight RGBA.
    pub fn file_icon(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
        use windows_sys::Win32::Foundation::RPC_E_CHANGED_MODE;
        use windows_sys::Win32::System::Com::{
            CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED,
        };

        let hr = unsafe { CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32) };
        if hr < 0 && hr != RPC_E_CHANGED_MODE {
            return None;
        }
        let result = shell_icon(path);
        if hr >= 0 {
            unsafe { CoUninitialize() };
        }
        result
    }

    fn shell_icon(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
        use windows_sys::core::{IUnknown_Vtbl, GUID};
        use windows_sys::Win32::Graphics::Gdi::{
            CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO,
            BITMAPINFOHEADER, DIB_RGB_COLORS,
        };
        use windows_sys::Win32::UI::Controls::{ImageList_GetIcon, ILD_TRANSPARENT};
        use windows_sys::Win32::UI::Shell::{
            SHGetFileInfoW, SHGetImageList, SHFILEINFOW, SHGFI_SYSICONINDEX, SHIL_EXTRALARGE,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO};

        // IImageList
        const IID: GUID = GUID::from_u128(0x46eb5926_582e_4017_9fdf_e8998daa0950);
        unsafe {
            let mut fi: SHFILEINFOW = std::mem::zeroed();
            let w = wide(path);
            if SHGetFileInfoW(
                w.as_ptr(),
                0,
                &mut fi,
                std::mem::size_of::<SHFILEINFOW>() as u32,
                SHGFI_SYSICONINDEX,
            ) == 0
            {
                return None;
            }
            let mut list: *mut std::ffi::c_void = null_mut();
            if SHGetImageList(SHIL_EXTRALARGE as i32, &IID, &mut list) != 0 || list.is_null() {
                return None;
            }
            let icon = ImageList_GetIcon(list as _, fi.iIcon, ILD_TRANSPARENT);
            let vtable = *list.cast::<*const IUnknown_Vtbl>();
            ((*vtable).Release)(list);
            if icon.is_null() {
                return None;
            }
            let mut ii: ICONINFO = std::mem::zeroed();
            let ok = GetIconInfo(icon, &mut ii) != 0;
            let out = if ok && !ii.hbmColor.is_null() {
                let mut bm: BITMAP = std::mem::zeroed();
                GetObjectW(
                    ii.hbmColor,
                    std::mem::size_of::<BITMAP>() as i32,
                    &mut bm as *mut _ as *mut _,
                );
                let (iw, ih) = (bm.bmWidth.max(1) as u32, bm.bmHeight.unsigned_abs().max(1));
                let mut bmi: BITMAPINFO = std::mem::zeroed();
                bmi.bmiHeader = BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: iw as i32,
                    biHeight: -(ih as i32), // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    ..std::mem::zeroed()
                };
                let mut px = vec![0u8; (iw * ih * 4) as usize];
                let dc = CreateCompatibleDC(null_mut());
                let got = GetDIBits(
                    dc,
                    ii.hbmColor,
                    0,
                    ih,
                    px.as_mut_ptr() as *mut _,
                    &mut bmi,
                    DIB_RGB_COLORS,
                );
                DeleteDC(dc);
                // BGRA → RGBA. Old-style icons have no alpha: then everything is opaque.
                let has_alpha = px.as_chunks::<4>().0.iter().any(|p| p[3] != 0);
                for p in px.as_chunks_mut::<4>().0 {
                    p.swap(0, 2);
                    if !has_alpha {
                        p[3] = 255;
                    }
                }
                (got != 0).then_some((iw, ih, px))
            } else {
                None
            };
            if !ii.hbmColor.is_null() {
                DeleteObject(ii.hbmColor);
            }
            if !ii.hbmMask.is_null() {
                DeleteObject(ii.hbmMask);
            }
            DestroyIcon(icon);
            out
        }
    }

    /// Resize and re-anchor the flyout in one Windows operation.
    pub fn resize_flyout(hwnd: isize, x: i32, y: i32, width: i32, height: i32) -> bool {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetClientRect, GetWindowRect, SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER,
        };
        unsafe {
            let hwnd = hwnd as _;
            let mut outer: RECT = std::mem::zeroed();
            let mut client: RECT = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut outer) == 0 || GetClientRect(hwnd, &mut client) == 0 {
                return false;
            }
            if outer.left == x && outer.top == y && client.right == width && client.bottom == height
            {
                return true;
            }
            SetWindowPos(
                hwnd,
                null_mut(),
                x,
                y,
                width + (outer.right - outer.left) - (client.right - client.left),
                height + (outer.bottom - outer.top) - (client.bottom - client.top),
                SWP_NOACTIVATE | SWP_NOZORDER,
            ) != 0
        }
    }

    /// Keep the transparent area above the visible card out of hit testing.
    pub fn clip_flyout_input(hwnd: isize, height: f32, scale: f32) -> bool {
        use windows_sys::Win32::Graphics::Gdi::{CreateRectRgn, DeleteObject, SetWindowRgn};
        use windows_sys::Win32::UI::Shell::SetWindowSubclass;
        use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect;
        unsafe {
            // Winit retains native frame styles for window management. With a
            // window region, DefWindowProc paints a classic frame on focus loss.
            if SetWindowSubclass(hwnd as _, Some(flyout_frame_proc), 0x4b4f4d50, 0) == 0 {
                return false;
            }
            let mut rect: RECT = std::mem::zeroed();
            if GetWindowRect(hwnd as _, &mut rect) == 0 {
                return false;
            }
            let width = rect.right - rect.left;
            let bottom = rect.bottom - rect.top;
            let top = (bottom - (height * scale).ceil() as i32).max(0);
            let region = CreateRectRgn(0, top, width, bottom);
            if region.is_null() {
                return false;
            }
            if SetWindowRgn(hwnd as _, region, 1) == 0 {
                DeleteObject(region);
                return false;
            }
            // Windows owns the region after a successful SetWindowRgn.
            true
        }
    }

    unsafe extern "system" fn flyout_frame_proc(
        hwnd: windows_sys::Win32::Foundation::HWND,
        message: u32,
        wparam: usize,
        lparam: isize,
        id: usize,
        _data: usize,
    ) -> isize {
        use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            WM_NCACTIVATE, WM_NCDESTROY, WM_NCPAINT,
        };
        match message {
            // Forward activation to Winit, but tell DefWindowProc to skip the
            // native frame repaint. Slint paints the flyout's border itself.
            WM_NCACTIVATE => unsafe { DefSubclassProc(hwnd, message, wparam, -1) },
            WM_NCPAINT => 0,
            WM_NCDESTROY => unsafe {
                RemoveWindowSubclass(hwnd, Some(flyout_frame_proc), id);
                DefSubclassProc(hwnd, message, wparam, lparam)
            },
            _ => unsafe { DefSubclassProc(hwnd, message, wparam, lparam) },
        }
    }

    /// Exclude the shell and file dialogs from fullscreen detection.
    pub fn browsing_files() -> bool {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetAncestor, GetClassNameW, GetForegroundWindow, GA_ROOTOWNER,
        };
        unsafe {
            let h = GetForegroundWindow();
            if h.is_null() {
                return true; // Focus is between windows.
            }
            let owner = GetAncestor(h, GA_ROOTOWNER);
            [h, owner].into_iter().filter(|w| !w.is_null()).any(|w| {
                let mut buf = [0u16; 64];
                let n = GetClassNameW(w, buf.as_mut_ptr(), buf.len() as i32);
                matches!(
                    String::from_utf16_lossy(&buf[..n.max(0) as usize]).as_str(),
                    "CabinetWClass"
                        | "ExploreWClass"
                        | "#32770"
                        | "Progman"
                        | "WorkerW"
                        | "Shell_TrayWnd"
                        | "Shell_SecondaryTrayWnd"
                )
            }) || window_executable_matches(h, |name| name.eq_ignore_ascii_case("explorer.exe"))
        }
    }

    /// A popup or picker owned by this process can take focus from the main window.
    pub fn app_is_foreground() -> bool {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId,
        };
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut pid) };
        pid == std::process::id()
    }

    pub fn left_mouse_down() -> bool {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
        (unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON)) }) < 0
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

    /// Where Windows 11 remembers, per tray icon, whether it sits next to the clock or in
    /// the hidden "^" overflow. Settings → Taskbar → "Other system tray icons" edits this.
    const TRAY_ICONS: &str = r"Control Panel\NotifyIconSettings";

    /// Registry paths of Kompacter's own entries in that list (an old install folder can
    /// leave a second one). Empty on Windows 10, or before the icon was ever shown.
    fn tray_entries() -> Vec<Vec<u16>> {
        use windows_sys::Win32::System::Registry::{
            RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, HKEY, KEY_READ,
        };
        let Some(exe) = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let value = wide("ExecutablePath");
        unsafe {
            let mut root: HKEY = null_mut();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                wide(TRAY_ICONS).as_ptr(),
                0,
                KEY_READ,
                &mut root,
            ) != 0
            {
                return out;
            }
            for index in 0.. {
                let mut id = [0u16; 64];
                let mut id_len = id.len() as u32;
                if RegEnumKeyExW(
                    root,
                    index,
                    id.as_mut_ptr(),
                    &mut id_len,
                    null(),
                    null_mut(),
                    null_mut(),
                    null_mut(),
                ) != 0
                {
                    break;
                }
                let mut path = [0u16; 1024];
                let mut bytes = (path.len() * 2) as u32;
                if RegGetValueW(
                    root,
                    id.as_ptr(),
                    value.as_ptr(),
                    RRF_RT_REG_SZ,
                    null_mut(),
                    path.as_mut_ptr() as *mut _,
                    &mut bytes,
                ) != 0
                {
                    continue;
                }
                let chars = (bytes as usize / 2).saturating_sub(1).min(path.len());
                let path = String::from_utf16_lossy(&path[..chars]);
                // The folder part can be written as a known-folder id, so match the name.
                let ours = path
                    .rsplit('\\')
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case(&exe));
                if ours {
                    let id = String::from_utf16_lossy(&id[..id_len as usize]);
                    out.push(wide(format!("{TRAY_ICONS}\\{id}")));
                }
            }
            RegCloseKey(root);
        }
        out
    }

    /// Is the tray icon next to the clock (`true`) or tucked into the "^" overflow
    /// (`false`)? `None` when Windows doesn't say.
    pub fn tray_pinned() -> Option<bool> {
        let entries = tray_entries();
        if entries.is_empty() {
            return None;
        }
        let name = wide("IsPromoted");
        Some(entries.iter().any(|key| {
            let mut value: u32 = 0;
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
            value == 1
        }))
    }

    /// Move the tray icon next to the clock, or back into the overflow. Windows gives apps
    /// no official call for this, so it flips the same per-user switch its Settings page
    /// does. False when that switch isn't there (the caller then opens that page).
    pub fn set_tray_pinned(on: bool) -> bool {
        use windows_sys::Win32::System::Registry::REG_DWORD;
        let entries = tray_entries();
        let name = wide("IsPromoted");
        let value = u32::from(on);
        !entries.is_empty()
            && entries.iter().all(|key| unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    key.as_ptr(),
                    name.as_ptr(),
                    REG_DWORD,
                    &value as *const u32 as *const _,
                    4,
                ) == 0
            })
    }

    /// Settings → Personalisation → Taskbar, where "Other system tray icons" lives.
    pub fn open_taskbar_settings() {
        let _ = std::process::Command::new("explorer")
            .arg("ms-settings:taskbar")
            .spawn();
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

    // ── The global shortcut and the paste guard ──
    //
    // One background thread owns both: the shortcut is a registered hotkey and the guard
    // a low-level keyboard hook (installed only while the guard is on). Both need a
    // message loop, which that thread runs. The UI thread talks to it with thread messages.

    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
    use std::sync::{Mutex, OnceLock};

    use super::InputEvent;

    static ON_INPUT: OnceLock<Box<dyn Fn(InputEvent) + Send + Sync>> = OnceLock::new();
    static INPUT_THREAD: AtomicU32 = AtomicU32::new(0);
    static GUARD_LIMIT: AtomicU64 = AtomicU64::new(u64::MAX);
    /// Clipboard sequence number and limit → the too-big files on it (empty: none), so
    /// repeated Ctrl+V doesn't read the clipboard each time.
    static GUARD_SEEN: Mutex<Option<(u32, u64, Vec<PathBuf>)>> = Mutex::new(None);

    const WM_APP: u32 = 0x8000;
    const MSG_SHORTCUT: u32 = WM_APP + 1;
    const MSG_GUARD: u32 = WM_APP + 2;

    fn emit(e: InputEvent) {
        if let Some(f) = ON_INPUT.get() {
            f(e);
        }
    }

    /// Start the input thread. `on_event` runs on that thread; keep it quick.
    pub fn start_input(on_event: impl Fn(InputEvent) + Send + Sync + 'static) {
        use windows_sys::Win32::System::Threading::GetCurrentThreadId;
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            RegisterHotKey, UnregisterHotKey, MOD_NOREPEAT,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetMessageW, PeekMessageW, SetWindowsHookExW, UnhookWindowsHookEx, MSG, PM_NOREMOVE,
            WH_KEYBOARD_LL, WM_HOTKEY, WM_USER,
        };
        if ON_INPUT.set(Box::new(on_event)).is_err() {
            return;
        }
        let (ready, wait) = std::sync::mpsc::channel();
        std::thread::spawn(move || unsafe {
            let mut msg: MSG = std::mem::zeroed();
            // Makes this thread's message queue, so posts to it work from now on.
            PeekMessageW(&mut msg, null_mut(), WM_USER, WM_USER, PM_NOREMOVE);
            INPUT_THREAD.store(GetCurrentThreadId(), Ordering::SeqCst);
            let _ = ready.send(());
            let mut hook: windows_sys::Win32::UI::WindowsAndMessaging::HHOOK = null_mut();
            let mut have_key = false;
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                match msg.message {
                    WM_HOTKEY => emit(InputEvent::Shortcut),
                    MSG_SHORTCUT => {
                        if have_key {
                            UnregisterHotKey(null_mut(), 1);
                            have_key = false;
                        }
                        if msg.lParam != 0 {
                            have_key = RegisterHotKey(
                                null_mut(),
                                1,
                                msg.wParam as u32 | MOD_NOREPEAT,
                                msg.lParam as u32,
                            ) != 0;
                            emit(InputEvent::ShortcutTaken(!have_key));
                        } else {
                            emit(InputEvent::ShortcutTaken(false));
                        }
                    }
                    MSG_GUARD => {
                        let on = msg.wParam != 0;
                        if on && hook.is_null() {
                            hook = SetWindowsHookExW(
                                WH_KEYBOARD_LL,
                                Some(guard_proc),
                                windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(null()),
                                0,
                            );
                        } else if !on && !hook.is_null() {
                            UnhookWindowsHookEx(hook);
                            hook = null_mut();
                        }
                    }
                    _ => {}
                }
            }
        });
        let _ = wait.recv_timeout(std::time::Duration::from_secs(2));
    }

    fn post(msg: u32, w: usize, l: isize) {
        use windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW;
        let t = INPUT_THREAD.load(Ordering::SeqCst);
        if t != 0 {
            unsafe { PostThreadMessageW(t, msg, w, l) };
        }
    }

    /// Register the global shortcut: (modifiers, virtual key), or None for no shortcut.
    /// The answer comes back as `InputEvent::ShortcutTaken`.
    pub fn set_shortcut(key: Option<(u32, u32)>) {
        let (mods, vk) = key.unwrap_or((0, 0));
        post(MSG_SHORTCUT, mods as usize, vk as isize);
    }

    /// Turn the paste guard on or off; `limit` is the current target in bytes.
    pub fn set_guard(on: bool, limit: u64) {
        GUARD_LIMIT.store(limit, Ordering::SeqCst);
        post(MSG_GUARD, on as usize, 0);
    }

    /// The window in front is Discord (the app, or Discord in a browser tab).
    fn is_discord(h: windows_sys::Win32::Foundation::HWND) -> bool {
        use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowTextW;
        unsafe {
            let mut title = [0u16; 256];
            let n = GetWindowTextW(h, title.as_mut_ptr(), title.len() as i32);
            if String::from_utf16_lossy(&title[..n.max(0) as usize]).contains("Discord") {
                return true;
            }
        }
        window_executable_matches(h, |name| name.to_ascii_lowercase().starts_with("discord"))
    }

    fn window_executable_matches(
        h: windows_sys::Win32::Foundation::HWND,
        matches: impl FnOnce(&str) -> bool,
    ) -> bool {
        use windows_sys::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
            PROCESS_QUERY_LIMITED_INFORMATION,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, &mut pid);
            let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if p.is_null() {
                return false;
            }
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
            CloseHandle(p);
            ok != 0
                && Path::new(&String::from_utf16_lossy(&buf[..len as usize]))
                    .file_name()
                    .is_some_and(|f| matches(&f.to_string_lossy()))
        }
    }

    /// The files on the clipboard if any of them is over the limit (else empty).
    fn too_big_on_clipboard() -> Vec<PathBuf> {
        let seq = clipboard_seq();
        let limit = GUARD_LIMIT.load(Ordering::SeqCst);
        let mut seen = GUARD_SEEN.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((s, l, files)) = seen.as_ref() {
            if *s == seq && *l == limit {
                return files.clone();
            }
        }
        let files = clipboard_files();
        let over = files
            .iter()
            .any(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() > limit));
        let files = if over { files } else { Vec::new() };
        *seen = Some((seq, limit, files.clone()));
        files
    }

    unsafe extern "system" fn guard_proc(code: i32, wp: usize, lp: isize) -> isize {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            GetAsyncKeyState, VK_CONTROL, VK_MENU, VK_V,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CallNextHookEx, GetForegroundWindow, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED,
            WM_KEYDOWN, WM_SYSKEYDOWN,
        };
        unsafe {
            if code == HC_ACTION as i32
                && (wp == WM_KEYDOWN as usize || wp == WM_SYSKEYDOWN as usize)
            {
                let kb = &*(lp as *const KBDLLHOOKSTRUCT);
                let down = |vk: u16| GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0;
                if kb.vkCode == VK_V as u32
                    && kb.flags & LLKHF_INJECTED == 0
                    && down(VK_CONTROL)
                    && !down(VK_MENU)
                {
                    let front = GetForegroundWindow();
                    if !front.is_null() && is_discord(front) {
                        let files = too_big_on_clipboard();
                        if !files.is_empty() {
                            emit(InputEvent::Guard {
                                files,
                                window: front as isize,
                            });
                            return 1; // Discord never sees this paste.
                        }
                    }
                }
            }
            CallNextHookEx(null_mut(), code, wp, lp)
        }
    }

    /// Bring a window to the front (Kompacter is in front, so Windows allows it).
    pub fn focus_window(window: isize) {
        use windows_sys::Win32::UI::WindowsAndMessaging::SetForegroundWindow;
        unsafe { SetForegroundWindow(window as _) };
    }

    /// `window` is the one in front.
    pub fn is_foreground(window: isize) -> bool {
        use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
        unsafe { GetForegroundWindow() as isize == window }
    }

    /// Press Ctrl+V for the window in front (marked as injected, so the guard lets it by).
    pub fn send_paste() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_CONTROL,
            VK_V,
        };
        let key = |vk: u16, up: bool| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let keys = [
            key(VK_CONTROL, false),
            key(VK_V, false),
            key(VK_V, true),
            key(VK_CONTROL, true),
        ];
        unsafe {
            SendInput(
                keys.len() as u32,
                keys.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            );
        }
    }

    /// Changes whenever anything is copied.
    pub fn clipboard_seq() -> u32 {
        unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() }
    }

    /// A full-screen app (usually a game) is in front: don't pop up over it.
    pub fn fullscreen_in_front() -> bool {
        use windows_sys::Win32::Graphics::Gdi::MonitorFromWindow;
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect};
        unsafe {
            let h = GetForegroundWindow();
            if h.is_null() || browsing_files() {
                return false;
            }
            let mut r: RECT = std::mem::zeroed();
            let mon = MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST);
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetWindowRect(h, &mut r) == 0 || GetMonitorInfoW(mon, &mut info) == 0 {
                return false;
            }
            let m = info.rcMonitor;
            r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
        }
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
    pub fn file_icon(_path: &Path) -> Option<(u32, u32, Vec<u8>)> {
        None
    }
    pub fn style_plain_window(_hwnd: isize) {}
    pub fn animations_enabled() -> bool {
        true
    }
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
    pub fn first_instance() -> bool {
        true
    }
    pub fn listen_for_show(_on_show: impl Fn() + Send + 'static) {}
    pub fn start_input(_on_event: impl Fn(super::InputEvent) + Send + Sync + 'static) {}
    pub fn set_shortcut(_key: Option<(u32, u32)>) {}
    pub fn set_guard(_on: bool, _limit: u64) {}
    pub fn focus_window(_window: isize) {}
    pub fn is_foreground(_window: isize) -> bool {
        true
    }
    pub fn send_paste() {}
    pub fn clipboard_seq() -> u32 {
        0
    }
    pub fn fullscreen_in_front() -> bool {
        false
    }
}
