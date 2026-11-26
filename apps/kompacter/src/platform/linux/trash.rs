//! Moving a file to the desktop's Trash (freedesktop.org Trash specification).
//!
//! `gio trash` comes first: it knows per-drive trash folders and tells the file manager.
//! Without it, the home trash is used directly when the file is on the same drive (a
//! rename). Anything else is an error, so a file is never deleted outright.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn recycle(path: &Path) -> Result<(), String> {
    let path = std::path::absolute(path).map_err(|e| e.to_string())?;
    // Never follow a link: the link itself goes to the Trash, like a file manager does.
    fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    let gio = Command::new("gio")
        .arg("trash")
        .arg("--")
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    let gio_error = match gio {
        Ok(out) if out.status.success() && fs::symlink_metadata(&path).is_err() => return Ok(()),
        Ok(out) => Some(String::from_utf8_lossy(&out.stderr).trim().to_owned()),
        // No gio on this system.
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => Some(e.to_string()),
    };
    let home = home_trash().ok_or("there's no Trash folder")?;
    match move_to(&home, &path) {
        Ok(()) => Ok(()),
        Err(e) => Err(match gio_error {
            Some(g) if !g.is_empty() => g,
            _ => e.to_string(),
        }),
    }
}

fn home_trash() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(data.join("Trash"))
}

/// The spec's steps for one trash folder: reserve a name by creating its `.trashinfo`
/// (exclusively), then rename the file into `files`. Fails across drives.
fn move_to(trash: &Path, path: &Path) -> io::Result<()> {
    let files = trash.join("files");
    let info = trash.join("info");
    for dir in [trash, &files, &info] {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    if fs::metadata(&files)?.dev() != fs::symlink_metadata(path)?.dev() {
        return Err(io::Error::new(
            io::ErrorKind::CrossesDevices,
            "the file is on another drive than the Trash",
        ));
    }
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let body = info_body(path);
    for n in 1..10_000u32 {
        let mut entry = name.to_os_string();
        if n > 1 {
            entry.push(format!(".{n}"));
        }
        let mut info_name = entry.clone();
        info_name.push(".trashinfo");
        let info_path = info.join(&info_name);
        let target = files.join(&entry);
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&info_path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        // A leftover entry in `files` without its info: pick another name.
        if fs::symlink_metadata(&target).is_ok() {
            drop(file);
            let _ = fs::remove_file(&info_path);
            continue;
        }
        let moved = file
            .write_all(body.as_bytes())
            .and_then(|()| file.sync_all())
            .and_then(|()| fs::rename(path, &target));
        if let Err(e) = moved {
            let _ = fs::remove_file(&info_path);
            return Err(e);
        }
        return Ok(());
    }
    Err(io::Error::other(
        "the Trash has too many files with this name",
    ))
}

fn info_body(path: &Path) -> String {
    // The spec wants the path URL-escaped, like the path part of a file:// URI.
    let escaped = url::Url::from_file_path(path)
        .map(|u| u.path().to_owned())
        .unwrap_or_else(|()| path.to_string_lossy().into_owned());
    format!(
        "[Trash Info]\nPath={escaped}\nDeletionDate={}\n",
        local_time()
    )
}

/// Now as `YYYY-MM-DDThh:mm:ss` in local time, as the spec asks.
fn local_time() -> String {
    // SAFETY: `time` and `localtime_r` only write to the struct passed in.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return String::new();
        }
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_trash_keeps_the_file_and_records_where_it_was() {
        let root = tempfile::tempdir().unwrap();
        let trash = root.path().join("Trash");
        let original = root.path().join("my clip.mp4");
        fs::write(&original, b"video").unwrap();
        move_to(&trash, &original).unwrap();
        assert!(!original.exists());
        assert_eq!(fs::read(trash.join("files/my clip.mp4")).unwrap(), b"video");
        let info = fs::read_to_string(trash.join("info/my clip.mp4.trashinfo")).unwrap();
        assert!(info.starts_with("[Trash Info]\nPath=/"));
        assert!(info.contains("my%20clip.mp4\n"));
        assert!(info.contains("DeletionDate=2"));

        // Same name again: a second entry, the first is untouched.
        fs::write(&original, b"again").unwrap();
        move_to(&trash, &original).unwrap();
        assert_eq!(fs::read(trash.join("files/my clip.mp4")).unwrap(), b"video");
        assert_eq!(
            fs::read(trash.join("files/my clip.mp4.2")).unwrap(),
            b"again"
        );
        assert!(trash.join("info/my clip.mp4.2.trashinfo").exists());
    }

    #[test]
    fn a_missing_file_is_an_error_and_leaves_no_info() {
        let root = tempfile::tempdir().unwrap();
        let trash = root.path().join("Trash");
        assert!(move_to(&trash, &root.path().join("gone.mp4")).is_err());
        assert_eq!(fs::read_dir(trash.join("info")).unwrap().count(), 0);
    }
}
