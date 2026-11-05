//! Click-through for the empty, transparent parts of the flyout window.
//!
//! The window is a fixed canvas big enough for the tallest view, so the card can animate
//! inside it without resizing the window (resizing mid-animation is what made it choppy).
//! A small loop watches the cursor: over the card the window takes the mouse, elsewhere
//! clicks fall through to whatever is behind.
//!
//! The loop asks Windows directly for the window's position and visibility. Going through
//! Tauri for that would hop onto the UI thread 60 times a second and stutter animations.

use std::sync::{Arc, Mutex};

use tauri::AppHandle;

#[cfg(windows)]
pub fn start(app: &AppHandle, hit: Arc<Mutex<[f64; 4]>>) {
    use std::time::Duration;
    use tauri::Manager;
    use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
    use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetWindowRect, IsWindowVisible,
    };

    /// Extra margin around the card, so the flag flips before the cursor reaches a button.
    const MARGIN: f64 = 6.0;

    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    let Ok(hwnd) = win.hwnd() else { return };
    // Window handles aren't Send; the number is all we need.
    let hwnd = hwnd.0 as usize;

    std::thread::spawn(move || {
        let hwnd = hwnd as HWND;
        let mut ignoring = false;
        loop {
            std::thread::sleep(Duration::from_millis(16));
            if unsafe { IsWindowVisible(hwnd) } == 0 {
                if ignoring {
                    ignoring = false;
                    let _ = win.set_ignore_cursor_events(false);
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            let mut r = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            let mut pt = POINT { x: 0, y: 0 };
            if unsafe { GetWindowRect(hwnd, &mut r) } == 0 || unsafe { GetCursorPos(&mut pt) } == 0
            {
                continue;
            }
            let scale = match unsafe { GetDpiForWindow(hwnd) } {
                0 => 1.0,
                dpi => dpi as f64 / 96.0,
            };
            let x = (pt.x - r.left) as f64 / scale;
            let y = (pt.y - r.top) as f64 / scale;
            let ww = (r.right - r.left) as f64 / scale;
            let wh = (r.bottom - r.top) as f64 / scale;
            let [rx, ry, rw, rh] = *hit.lock().unwrap();
            let on_card = rw > 0.0
                && x >= rx - MARGIN
                && x <= rx + rw + MARGIN
                && y >= ry - MARGIN
                && y <= ry + rh + MARGIN;
            // A file being dragged must find us: click-through windows are invisible to
            // drag & drop, so while the button is held over the window, take the mouse.
            let held = unsafe { GetAsyncKeyState(VK_LBUTTON as i32) } as u16 & 0x8000 != 0;
            let in_window = x >= 0.0 && y >= 0.0 && x <= ww && y <= wh;
            let accept = on_card || (held && in_window);
            if ignoring == accept {
                ignoring = !accept;
                let _ = win.set_ignore_cursor_events(ignoring);
            }
        }
    });
}

/// Other platforms: no click-through yet (the window still works, it just blocks clicks
/// in its transparent area).
#[cfg(not(windows))]
pub fn start(_app: &AppHandle, _hit: Arc<Mutex<[f64; 4]>>) {}
