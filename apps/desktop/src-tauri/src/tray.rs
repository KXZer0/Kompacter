//! Tray icon and showing/hiding the flyout above it.

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition};

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Kompacter", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;

    TrayIconBuilder::with_id("kompacter")
        .icon(app.default_window_icon().cloned().expect("app icon"))
        .tooltip("Kompacter")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

pub fn toggle(app: &AppHandle) {
    let Some(w) = app.get_webview_window("main") else {
        return;
    };
    if w.is_visible().unwrap_or(false) {
        // The page plays the close animation, then calls `hide`.
        let _ = w.emit("flyout-close", ());
    } else {
        show(app);
    }
}

/// Show the flyout in the corner of the screen the mouse is on, inside the work area
/// (the screen minus the taskbar, wherever it is). The window is a fixed, mostly
/// transparent canvas; the card inside grows open from its bottom edge.
pub fn show(app: &AppHandle) {
    let Some(w) = app.get_webview_window("main") else {
        return;
    };
    let monitor = app
        .cursor_position()
        .ok()
        .and_then(|p| app.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| w.current_monitor().ok().flatten())
        .or_else(|| w.primary_monitor().ok().flatten());
    if let (Some(m), Ok(size), Ok(scale)) = (monitor, w.outer_size(), w.scale_factor()) {
        // Size on the target monitor, which may use a different display scale.
        let k = m.scale_factor() / scale;
        let (ww, wh) = (
            (size.width as f64 * k).round() as i32,
            (size.height as f64 * k).round() as i32,
        );
        let area = m.work_area();
        let (ax, ay) = (area.position.x, area.position.y);
        let (aw, ah) = (area.size.width as i32, area.size.height as i32);
        let x = ax + aw - ww;
        let y = ay + ah - wh;
        let _ = w.set_position(PhysicalPosition::new(x, y));
    }
    let _ = w.show();
    let _ = w.set_focus();
    let _ = w.emit("flyout-open", ());
}
