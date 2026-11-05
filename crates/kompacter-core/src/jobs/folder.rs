//! Folders: zipping alone barely helps (media is already compressed), so each media file is
//! shrunk first, with the size budget shared out by original size, then everything is zipped.
//! If it still can't fit in one zip, `fit_parts` packs it into several zips that each fit.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;
use zip::CompressionMethod;

use super::{fit_media, name_of, with_ext, Ctx};
use crate::error::{Error, Result};
use crate::ffmpeg::{scaled, Progress};
use crate::probe::{probe, MediaKind};

/// One file in a zip: (name inside the zip, file on disk, stored without compressing).
type Entry = (String, PathBuf, bool);

/// Headroom for zip headers and rounding.
const ZIP_SLACK: f64 = 0.95;
const PER_ENTRY_OVERHEAD: u64 = 512;

pub(crate) fn fit(
    ctx: &Ctx,
    dir: &Path,
    target: u64,
    out_base: &Path,
    progress: Progress,
) -> Result<PathBuf> {
    let root = name_of(dir);
    let mut files = Vec::new();
    walk(dir, Path::new(""), &mut files)?;
    let infos = files
        .iter()
        .map(|(_, abs)| probe(ctx.ff, abs))
        .collect::<Result<Vec<_>>>()?;

    let total: u64 = infos.iter().map(|i| i.size_bytes).sum();
    let usable = (target as f64 * ZIP_SLACK) as u64;
    let is_media =
        |k: MediaKind| matches!(k, MediaKind::Video | MediaKind::Audio | MediaKind::Image);
    let fixed: u64 = infos
        .iter()
        .filter(|i| !is_media(i.kind))
        .map(|i| i.size_bytes)
        .sum::<u64>()
        + PER_ENTRY_OVERHEAD * infos.len() as u64;
    let media_total: u64 = infos
        .iter()
        .filter(|i| is_media(i.kind))
        .map(|i| i.size_bytes)
        .sum();
    let needs_shrink = total + PER_ENTRY_OVERHEAD * infos.len() as u64 > usable;
    if needs_shrink && fixed >= usable {
        return Err(Error::DoesNotFit {
            what: root,
            target,
            best: fixed,
        });
    }
    let budget = usable.saturating_sub(fixed);

    let staging = tempfile::tempdir()?;
    let mut entries: Vec<(String, PathBuf, bool)> = Vec::new(); // (name in zip, file, stored)
    let n = infos.len().max(1) as f32;
    for (i, ((rel, abs), info)) in files.iter().zip(&infos).enumerate() {
        let slice = |k: f32| 0.95 * k / n;
        let share = if media_total == 0 {
            0
        } else {
            (info.size_bytes as f64 * budget as f64 / media_total as f64) as u64
        };
        let mut chosen = abs.clone();
        let mut zip_name = rel.clone();
        if needs_shrink && is_media(info.kind) && info.size_bytes > share {
            let out_base = staging.path().join(format!("{i}"));
            let mut p = scaled(&mut *progress, slice(i as f32), slice(i as f32 + 1.0));
            // If one file can't be shrunk, keep the original; the final size check decides.
            if let Ok(out) = fit_media(ctx, info, None, share, &out_base, &mut p) {
                if let Some(ext) = out.extension() {
                    zip_name = rel.with_extension(ext);
                }
                chosen = out;
            }
        }
        if ctx.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        progress(slice(i as f32 + 1.0));
        let name = Path::new(&root)
            .join(&zip_name)
            .to_string_lossy()
            .replace('\\', "/");
        entries.push((name, chosen, is_media(info.kind)));
    }

    let out = with_ext(out_base, "zip");
    write_zip(&out, &entries)?;
    progress(1.0);
    let size = std::fs::metadata(&out)?.len();
    if size > target {
        let _ = std::fs::remove_file(&out);
        return Err(Error::DoesNotFit {
            what: root,
            target,
            best: size,
        });
    }
    Ok(out)
}

/// A folder too big for one zip, as several zips that each fit `target`. Media bigger than
/// a part is shrunk to fit one part (so quality stays as high as possible); then files are
/// packed into as few zips as possible, biggest first. Each zip keeps the folder name and
/// structure, so unzipping all parts gives back the whole folder.
pub(crate) fn fit_parts(
    ctx: &Ctx,
    dir: &Path,
    target: u64,
    out_base: &Path,
    progress: Progress,
) -> Result<Vec<PathBuf>> {
    let root = name_of(dir);
    let mut files = Vec::new();
    walk(dir, Path::new(""), &mut files)?;
    let infos = files
        .iter()
        .map(|(_, abs)| probe(ctx.ff, abs))
        .collect::<Result<Vec<_>>>()?;
    let usable = (target as f64 * ZIP_SLACK) as u64;
    let is_media =
        |k: MediaKind| matches!(k, MediaKind::Video | MediaKind::Audio | MediaKind::Image);

    let staging = tempfile::tempdir()?;
    // (name in zip, file, stored, size)
    let mut entries: Vec<(String, PathBuf, bool, u64)> = Vec::new();
    let n = infos.len().max(1) as f32;
    for (i, ((rel, abs), info)) in files.iter().zip(&infos).enumerate() {
        let slice = |k: f32| 0.9 * k / n;
        let mut chosen = abs.clone();
        let mut zip_name = rel.clone();
        let mut size = info.size_bytes;
        if size + PER_ENTRY_OVERHEAD > usable {
            if !is_media(info.kind) {
                // One document bigger than a whole part can't be split into usable pieces.
                return Err(Error::DoesNotFit {
                    what: name_of(abs),
                    target,
                    best: size,
                });
            }
            let out_base = staging.path().join(format!("{i}"));
            let mut p = scaled(&mut *progress, slice(i as f32), slice(i as f32 + 1.0));
            let out = fit_media(
                ctx,
                info,
                None,
                usable - PER_ENTRY_OVERHEAD,
                &out_base,
                &mut p,
            )?;
            if let Some(ext) = out.extension() {
                zip_name = rel.with_extension(ext);
            }
            size = std::fs::metadata(&out)?.len();
            chosen = out;
        }
        if ctx.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        progress(slice(i as f32 + 1.0));
        let name = Path::new(&root)
            .join(&zip_name)
            .to_string_lossy()
            .replace('\\', "/");
        entries.push((name, chosen, is_media(info.kind), size + PER_ENTRY_OVERHEAD));
    }

    // First-fit, biggest first.
    entries.sort_by_key(|e| std::cmp::Reverse(e.3));
    let mut bins: Vec<(u64, Vec<Entry>)> = Vec::new();
    for (name, file, stored, size) in entries {
        match bins.iter_mut().find(|(used, _)| used + size <= usable) {
            Some((used, items)) => {
                *used += size;
                items.push((name, file, stored));
            }
            None => bins.push((size, vec![(name, file, stored)])),
        }
    }
    let parts = bins.len() as u32;
    if parts > super::split::MAX_PARTS {
        return Err(Error::DoesNotFit {
            what: root,
            target,
            best: bins.iter().map(|b| b.0).sum(),
        });
    }
    let mut out = Vec::new();
    for (i, (_, items)) in bins.iter().enumerate() {
        let path = if parts == 1 {
            with_ext(out_base, "zip")
        } else {
            with_ext(
                &super::split::part_base(out_base, i as u32 + 1, parts),
                "zip",
            )
        };
        write_zip(&path, items)?;
        if std::fs::metadata(&path)?.len() > target {
            return Err(Error::DoesNotFit {
                what: root,
                target,
                best: std::fs::metadata(&path)?.len(),
            });
        }
        progress(0.9 + 0.1 * (i + 1) as f32 / parts as f32);
        out.push(path);
    }
    Ok(out)
}

/// Non-media files (documents, code, ...): deflate them into a zip and hope it fits.
pub(crate) fn zip_single(path: &Path, target: u64, out_base: &Path) -> Result<PathBuf> {
    let out = with_ext(out_base, "zip");
    write_zip(&out, &[(name_of(path), path.to_path_buf(), false)])?;
    let size = std::fs::metadata(&out)?.len();
    if size > target {
        let _ = std::fs::remove_file(&out);
        return Err(Error::DoesNotFit {
            what: name_of(path),
            target,
            best: size,
        });
    }
    Ok(out)
}

fn write_zip(out: &Path, entries: &[(String, PathBuf, bool)]) -> Result<()> {
    let mut zip = zip::ZipWriter::new(File::create(out)?);
    for (name, file, stored) in entries {
        // Media is already compressed: storing it is faster and no bigger.
        let method = if *stored {
            CompressionMethod::Stored
        } else {
            CompressionMethod::Deflated
        };
        let opts = SimpleFileOptions::default()
            .compression_method(method)
            .large_file(std::fs::metadata(file)?.len() >= u32::MAX as u64);
        zip.start_file(name.as_str(), opts)?;
        io::copy(&mut File::open(file)?, &mut zip)?;
    }
    zip.finish()?.flush()?;
    Ok(())
}

/// Recursively list files as (relative path, absolute path), skipping hidden files.
fn walk(dir: &Path, rel: &Path, out: &mut Vec<(PathBuf, PathBuf)>) -> Result<()> {
    let mut items: Vec<_> = std::fs::read_dir(dir)?.collect::<io::Result<_>>()?;
    items.sort_by_key(|e| e.file_name());
    for e in items {
        let name = e.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let (abs, rel) = (e.path(), rel.join(&name));
        if e.file_type()?.is_dir() {
            walk(&abs, &rel, out)?;
        } else {
            out.push((rel, abs));
        }
    }
    Ok(())
}
