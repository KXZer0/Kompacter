//! Tray icon and showing/hiding the flyout above it.

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, PhysicalPosition};

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
        let _ = w.hide();
    } else {
        show(app);
    }
}

/// Show the flyout in the bottom-right corner, just above the taskbar.
pub fn show(app: &AppHandle) {
    let Some(w) = app.get_webview_window("main") else {
        return;
    };
    if let (Ok(Some(m)), Ok(size)) = (w.current_monitor(), w.outer_size()) {
        let scale = m.scale_factor();
        let (mx, my) = (m.position().x, m.position().y);
        let (mw, mh) = (m.size().width as i32, m.size().height as i32);
        let margin = (12.0 * scale) as i32;
        let taskbar = (48.0 * scale) as i32;
        let x = mx + mw - size.width as i32 - margin;
        let y = my + mh - size.height as i32 - taskbar - margin;
        let _ = w.set_position(PhysicalPosition::new(x, y));
    }
    let _ = w.show();
    let _ = w.set_focus();
}
