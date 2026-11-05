//! Kompacter's engine: fit any file under an upload limit, fully on this machine.
//!
//! - [`Engine::probe`] reads what a file is (video, audio, image, folder, other).
//! - [`Engine::estimate`] is pure math for live UI readouts (no encoding).
//! - [`Engine::compress`] does the work with ffmpeg, reporting progress and honouring cancel.

pub mod error;
pub mod ffmpeg;
pub mod gpu;
mod jobs;
pub mod plan;
pub mod probe;
pub mod size;
mod thumbs;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

pub use error::{Error, Result};
pub use ffmpeg::Ffmpeg;
pub use gpu::Encoder;
pub use jobs::split::MAX_PARTS;
pub use plan::{Quality, Trim, VideoPlan};
pub use probe::{MediaInfo, MediaKind};
pub use thumbs::Frames;

/// What to do with a file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Re-encode so it lands under the limit (quality adapts).
    #[default]
    Fit,
    /// Only cut the selected range, no re-encode (size adapts).
    KeepQuality,
}

/// Encoder speed vs. efficiency.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Speed {
    Fast,
    #[default]
    Balanced,
    Quality,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub target_bytes: u64,
    pub mode: Mode,
    pub trim: Option<Trim>,
    /// Where results go. Defaults to the input's folder.
    pub out_dir: Option<PathBuf>,
    pub speed: Speed,
    /// Use a hardware encoder when one works (falls back to CPU on any failure).
    pub gpu: bool,
    /// When it can't fit (well) in one file: video/audio are cut into equal parts and
    /// folders into several zips, each under the target (see `Estimate::parts`).
    pub split: bool,
}

impl Options {
    pub fn new(target_bytes: u64) -> Self {
        Options {
            target_bytes,
            mode: Mode::Fit,
            trim: None,
            out_dir: None,
            speed: Speed::Balanced,
            gpu: true,
            split: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub path: PathBuf,
    pub kind: MediaKind,
    pub original_bytes: u64,
    pub output_bytes: u64,
    pub fits: bool,
    /// True when the file already fit and nothing was written.
    pub untouched: bool,
    /// Every file made, in order, when it was split into parts (`path` is the first).
    /// Empty for a single file. `output_bytes` is then the total.
    pub parts: Vec<PathBuf>,
}

/// Live numbers for the UI. Cheap enough to recompute on every trim-handle drag.
#[derive(Debug, Clone, Serialize)]
pub struct Estimate {
    pub kind: MediaKind,
    pub original_bytes: u64,
    pub target_bytes: u64,
    /// Length of the selection (or whole file), in seconds.
    pub seconds: f64,
    pub fits_as_is: bool,
    /// Keep-quality mode: expected size of the cut.
    pub keep_quality_bytes: Option<u64>,
    /// Fit mode, video: resolution/bitrate/quality we'd use.
    pub video: Option<VideoPlan>,
    /// Fit mode, audio: bitrate we'd use (`None` = too long to fit).
    pub audio_kbps: Option<u32>,
    /// Video/audio too long to fit well in one file: how many parts would each fit at a
    /// decent quality (at most `MAX_PARTS`). `None` when one file is fine.
    pub parts: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct Engine {
    ff: Ffmpeg,
    encoders: OnceLock<Vec<Encoder>>,
}

impl Engine {
    pub fn new(ff: Ffmpeg) -> Self {
        Engine {
            ff,
            encoders: OnceLock::new(),
        }
    }

    /// Hardware encoders that work on this PC, best first. Detected once, then cached.
    pub fn hw_encoders(&self) -> &[Encoder] {
        self.encoders.get_or_init(|| gpu::detect(&self.ff))
    }

    /// `count` small JPEG frames across a video, for the trimmer's filmstrip.
    /// Filmstrip frames for the trimmer (raw RGB).
    pub fn thumbnails(&self, info: &MediaInfo, count: u32, height: u32) -> Result<thumbs::Frames> {
        let size = info.video.as_ref().map_or((0, 0), |v| (v.width, v.height));
        thumbs::thumbnails(&self.ff, &info.path, info.duration, size, count, height)
    }

    pub fn probe(&self, path: &Path) -> Result<MediaInfo> {
        probe::probe(&self.ff, path)
    }

    pub fn estimate(&self, info: &MediaInfo, opts: &Options) -> Result<Estimate> {
        let trim = resolve_trim(info, opts.trim)?;
        let seconds = trim.map(|t| t.len()).unwrap_or(info.duration);
        let target = opts.target_bytes;
        let keep_quality_bytes =
            trim.map(|t| plan::estimate_trim_bytes(info.size_bytes, info.duration, t));
        let video = match (&info.video, info.kind) {
            (Some(v), MediaKind::Video) => Some(plan::plan_video(
                target,
                seconds,
                v.width,
                v.height,
                v.fps,
                info.audio.is_some(),
            )),
            _ => None,
        };
        let audio_kbps = (info.kind == MediaKind::Audio)
            .then(|| plan::plan_audio_kbps(target, seconds))
            .flatten();
        let parts = split_count(info, seconds, target);
        Ok(Estimate {
            kind: info.kind,
            original_bytes: info.size_bytes,
            target_bytes: target,
            seconds,
            fits_as_is: trim.is_none() && info.size_bytes <= target,
            keep_quality_bytes,
            video,
            audio_kbps,
            parts,
        })
    }

    /// Compress `path` per `opts`. `progress` gets 0..=1; set `cancel` to stop early.
    pub fn compress(
        &self,
        path: &Path,
        opts: &Options,
        progress: &mut dyn FnMut(f32),
        cancel: &AtomicBool,
    ) -> Result<Outcome> {
        let info = self.probe(path)?;
        let trim = resolve_trim(&info, opts.trim)?;
        let target = opts.target_bytes;

        if trim.is_none() && info.size_bytes <= target {
            progress(1.0);
            return Ok(outcome(
                &info,
                path.to_path_buf(),
                info.size_bytes,
                target,
                true,
            ));
        }

        let out_base = out_base(path, opts.out_dir.as_deref())?;
        let ctx = jobs::Ctx {
            ff: &self.ff,
            cancel,
            speed: opts.speed,
            encoders: if opts.gpu { self.hw_encoders() } else { &[] },
        };
        if opts.split && opts.mode == Mode::Fit {
            let parts = match info.kind {
                MediaKind::Video | MediaKind::Audio => {
                    let secs = trim.map_or(info.duration, |t| t.len());
                    split_count(&info, secs, target).map(|n| {
                        jobs::split::media(&ctx, &info, trim, n, target, &out_base, progress)
                    })
                }
                MediaKind::Folder => Some(jobs::folder::fit_parts(
                    &ctx, path, target, &out_base, progress,
                )),
                _ => None,
            };
            if let Some(parts) = parts {
                let parts = parts?;
                let sizes = parts
                    .iter()
                    .map(|p| jobs::file_size(p))
                    .collect::<Result<Vec<_>>>()?;
                return Ok(Outcome {
                    path: parts[0].clone(),
                    kind: info.kind,
                    original_bytes: info.size_bytes,
                    output_bytes: sizes.iter().sum(),
                    fits: sizes.iter().all(|&s| s <= target),
                    untouched: false,
                    parts: if parts.len() > 1 { parts } else { Vec::new() },
                });
            }
        }
        let out = match (opts.mode, trim) {
            (Mode::KeepQuality, Some(t)) => jobs::cut(&ctx, &info, t, &out_base, progress)?,
            (Mode::KeepQuality, None) => {
                return Err(Error::DoesNotFit {
                    what: jobs::name_of(path),
                    target,
                    best: info.size_bytes,
                })
            }
            (Mode::Fit, trim) => jobs::fit_media(&ctx, &info, trim, target, &out_base, progress)?,
        };
        let size = jobs::file_size(&out)?;
        Ok(outcome(&info, out, size, target, false))
    }
}

fn outcome(info: &MediaInfo, path: PathBuf, size: u64, target: u64, untouched: bool) -> Outcome {
    Outcome {
        path,
        kind: info.kind,
        original_bytes: info.size_bytes,
        output_bytes: size,
        fits: size <= target,
        untouched,
        parts: Vec::new(),
    }
}

/// How many equal parts a video/audio needs so each fits at a decent quality: `None` when
/// one file is already fine, otherwise 2…`MAX_PARTS` (`MAX_PARTS` if even that isn't great).
fn split_count(info: &MediaInfo, seconds: f64, target: u64) -> Option<u32> {
    let good = |secs: f64| match (info.kind, &info.video) {
        (MediaKind::Video, Some(v)) => {
            let p = plan::plan_video(target, secs, v.width, v.height, v.fps, info.audio.is_some());
            p.fits && p.quality != Quality::Low
        }
        (MediaKind::Audio, _) => plan::plan_audio_kbps(target, secs).is_some_and(|k| k >= 64),
        _ => true,
    };
    if seconds <= 0.0 || info.size_bytes <= target || good(seconds) {
        return None;
    }
    let max = jobs::split::MAX_PARTS;
    Some((2..=max).find(|&n| good(seconds / n as f64)).unwrap_or(max))
}

/// Trims only make sense for video/audio; clamp them to the real duration.
fn resolve_trim(info: &MediaInfo, trim: Option<Trim>) -> Result<Option<Trim>> {
    match trim {
        None => Ok(None),
        Some(_) if !matches!(info.kind, MediaKind::Video | MediaKind::Audio) => Err(
            Error::InvalidTrim("only video and audio can be trimmed".into()),
        ),
        Some(t) => t.clamp(info.duration).map(Some).map_err(Error::InvalidTrim),
    }
}

/// `<out_dir or input folder>/<name>.kompact` (each job adds its own extension).
fn out_base(input: &Path, out_dir: Option<&Path>) -> Result<PathBuf> {
    let dir = match out_dir {
        Some(d) => d.to_path_buf(),
        None => input
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(".")),
    };
    std::fs::create_dir_all(&dir)?;
    let stem = if input.is_dir() {
        jobs::name_of(input)
    } else {
        input
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    Ok(dir.join(format!("{stem}.kompact")))
}
