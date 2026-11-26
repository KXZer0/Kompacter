//! Where compressed files go, and tidying up old ones.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use crate::platform;

use super::settings::Settings;

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
/// Try the portable output folder on Windows, then the user's Videos or data folder.
pub fn app_dir() -> Result<PathBuf, String> {
    cfg!(windows)
        .then(|| std::env::current_exe().ok())
        .flatten()
        .and_then(|p| p.parent().map(|d| d.join("Kompacted")))
        .filter(|d| writable(d))
        .or_else(|| {
            platform::videos_dir()
                .map(|d| d.join("Kompacter"))
                .filter(|d| writable(d))
        })
        .or_else(|| {
            platform::local_data_dir()
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

/// "Replace the original": the original goes to the Recycle Bin / Trash (never deleted outright)
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
    let bin = platform::TRASH_NAME;
    platform::recycle(original)
        .map_err(|e| format!("Couldn't move the original to the {bin}: {e}"))?;
    // rename fails across drives, so fall back to copy.
    if std::fs::rename(made, &dest).is_err() {
        std::fs::copy(made, &dest).map_err(|e| {
            format!(
                "Couldn't put the result in place ({e}). The original is in the {bin} \
                 and the result is at {}",
                made.display()
            )
        })?;
        let _ = std::fs::remove_file(made);
    }
    Ok(dest)
}

/// Delete week-old files from the app folder (never folders the user chose), and staging
/// folders a crashed compress left there (see `stale_staging`). `keep` is skipped, so a
/// just-used file is never removed.
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
        if stale_staging(&path, now) {
            let _ = std::fs::remove_dir_all(&path);
            continue;
        }
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

/// Staging folders older than this are left over from a crash (a compress never takes a day).
const STAGING_KEEP: Duration = Duration::from_secs(24 * 3600);

/// A `.tmpXXXXXX` folder the engine stages outputs in, which only a crash leaves behind.
/// Deliberately strict: a real folder (not a link), untouched for a day, holding only plain
/// files with the engine's own names. Anything else there means it isn't ours to delete.
fn stale_staging(path: &Path, now: SystemTime) -> bool {
    let old = |m: &std::fs::Metadata| {
        m.modified()
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > STAGING_KEEP)
    };
    let named = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.len() > 4 && n.starts_with(".tmp"));
    if !named {
        return false;
    }
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() && old(&m) => {}
        _ => return false,
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    entries.into_iter().all(|e| {
        let Ok(e) = e else { return false };
        let name = e.file_name();
        let Some(name) = name.to_str() else {
            return false;
        };
        let ours = matches!(
            name,
            "video.mp4" | "native.webp" | "probe.gif" | "candidate.gif"
        ) || name.starts_with("x264")
            || name.ends_with(".jpg")
            || name.ends_with(".webp");
        ours && e.metadata().is_ok_and(|m| m.is_file() && old(&m))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Duration = Duration::from_secs(24 * 3600);

    fn staging(root: &Path, name: &str, files: &[&str]) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir(&dir).unwrap();
        for f in files {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        dir
    }

    #[test]
    fn only_old_engine_staging_folders_are_stale() {
        let root = tempfile::tempdir().unwrap();
        let later = SystemTime::now() + 2 * DAY;
        let soon = SystemTime::now() + DAY / 2;

        let video = staging(
            root.path(),
            ".tmpAb12Cd",
            &["video.mp4", "x264-0.log", "x264-0.log.mbtree"],
        );
        let image = staging(
            root.path(),
            ".tmpEf34Gh",
            &["native.webp", "cat.kompacter.jpg"],
        );
        let empty = staging(root.path(), ".tmpIj56Kl", &[]);
        assert!(stale_staging(&video, later));
        assert!(stale_staging(&image, later));
        assert!(stale_staging(&empty, later));
        // A compress could still be running.
        assert!(!stale_staging(&video, soon));

        // Something we never write, a subfolder, or a folder not named like ours: kept.
        let foreign = staging(root.path(), ".tmpMn78Op", &["video.mp4", "notes.txt"]);
        let nested = staging(root.path(), ".tmpQr90St", &["video.mp4"]);
        std::fs::create_dir(nested.join("inner")).unwrap();
        let named = staging(root.path(), "Holiday", &["video.mp4"]);
        let bare = staging(root.path(), ".tmp", &[]);
        for dir in [&foreign, &nested, &named, &bare] {
            assert!(!stale_staging(dir, later), "{}", dir.display());
        }
        // A plain file with the prefix is not a staging folder.
        std::fs::write(root.path().join(".tmpFile00"), b"x").unwrap();
        assert!(!stale_staging(&root.path().join(".tmpFile00"), later));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_named_like_staging_is_never_followed() {
        let root = tempfile::tempdir().unwrap();
        let target = staging(root.path(), "elsewhere", &["video.mp4"]);
        let link = root.path().join(".tmpLink00");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(!stale_staging(&link, SystemTime::now() + 2 * DAY));
    }
}
