//! Commands the UI calls. Heavy work runs on a blocking thread; progress is sent as
//! `progress` events (0..1).

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

use kompacter_core::size::{Preset, PRESETS};
use kompacter_core::{Estimate, MediaInfo, Mode, Options, Outcome, Trim};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::AppState;

/// Compressed files are kept this long so they can be re-sent, then cleaned up.
const KEEP_OUTPUTS: Duration = Duration::from_secs(7 * 24 * 3600);

#[tauri::command]
pub fn presets() -> Vec<Preset> {
    PRESETS.to_vec()
}

#[derive(Serialize)]
pub struct Inspection {
    info: MediaInfo,
    estimate: Estimate,
}

fn options(target: u64, start: Option<f64>, end: Option<f64>) -> Result<Options, String> {
    let mut o = Options::new(target);
    if start.is_some() || end.is_some() {
        o.trim = Some(Trim::new(start.unwrap_or(0.0), end.unwrap_or(f64::MAX))?);
    }
    Ok(o)
}

/// What is this file, and what would compressing it give?
#[tauri::command]
pub async fn inspect(
    state: State<'_, AppState>,
    path: String,
    target: u64,
    start: Option<f64>,
    end: Option<f64>,
) -> Result<Inspection, String> {
    let engine = state.engine.clone();
    let opts = options(target, start, end)?;
    tauri::async_runtime::spawn_blocking(move || {
        let info = engine.probe(Path::new(&path)).map_err(|e| e.to_string())?;
        let estimate = engine.estimate(&info, &opts).map_err(|e| e.to_string())?;
        Ok(Inspection { info, estimate })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
pub struct Compressed {
    #[serde(flatten)]
    outcome: Outcome,
    /// False if the file couldn't be put on the clipboard (the UI then offers "Show file").
    copied: bool,
}

#[tauri::command]
pub async fn compress(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    target: u64,
    start: Option<f64>,
    end: Option<f64>,
    keep_quality: bool,
) -> Result<Compressed, String> {
    let engine = state.engine.clone();
    let cancel = state.cancel.clone();
    cancel.store(false, Ordering::Relaxed);

    let mut opts = options(target, start, end)?;
    opts.mode = if keep_quality {
        Mode::KeepQuality
    } else {
        Mode::Fit
    };
    opts.out_dir = Some(outputs_dir(&app)?);

    tauri::async_runtime::spawn_blocking(move || {
        let mut last = -1;
        let mut on_progress = |p: f32| {
            let step = (p * 200.0) as i32; // ~0.5% steps, keeps the event rate low
            if step != last {
                last = step;
                let _ = app.emit("progress", p);
            }
        };
        let outcome = engine
            .compress(Path::new(&path), &opts, &mut on_progress, &cancel)
            .map_err(|e| e.to_string())?;
        let copied = outcome.fits
            && crate::clipboard::copy_files(std::slice::from_ref(&outcome.path)).is_ok();
        Ok(Compressed { outcome, copied })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn cancel(state: State<'_, AppState>) {
    state.cancel.store(true, Ordering::Relaxed);
}

#[tauri::command]
pub fn hide(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
}

fn outputs_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?
        .join("outputs");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Delete outputs older than a week. Runs once at startup; errors are ignored.
pub fn clean_old_outputs(app: &AppHandle) {
    let Ok(dir) = outputs_dir(app) else { return };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for e in entries.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > KEEP_OUTPUTS);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}
