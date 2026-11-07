//! Where compressed files go, and tidying up old ones.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use crate::settings::Settings;

/// Compressed files in Kompacter's own folder are kept this long, then deleted.
const KEEP: Duration = Duration::from_secs(7 * 24 * 3600);
const CLEAN_INTERVAL: Duration = Duration::from_secs(3600);
static LAST_CLEAN: Mutex<Option<Instant>> = Mutex::new(None);

/// Where results go (setting `output`). Files keep the `name.kompacter.ext` name.
/// - "app" (default): Kompacter's own folder (see `app_dir`), cleaned after 7 days
/// - "beside": a `Kompacted` folder next to the original
/// - "custom": the folder in `output_dir`
/// - "replace": made in the app folder, then swapped in for the original
///
/// If the chosen folder can't be written (read-only drive, unplugged USB stick, deleted
/// custom folder), the app folder is used so the compress still works.
pub fn dir(prefs: &Settings, original: &Path) -> Result<PathBuf, String> {
    let parent = original.parent().unwrap_or(Path::new("."));
    let chosen = match prefs.output.as_str() {
        "beside" => Some(parent.join("Kompacted")),
        "custom" if !prefs.output_dir.trim().is_empty() => Some(PathBuf::from(&prefs.output_dir)),
        _ => None,
    };
    match chosen {
        Some(d) if writable(&d) => Ok(d),
        _ => app_dir(),
    }
}

/// Kompacter's own output folder, first one that works:
/// 1. `Kompacted` next to Kompacter.exe (portable style)
/// 2. `Videos\Kompacter`
/// 3. the app's local data folder
pub fn app_dir() -> Result<PathBuf, String> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("Kompacted")))
        .filter(|d| writable(d))
        .or_else(|| {
            crate::sys::videos_dir()
                .map(|d| d.join("Kompacter"))
                .filter(|d| writable(d))
        })
        .or_else(|| {
            crate::sys::local_data_dir()
                .map(|d| d.join("Kompacter").join("Kompacted"))
                .filter(|d| writable(d))
        })
        .ok_or_else(|| "There's no folder Kompacter can save to.".into())
}

/// Creates `dir` if needed and checks a file can actually be written there (a folder can
/// exist and still be read-only, e.g. under Program Files).
fn writable(dir: &Path) -> bool {
    std::fs::create_dir_all(dir).is_ok() && tempfile::NamedTempFile::new_in(dir).is_ok()
}

/// "Replace the original": the original goes to the Recycle Bin (never deleted outright)
/// and the result takes its place, under the original name with the new extension.
/// Folders are never replaced (the zip stays where it was made), and a different file that
/// already has the new name is never overwritten: the result gets a numbered name instead.
pub fn replace_original(made: &Path, original: &Path) -> Result<PathBuf, String> {
    if original.is_dir() {
        return Ok(made.to_path_buf());
    }
    let stem = original
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "kompacted".into());
    let ext = made
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let mut dest = original.with_file_name(format!("{stem}{ext}"));
    let mut n = 2;
    while dest != original && dest.exists() {
        dest = original.with_file_name(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    crate::sys::recycle(original)
        .map_err(|e| format!("Couldn't move the original to the Recycle Bin: {e}"))?;
    // rename fails across drives, so fall back to copy.
    if std::fs::rename(made, &dest).is_err() {
        std::fs::copy(made, &dest).map_err(|e| {
            format!(
                "Couldn't put the result in place ({e}). The original is in the Recycle Bin \
                 and the result is at {}",
                made.display()
            )
        })?;
        let _ = std::fs::remove_file(made);
    }
    Ok(dest)
}

/// Delete week-old files from the app folder (never folders the user chose). `keep` is
/// skipped, so a just-used file is never removed.
pub fn clean(keep: Option<&Path>) {
    let Ok(mut last) = LAST_CLEAN.try_lock() else {
        return;
    };
    if last.is_some_and(|at| at.elapsed() < CLEAN_INTERVAL) {
        return;
    }
    let Ok(dir) = app_dir() else { return };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    *last = Some(Instant::now());
    drop(last);
    let now = SystemTime::now();
    for e in entries.flatten() {
        let path = e.path();
        let old = e
            .metadata()
            .and_then(|m| if m.is_file() { m.modified() } else { Ok(now) })
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > KEEP);
        if old && keep != Some(path.as_path()) {
            let _ = std::fs::remove_file(path);
        }
    }
}
