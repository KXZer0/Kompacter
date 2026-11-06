//! One module per kind of input. Each job writes `<out_base>.<ext>` and returns that path.

pub(crate) mod audio;
pub(crate) mod docs;
pub(crate) mod folder;
pub(crate) mod gif;
pub(crate) mod image;
pub(crate) mod split;
pub(crate) mod video;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::error::{Error, Result};
use crate::ffmpeg::{Ffmpeg, Progress};
use crate::gpu::Encoder;
use crate::plan::Trim;
use crate::probe::{MediaInfo, MediaKind};
use crate::{ArchiveFormat, AudioFormat, ImageFormat, Speed, VideoCodec};

pub(crate) struct Ctx<'a> {
    pub ff: &'a Ffmpeg,
    pub cancel: &'a AtomicBool,
    pub speed: Speed,
    /// Working hardware encoders for `codec`, best first. Empty = CPU only.
    pub encoders: &'a [Encoder],
    pub codec: VideoCodec,
    pub audio_format: AudioFormat,
    pub image_format: ImageFormat,
    pub archive: ArchiveFormat,
}

/// Compress one media file to fit `target` bytes.
pub(crate) fn fit_media(
    ctx: &Ctx,
    info: &MediaInfo,
    trim: Option<Trim>,
    target: u64,
    out_base: &Path,
    progress: Progress,
) -> Result<PathBuf> {
    match info.kind {
        MediaKind::Video => video::fit(ctx, info, trim, target, out_base, progress),
        MediaKind::Audio => audio::fit(ctx, info, trim, target, out_base, progress),
        MediaKind::Image => image::fit(ctx, info, target, out_base, progress),
        MediaKind::Folder => folder::fit(ctx, &info.path, target, out_base, progress),
        MediaKind::Other => match docs::kind(&info.path) {
            // PDFs and Office files: shrink the pictures inside, keep it a document.
            // If that can't reach the limit, pack it like any other file.
            Some(doc) => match docs::fit(ctx, &info.path, doc, target, out_base, progress) {
                Err(Error::Cancelled) => Err(Error::Cancelled),
                Err(_) => folder::zip_single(&info.path, target, out_base, ctx.archive),
                ok => ok,
            },
            None => folder::zip_single(&info.path, target, out_base, ctx.archive),
        },
    }
}

/// `base` + "." + `ext`, without eating dots already in the name.
pub(crate) fn with_ext(base: &Path, ext: &str) -> PathBuf {
    let mut s: OsString = base.as_os_str().to_owned();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}

/// `-ss` before the input (fast, accurate when re-encoding), `-t` after it.
pub(crate) fn input_args(input: &Path, trim: Option<Trim>) -> Vec<OsString> {
    let mut a = Vec::new();
    if let Some(t) = trim {
        a.extend(["-ss".into(), format!("{:.3}", t.start).into()]);
    }
    a.extend(["-i".into(), input.as_os_str().to_owned()]);
    if let Some(t) = trim {
        a.extend(["-t".into(), format!("{:.3}", t.len()).into()]);
    }
    a
}

pub(crate) fn file_size(p: &Path) -> Result<u64> {
    Ok(std::fs::metadata(p)?.len())
}

pub(crate) fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Keep-quality trim: stream copy, no re-encode. Fast, but cuts snap to keyframes.
pub(crate) fn cut(
    ctx: &Ctx,
    info: &MediaInfo,
    trim: Trim,
    out_base: &Path,
    progress: Progress,
) -> Result<PathBuf> {
    let ext = info
        .path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .ok_or_else(|| Error::Unsupported("file has no extension".into()))?;
    let out = with_ext(out_base, &ext);
    let mut a = input_args(&info.path, Some(trim));
    a.extend(crate::ffmpeg::args([
        "-map",
        "0",
        "-c",
        "copy",
        "-avoid_negative_ts",
        "make_zero",
    ]));
    a.push(out.clone().into());
    ctx.ff.run(&a, trim.len(), progress, ctx.cancel)?;
    Ok(out)
}
