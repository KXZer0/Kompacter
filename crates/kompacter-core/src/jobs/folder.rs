//! Folders: zipping alone barely helps (media is already compressed), so each media file is
//! shrunk first, with the size budget shared out by original size, then everything is zipped.
//! If it still can't fit in one zip, `fit_parts` packs it into several zips that each fit.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use zip::write::SimpleFileOptions;
use zip::CompressionMethod;

use super::{fit_media, name_of, with_ext, Ctx};
use crate::error::{Error, Result};
use crate::ffmpeg::{scaled, Progress};
use crate::probe::{probe, MediaInfo, MediaKind};
use crate::ArchiveFormat;

/// A file in the folder: (path relative to the folder, absolute path).
type RelAbs = (PathBuf, PathBuf);

/// One file in a zip: (name inside the zip, file on disk, stored without compressing).
type Entry = (String, PathBuf, bool);

/// Headroom for zip headers and rounding.
const ZIP_SLACK: f64 = 0.95;
const PER_ENTRY_OVERHEAD: u64 = 512;
const COPY_BUFFER: usize = 64 * 1024;

pub(crate) fn fit(
    ctx: &Ctx,
    dir: &Path,
    target: u64,
    out_base: &Path,
    progress: Progress,
) -> Result<PathBuf> {
    fit_dir(
        ctx,
        dir,
        Some(&name_of(dir)),
        false,
        target,
        out_base,
        progress,
    )
}

/// `fit` for any folder: `root` is the folder name inside the archive (`None`: files at
/// the top, as they were in an archive that was opened); `hidden` keeps dot files.
pub(crate) fn fit_dir(
    ctx: &Ctx,
    dir: &Path,
    root: Option<&str>,
    hidden: bool,
    target: u64,
    out_base: &Path,
    progress: Progress,
) -> Result<PathBuf> {
    let what = root.map_or_else(|| name_of(out_base), str::to_string);
    let (files, infos) = list(ctx, dir, hidden)?;

    let total: u64 = infos.iter().map(|i| i.size_bytes).sum();
    let usable = (target as f64 * ZIP_SLACK) as u64;
    let fixed: u64 = infos
        .iter()
        .filter(|i| !shrinkable(i))
        .map(|i| i.size_bytes)
        .sum::<u64>()
        + PER_ENTRY_OVERHEAD * infos.len() as u64;
    let media_total: u64 = infos
        .iter()
        .filter(|i| shrinkable(i))
        .map(|i| i.size_bytes)
        .sum();
    let needs_shrink = total + PER_ENTRY_OVERHEAD * infos.len() as u64 > usable;
    if needs_shrink && fixed >= usable {
        return Err(Error::DoesNotFit {
            what: what.clone(),
            target,
            best: fixed,
        });
    }
    let budget = usable.saturating_sub(fixed);

    let staging = tempfile::tempdir()?;
    // Pictures and documents first, videos last: whatever a file doesn't use of its share
    // goes to the files still to come, so the videos (which gain the most) get the rest.
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by_key(|&i| (shrink_order(infos[i].kind), infos[i].size_bytes));
    let (mut left_budget, mut left_total) = (budget, media_total);
    let mut made: Vec<Option<Entry>> = vec![None; files.len()];
    let n = infos.len().max(1) as f32;
    for (step, &i) in order.iter().enumerate() {
        let ((rel, abs), info) = (&files[i], &infos[i]);
        let slice = |k: f32| 0.95 * k / n;
        let share = if left_total == 0 {
            0
        } else {
            (info.size_bytes as f64 * left_budget as f64 / left_total as f64) as u64
        };
        let mut chosen = abs.clone();
        let mut zip_name = rel.clone();
        if needs_shrink && shrinkable(info) && info.size_bytes > share {
            let out_base = staging.path().join(format!("{i}"));
            let mut p = scaled(&mut *progress, slice(step as f32), slice(step as f32 + 1.0));
            // If one file can't be shrunk, keep the original; the final size check decides.
            if let Ok(out) = fit_media(ctx, info, None, share, &out_base, &mut p) {
                if let Some(ext) = out.extension() {
                    zip_name = rel.with_extension(ext);
                }
                chosen = out;
            }
        }
        if ctx.cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        if shrinkable(info) {
            let used = std::fs::metadata(&chosen).map_or(info.size_bytes, |m| m.len());
            left_budget = left_budget.saturating_sub(used);
            left_total = left_total.saturating_sub(info.size_bytes);
        }
        progress(slice(step as f32 + 1.0));
        let name = root
            .map_or_else(|| zip_name.clone(), |r| Path::new(r).join(&zip_name))
            .to_string_lossy()
            .replace('\\', "/");
        made[i] = Some((name, chosen, is_media(info.kind)));
    }
    // The archive keeps the folder's own order.
    let entries: Vec<Entry> = made.into_iter().flatten().collect();

    let (out, size) = pack(ctx, out_base, &entries, target)?;
    progress(1.0);
    if size > target {
        return Err(Error::DoesNotFit {
            what: what.clone(),
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
    let (files, infos) = list(ctx, dir, false)?;
    let usable = (target as f64 * ZIP_SLACK) as u64;

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
            if !shrinkable(info) {
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
        if ctx.cancel.load(Ordering::Relaxed) {
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
        let ext = ctx.archive.ext();
        let path = if parts == 1 {
            with_ext(out_base, ext)
        } else {
            with_ext(&super::split::part_base(out_base, i as u32 + 1, parts), ext)
        };
        let size = write_archive(ctx, ctx.archive, &path, items, target)?;
        if size > target {
            return Err(Error::DoesNotFit {
                what: root,
                target,
                best: size,
            });
        }
        progress(0.9 + 0.1 * (i + 1) as f32 / parts as f32);
        out.push(path);
    }
    Ok(out)
}

/// Non-media files (documents, programs, ...): pack them into an archive and hope it fits.
pub(crate) fn zip_single(ctx: &Ctx, path: &Path, target: u64, out_base: &Path) -> Result<PathBuf> {
    let (out, size) = pack(
        ctx,
        out_base,
        &[(name_of(path), path.to_path_buf(), false)],
        target,
    )?;
    if size > target {
        return Err(Error::DoesNotFit {
            what: name_of(path),
            target,
            best: size,
        });
    }
    Ok(out)
}

/// Pack `entries` in the chosen format. If a zip doesn't fit and there's something to
/// squeeze (documents, text: not already-compressed media), try 7z, which is often 2-3x
/// smaller on text. Returns the archive and its size (the smallest tried, if none fit).
fn pack(ctx: &Ctx, out_base: &Path, entries: &[Entry], target: u64) -> Result<(PathBuf, u64)> {
    let out = with_ext(out_base, ctx.archive.ext());
    let size = write_archive(ctx, ctx.archive, &out, entries, target)?;
    if size > target && ctx.archive == ArchiveFormat::Zip && entries.iter().any(|e| !e.2) {
        let alt = with_ext(out_base, ArchiveFormat::SevenZ.ext());
        let alt_size = write_archive(ctx, ArchiveFormat::SevenZ, &alt, entries, target)?;
        if alt_size <= target {
            return Ok((alt, alt_size));
        }
        return Ok((out, size.min(alt_size)));
    }
    Ok((out, size))
}

fn write_archive(
    ctx: &Ctx,
    format: ArchiveFormat,
    out: &Path,
    entries: &[Entry],
    target: u64,
) -> Result<u64> {
    if ctx.cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    // Publish only complete archives that fit.
    let dir = out.parent().unwrap_or_else(|| Path::new("."));
    let mut staged = tempfile::NamedTempFile::new_in(dir)?;
    let result = match format {
        ArchiveFormat::Zip => write_zip(staged.as_file_mut(), entries, ctx.cancel),
        ArchiveFormat::SevenZ => write_7z(staged.as_file_mut(), entries, ctx.cancel),
    };
    if ctx.cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    result?;
    let size = staged.as_file().metadata()?.len();
    if size <= target {
        staged.persist(out).map_err(|e| e.error)?;
    }
    Ok(size)
}

struct ArchiveReader<'a> {
    file: BufReader<File>,
    cancel: &'a AtomicBool,
}

impl<'a> ArchiveReader<'a> {
    fn open(path: &Path, cancel: &'a AtomicBool) -> io::Result<Self> {
        Ok(Self {
            file: BufReader::with_capacity(COPY_BUFFER, File::open(path)?),
            cancel,
        })
    }
}

impl Read for ArchiveReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::other("archive cancelled"));
        }
        self.file.read(buffer)
    }
}

/// 7z: LZMA2 (level 6: much smaller than zip for documents and programs, ~100 MB of RAM
/// while working); media is stored as-is, since it's already compressed.
fn write_7z(out: &mut File, entries: &[Entry], cancel: &AtomicBool) -> Result<()> {
    use sevenz_rust2::encoder_options::{EncoderOptions, Lzma2Options};
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter, EncoderConfiguration, EncoderMethod};
    let err = |e: sevenz_rust2::Error| io::Error::other(e.to_string());
    let mut w = ArchiveWriter::new(BufWriter::with_capacity(COPY_BUFFER, out)).map_err(err)?;
    for (name, file, stored) in entries {
        let method = if *stored {
            EncoderConfiguration::new(EncoderMethod::COPY)
        } else {
            EncoderConfiguration::new(EncoderMethod::LZMA2)
                .with_options(EncoderOptions::Lzma2(Lzma2Options::from_level(6)))
        };
        w.set_content_methods(vec![method]);
        w.push_archive_entry(
            ArchiveEntry::from_path(file, name.clone()),
            Some(ArchiveReader::open(file, cancel)?),
        )
        .map_err(err)?;
    }
    w.finish()?.flush()?;
    Ok(())
}

fn write_zip(out: &mut File, entries: &[Entry], cancel: &AtomicBool) -> Result<()> {
    let mut zip = zip::ZipWriter::new(BufWriter::with_capacity(COPY_BUFFER, out));
    for (name, file, stored) in entries {
        // Media is already compressed: storing it is faster and no bigger.
        let method = if *stored {
            CompressionMethod::Stored
        } else {
            CompressionMethod::Deflated
        };
        let opts = SimpleFileOptions::default()
            .compression_method(method)
            // Smallest deflate (a little slower, still opens everywhere).
            .compression_level((!*stored).then_some(9))
            .large_file(std::fs::metadata(file)?.len() >= u32::MAX as u64);
        zip.start_file(name.as_str(), opts)?;
        io::copy(&mut ArchiveReader::open(file, cancel)?, &mut zip)?;
    }
    zip.finish()?.flush()?;
    Ok(())
}

/// Which files a folder shrinks first (see `fit`): the ones that usually come in under
/// their share, so the leftover goes to video.
fn shrink_order(k: MediaKind) -> u8 {
    match k {
        MediaKind::Image => 0,
        MediaKind::Other | MediaKind::Folder => 1,
        MediaKind::Audio => 2,
        MediaKind::Video => 3,
    }
}

/// Already-compressed media: stored in archives as-is.
fn is_media(k: MediaKind) -> bool {
    matches!(k, MediaKind::Video | MediaKind::Audio | MediaKind::Image)
}

/// Media, documents (PDF, Office) and glTF models can be shrunk; everything else is
/// packed as-is.
fn shrinkable(i: &MediaInfo) -> bool {
    is_media(i.kind) || shrinkable_file(&i.path)
}

fn shrinkable_file(p: &Path) -> bool {
    super::docs::kind(p).is_some() || super::model::kind(p).is_some()
}

/// Every file in the folder (relative and absolute path) with its probe.
fn list(ctx: &Ctx, dir: &Path, hidden: bool) -> Result<(Vec<RelAbs>, Vec<MediaInfo>)> {
    let mut files = Vec::new();
    walk(dir, Path::new(""), hidden, &mut files)?;
    let infos = files
        .iter()
        .map(|(_, abs)| {
            if ctx.cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            probe(ctx.ff, abs)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((files, infos))
}

/// Recursively list files as (relative path, absolute path), skipping hidden files
/// unless `hidden`.
fn walk(dir: &Path, rel: &Path, hidden: bool, out: &mut Vec<RelAbs>) -> Result<()> {
    let mut items: Vec<_> = std::fs::read_dir(dir)?.collect::<io::Result<_>>()?;
    items.sort_by_key(|e| e.file_name());
    for e in items {
        let name = e.file_name();
        if !hidden && name.to_string_lossy().starts_with('.') {
            continue;
        }
        let (abs, rel) = (e.path(), rel.join(&name));
        if e.file_type()?.is_dir() {
            walk(&abs, &rel, hidden, out)?;
        } else {
            out.push((rel, abs));
        }
    }
    Ok(())
}

/// Guess the kind from the extension (no probing, so a preview of a big folder is instant).
fn guess_kind(p: &Path) -> MediaKind {
    let ext = p
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "mp4" | "mkv" | "mov" | "avi" | "webm" | "m4v" | "wmv" | "flv" | "ts" | "mts" | "m2ts"
        | "3gp" => MediaKind::Video,
        "mp3" | "wav" | "flac" | "m4a" | "aac" | "ogg" | "opus" | "wma" | "aiff" => {
            MediaKind::Audio
        }
        "jpg" | "jpeg" | "png" | "webp" | "bmp" | "gif" | "tif" | "tiff" | "heic" | "heif"
        | "hif" | "avif" => MediaKind::Image,
        _ => MediaKind::Other,
    }
}

/// Before → after for every file and subfolder, using the same budget sharing as `fit`
/// (each shrinkable file gets its share of the limit; everything else stays the same).
pub(crate) fn preview(dir: &Path, target: u64) -> Result<Vec<crate::TreeRow>> {
    let mut files = Vec::new();
    walk(dir, Path::new(""), false, &mut files)?;
    let sized: Vec<(PathBuf, u64, bool)> = files
        .into_iter()
        .map(|(rel, abs)| {
            let size = std::fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
            let shrink = is_media(guess_kind(&abs)) || shrinkable_file(&abs);
            (rel, size, shrink)
        })
        .collect();
    let usable = (target as f64 * ZIP_SLACK) as u64;
    let total: u64 = sized.iter().map(|f| f.1).sum();
    let overhead = PER_ENTRY_OVERHEAD * sized.len() as u64;
    let fixed: u64 = sized.iter().filter(|f| !f.2).map(|f| f.1).sum::<u64>() + overhead;
    let shrink_total: u64 = sized.iter().filter(|f| f.2).map(|f| f.1).sum();
    let needs = total + overhead > usable;
    let budget = usable.saturating_sub(fixed);
    let after = |size: u64, shrink: bool| {
        if !needs || !shrink || shrink_total == 0 {
            return size;
        }
        let share = (size as f64 * budget as f64 / shrink_total as f64) as u64;
        size.min(share)
    };

    // Build the tree: folders first in walk order, each with the sum of what's inside.
    let mut rows = vec![crate::TreeRow {
        depth: 0,
        name: name_of(dir),
        is_dir: true,
        before: 0,
        after: 0,
    }];
    let mut open: Vec<PathBuf> = Vec::new(); // folders currently shown, as relative paths
    for (rel, size, shrink) in &sized {
        let parent = rel.parent().unwrap_or(Path::new("")).to_path_buf();
        // Close folders we've left, open the ones we entered.
        while open.last().is_some_and(|o| !parent.starts_with(o)) {
            open.pop();
        }
        let mut walked = open.last().cloned().unwrap_or_default();
        for part in parent.strip_prefix(&walked).unwrap_or(&parent).components() {
            walked = walked.join(part);
            rows.push(crate::TreeRow {
                depth: open.len() as u32 + 1,
                name: part.as_os_str().to_string_lossy().into_owned(),
                is_dir: true,
                before: 0,
                after: 0,
            });
            open.push(walked.clone());
        }
        let a = after(*size, *shrink);
        rows.push(crate::TreeRow {
            depth: open.len() as u32 + 1,
            name: rel
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            is_dir: false,
            before: *size,
            after: a,
        });
    }
    // Fill in folder sums: each folder row adds up the rows below it that are deeper.
    for i in (0..rows.len()).rev() {
        if rows[i].is_dir {
            let depth = rows[i].depth;
            let (mut b, mut a) = (0, 0);
            for r in rows[i + 1..].iter().take_while(|r| r.depth > depth) {
                if !r.is_dir {
                    b += r.before;
                    a += r.after;
                }
            }
            rows[i].before = b;
            rows[i].after = a;
        }
    }
    Ok(rows)
}
