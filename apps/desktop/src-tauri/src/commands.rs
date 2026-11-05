//! Commands the UI calls. Heavy work runs on a blocking thread; progress is sent as
//! `progress` events (0..1).

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

use kompacter_core::size::{Preset, PRESETS};
use kompacter_core::{Encoder, Estimate, MediaInfo, Mode, Options, Outcome, Trim};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::settings::{self, Settings};
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

/// What is this file, and what would compressing it give? Probes are cached, so the
/// trimmer can call this on every drag and only pay for the math.
#[tauri::command]
pub async fn inspect(
    state: State<'_, AppState>,
    path: String,
    target: u64,
    start: Option<f64>,
    end: Option<f64>,
) -> Result<Inspection, String> {
    let opts = options(target, start, end)?;
    let info = probe_cached(&state, Path::new(&path)).await?;
    let estimate = state
        .engine
        .estimate(&info, &opts)
        .map_err(|e| e.to_string())?;
    Ok(Inspection { info, estimate })
}

async fn probe_cached(state: &State<'_, AppState>, path: &Path) -> Result<MediaInfo, String> {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let key = (path.to_path_buf(), modified);
    if let Some(info) = state.probes.lock().unwrap().get(&key) {
        return Ok(info.clone());
    }
    let engine = state.engine.clone();
    let p = path.to_path_buf();
    let info = tauri::async_runtime::spawn_blocking(move || engine.probe(&p))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let mut cache = state.probes.lock().unwrap();
    if cache.len() > 64 {
        cache.clear();
    }
    cache.insert(key, info.clone());
    Ok(info)
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
    let prefs = settings::load(&app);
    opts.gpu = prefs.gpu;
    // The engine always writes to the app cache first; `place` moves the result after.
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
        let mut outcome = engine
            .compress(Path::new(&path), &opts, &mut on_progress, &cancel)
            .map_err(|e| e.to_string())?;
        if !outcome.untouched {
            outcome.path = place(&outcome.path, Path::new(&path), &prefs)?;
        }
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

/// Move a finished file to where the user wants it (setting `output`):
/// - "kompacted": a `Kompacted` folder next to the original (default)
/// - "custom": the folder in `output_dir`
/// - "replace": where the original was; the original goes to the Recycle Bin
/// - "temp": stay in the app cache (auto-deleted after 7 days)
fn place(made: &Path, original: &Path, prefs: &Settings) -> Result<PathBuf, String> {
    let parent = original.parent().unwrap_or(Path::new("."));
    let dir = match prefs.output.as_str() {
        "temp" => return Ok(made.to_path_buf()),
        "replace" => parent.to_path_buf(),
        "custom" if !prefs.output_dir.trim().is_empty() => PathBuf::from(&prefs.output_dir),
        _ => parent.join("Kompacted"),
    };
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    // Clean name: "clip.mp4", not "clip.kompact.mp4".
    let stem = original
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "kompacted".into());
    let stem = if original.is_dir() {
        original
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or(stem)
    } else {
        stem
    };
    let ext = made.extension().map(|e| e.to_string_lossy().into_owned());
    let dest = match ext {
        Some(e) => dir.join(format!("{stem}.{e}")),
        None => dir.join(&stem),
    };

    if prefs.output == "replace" {
        trash::delete(original)
            .map_err(|e| format!("couldn't move the original to the Recycle Bin: {e}"))?;
    }
    // rename fails across drives (cache on C:, video on D:), so fall back to copy.
    if std::fs::rename(made, &dest).is_err() {
        std::fs::copy(made, &dest).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(made);
    }
    Ok(dest)
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

/// Native "open file" dialog. `None` if the user cancels.
#[tauri::command]
pub async fn pick_file(app: AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .blocking_pick_file()
            .map(|f| f.to_string())
    })
    .await
    .ok()
    .flatten()
}

/// Files currently on the clipboard (copied in Explorer). Empty if none.
#[tauri::command]
pub fn clipboard_files() -> Vec<String> {
    crate::clipboard::read_files()
}

/// Open the file's folder with the file selected.
#[tauri::command]
pub fn show_in_folder(path: String) -> Result<(), String> {
    #[cfg(windows)]
    let r = std::process::Command::new("explorer")
        .arg(format!("/select,{path}"))
        .spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open")
        .args(["-R", &path])
        .spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open")
        .arg(Path::new(&path).parent().unwrap_or(Path::new(".")))
        .spawn();
    r.map(|_| ()).map_err(|e| e.to_string())
}

/// Working GPU encoders (empty = CPU only). Detected once in the background at startup.
#[tauri::command]
pub async fn encoders(state: State<'_, AppState>) -> Result<Vec<Encoder>, String> {
    let engine = state.engine.clone();
    tauri::async_runtime::spawn_blocking(move || engine.hw_encoders().to_vec())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_settings(app: AppHandle) -> Settings {
    settings::load(&app)
}

#[tauri::command]
pub fn set_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
    settings::save(&app, &settings)
}

/// Filmstrip frames for the trimmer, as `data:` URLs (no file access needed in the UI).
#[tauri::command]
pub async fn thumbnails(
    state: State<'_, AppState>,
    path: String,
    count: u32,
) -> Result<Vec<String>, String> {
    use base64::Engine as _;
    let info = probe_cached(&state, Path::new(&path)).await?;
    let engine = state.engine.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let frames = engine
            .thumbnails(&info, count.clamp(1, 24), 56)
            .map_err(|e| e.to_string())?;
        Ok(frames
            .iter()
            .map(|f| {
                let b64 = base64::engine::general_purpose::STANDARD.encode(f);
                format!("data:image/jpeg;base64,{b64}")
            })
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Native "choose folder" dialog for the custom output folder. `None` if cancelled.
#[tauri::command]
pub async fn pick_folder(app: AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .blocking_pick_folder()
            .map(|f| f.to_string())
    })
    .await
    .ok()
    .flatten()
}

/// The card's on-screen rectangle (logical px, window-relative). Outside it, clicks pass
/// through the transparent window to whatever is behind (see clickthrough.rs).
#[tauri::command]
pub fn set_hit_rect(state: State<'_, AppState>, x: f64, y: f64, w: f64, h: f64) {
    *state.hit.lock().unwrap() = [x, y, w, h];
}
