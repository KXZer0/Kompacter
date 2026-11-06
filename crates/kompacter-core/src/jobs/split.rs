//! Splitting what can't fit in one file into parts that each fit: videos and audio are
//! cut into equal pieces (each its own playable file); folders are packed into several
//! zips. Up to 10 parts, the most Discord takes in one message.

use std::path::{Path, PathBuf};

use super::{fit_media, Ctx};
use crate::error::{Error, Result};
use crate::ffmpeg::{scaled, Progress};
use crate::plan::Trim;
use crate::probe::MediaInfo;

pub const MAX_PARTS: u32 = 10;

/// `<name>.kompacter` → `<name> (part 2 of 3).kompacter` (each job adds its extension).
pub(crate) fn part_base(out_base: &Path, i: u32, n: u32) -> PathBuf {
    let s = out_base.to_string_lossy();
    let stem = s.strip_suffix(".kompacter").unwrap_or(&s);
    PathBuf::from(format!("{stem} (part {i} of {n}).kompacter"))
}

/// Cut video/audio into `n` equal parts and fit each one to `target`.
pub(crate) fn media(
    ctx: &Ctx,
    info: &MediaInfo,
    trim: Option<Trim>,
    n: u32,
    target: u64,
    out_base: &Path,
    progress: Progress,
) -> Result<Vec<PathBuf>> {
    let (start, len) = trim.map_or((0.0, info.duration), |t| (t.start, t.len()));
    let seg = len / n as f64;
    let mut out = Vec::new();
    for i in 0..n {
        let piece = Trim::new(start + seg * i as f64, start + seg * (i + 1) as f64)
            .map_err(Error::InvalidTrim)?;
        let (a, b) = (i as f32 / n as f32, (i + 1) as f32 / n as f32);
        let mut p = scaled(&mut *progress, a, b);
        let base = part_base(out_base, i + 1, n);
        out.push(fit_media(ctx, info, Some(piece), target, &base, &mut p)?);
    }
    Ok(out)
}
