//! Click-through for the empty, transparent parts of the flyout window.
//!
//! The window is a fixed canvas big enough for the tallest view, so the card can animate
//! inside it without resizing the window (resizing mid-animation is what made it choppy).
//! A small loop watches the cursor: over the card the window takes the mouse, elsewhere
//! clicks fall through to whatever is behind.

use std::sync::{Arc, Mutex};

use tauri::AppHandle;

#[cfg(windows)]
pub fn start(app: &AppHandle, hit: Arc<Mutex<[f64; 4]>>) {
    use std::time::Duration;
    use tauri::Manager;
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

    /// Extra margin around the card, so the flag flips before the cursor reaches a button.
    const MARGIN: f64 = 6.0;

    let app = app.clone();
    std::thread::spawn(move || {
        let mut ignoring = false;
        loop {
            std::thread::sleep(Duration::from_millis(16));
            let Some(win) = app.get_webview_window("main") else {
                continue;
            };
            if !win.is_visible().unwrap_or(false) {
                if ignoring {
                    ignoring = false;
                    let _ = win.set_ignore_cursor_events(false);
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            let (Ok(origin), Ok(size), Ok(scale)) =
                (win.outer_position(), win.outer_size(), win.scale_factor())
            else {
                continue;
            };
            let mut pt = POINT { x: 0, y: 0 };
            if unsafe { GetCursorPos(&mut pt) } == 0 {
                continue;
            }
            let x = (pt.x - origin.x) as f64 / scale;
            let y = (pt.y - origin.y) as f64 / scale;
            let (ww, wh) = (size.width as f64 / scale, size.height as f64 / scale);
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
