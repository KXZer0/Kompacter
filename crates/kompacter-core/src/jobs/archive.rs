//! Zip and 7z files. What's inside is usually already-compressed media, so packing it
//! again saves almost nothing. Instead the archive is opened, every video, picture,
//! document and model inside is shrunk like in a folder, and it's packed again with the
//! same layout (hidden files kept) in the same format.
//!
//! RAR can only be opened with the proprietary unrar code, so RAR files are packed as-is.
//! Password-protected archives can't be opened either; they're packed as-is too.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::Ordering;

use super::{folder, name_of, Ctx};
use crate::error::{Error, Result};
use crate::ffmpeg::{scaled, Progress};
use crate::ArchiveFormat;

/// Never unpack more than this (a small zip can claim to hold terabytes).
const MAX_UNPACKED: u64 = 32 * 1024 * 1024 * 1024;

/// Archives we can open, by extension.
pub(crate) fn kind(path: &Path) -> Option<ArchiveFormat> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    match ext.as_str() {
        "zip" => Some(ArchiveFormat::Zip),
        "7z" => Some(ArchiveFormat::SevenZ),
        _ => None,
    }
}

pub(crate) fn fit(
    ctx: &Ctx,
    path: &Path,
    target: u64,
    out_base: &Path,
    progress: Progress,
) -> Result<PathBuf> {
    let format = kind(path).ok_or_else(|| Error::Unsupported("not a zip or 7z".into()))?;
    let tmp = tempfile::tempdir()?;
    let dir = tmp.path().join("open");
    std::fs::create_dir(&dir)?;
    match format {
        ArchiveFormat::Zip => unzip(ctx, path, &dir)?,
        ArchiveFormat::SevenZ => un7z(ctx, path, &dir)?,
    }
    progress(0.05);
    let inner = Ctx {
        archive: format,
        ..*ctx
    };
    let mut p = scaled(&mut *progress, 0.05, 1.0);
    folder::fit_dir(&inner, &dir, None, true, target, out_base, &mut p).map_err(|e| match e {
        Error::DoesNotFit { target, best, .. } => Error::DoesNotFit {
            what: name_of(path),
            target,
            best,
        },
        e => e,
    })
}

/// Where an entry goes, or `None` for names that would escape the folder.
fn inside(dir: &Path, name: &Path) -> Option<PathBuf> {
    let safe = name.components().all(|c| matches!(c, Component::Normal(_)));
    (safe && name.components().next().is_some()).then(|| dir.join(name))
}

/// Copy one entry to `to`, counting against the unpack limit.
fn write_entry(reader: &mut dyn Read, to: &Path, left: &mut u64) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = File::create(to)?;
    let n = io::copy(&mut reader.take(*left + 1), &mut f)?;
    if n > *left {
        return Err(Error::Unsupported("too big to open".into()));
    }
    *left -= n;
    Ok(())
}

fn unzip(ctx: &Ctx, src: &Path, dir: &Path) -> Result<()> {
    let bad = |e: zip::result::ZipError| Error::Unsupported(format!("can't open this zip: {e}"));
    let mut z = zip::ZipArchive::new(File::open(src)?).map_err(bad)?;
    let mut left = MAX_UNPACKED;
    for i in 0..z.len() {
        if ctx.cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let mut e = z.by_index(i).map_err(bad)?;
        let Some(to) = e.enclosed_name().and_then(|n| inside(dir, &n)) else {
            continue;
        };
        if e.is_dir() {
            std::fs::create_dir_all(&to)?;
        } else {
            write_entry(&mut e, &to, &mut left)?;
        }
    }
    Ok(())
}

fn un7z(ctx: &Ctx, src: &Path, dir: &Path) -> Result<()> {
    use sevenz_rust2::{ArchiveReader, Password};
    let bad = |e: sevenz_rust2::Error| Error::Unsupported(format!("can't open this 7z: {e}"));
    let mut r = ArchiveReader::open(src, Password::empty()).map_err(bad)?;
    let mut left = MAX_UNPACKED;
    let mut failed: Option<Error> = None;
    r.for_each_entries(|entry, reader| {
        if ctx.cancel.load(Ordering::Relaxed) {
            failed = Some(Error::Cancelled);
            return Ok(false);
        }
        let Some(to) = inside(dir, Path::new(&entry.name().replace('\\', "/"))) else {
            return Ok(true);
        };
        let done = if entry.is_directory() {
            std::fs::create_dir_all(&to).map_err(Error::from)
        } else {
            write_entry(reader, &to, &mut left)
        };
        match done {
            Ok(()) => Ok(true),
            Err(e) => {
                failed = Some(e);
                Ok(false)
            }
        }
    })
    .map_err(bad)?;
    match failed {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
