// No console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Kompacter: a tray app that shrinks any file to fit an upload limit and puts the result
//! on the clipboard. The window is drawn by Slint (frontend/*.slint).
//! Heavy work (probing, compressing, filmstrips) runs on worker threads; results come
//! back to the UI thread with `slint::invoke_from_event_loop`. Nothing runs while the
//! window is hidden.

mod app;
mod frontend;
mod platform;

fn main() -> Result<(), slint::PlatformError> {
    app::start()
}
