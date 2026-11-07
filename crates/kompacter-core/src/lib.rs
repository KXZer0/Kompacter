//! Kompacter's engine: fit any file under an upload limit, fully on this machine.
//!
//! - [`Engine::probe`] reads what a file is (video, audio, image, folder, other).
//! - [`Engine::estimate`] is pure math for live UI readouts (no encoding).
//! - [`Engine::compress`] does the work with ffmpeg, reporting progress and honouring cancel.

pub mod error;
pub mod ffmpeg;
pub mod formats;
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
pub use formats::{ArchiveFormat, AudioFormat, ImageFormat, VideoCodec};
pub use gpu::Encoder;
pub use jobs::split::MAX_PARTS;
pub use plan::{Quality, Trim, VideoPlan};
pub use probe::{MediaInfo, MediaKind};
pub use thumbs::scrub::TimedFrame;
pub use thumbs::Frames;
pub use thumbs::PCM_RATE as PREVIEW_PCM_RATE;

/// What to do with a file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Re-encode so it lands under the limit (quality adapts).
    #[default]
    Fit,
    /// Only cut the selected range, no re-encode (size adapts).
    KeepQuality,
    /// A GIF of a short clip (up to `MAX_GIF_SECONDS`), sized to fit.
    Gif,
}

pub use jobs::gif::MAX_GIF_SECONDS;

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
    pub video_codec: VideoCodec,
    pub audio_format: AudioFormat,
    pub image_format: ImageFormat,
    pub archive: ArchiveFormat,
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
            video_codec: VideoCodec::H264,
            audio_format: AudioFormat::Auto,
            image_format: ImageFormat::Auto,
            archive: ArchiveFormat::Zip,
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

/// One line of a folder's before → after view.
#[derive(Debug, Clone, Serialize)]
pub struct TreeRow {
    /// 0 is the folder itself.
    pub depth: u32,
    pub name: String,
    pub is_dir: bool,
    pub before: u64,
    /// Estimated (media and documents shrink to their share of the limit).
    pub after: u64,
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
    /// Working hardware encoders per video codec (H.264, H.265, AV1), found on first use.
    encoders: [OnceLock<Vec<Encoder>>; 3],
}

impl Engine {
    pub fn new(ff: Ffmpeg) -> Self {
        Engine {
            ff,
            encoders: Default::default(),
        }
    }

    /// Hardware encoders that work on this PC, best first. Detected once, then cached.
    pub fn hw_encoders(&self) -> &[Encoder] {
        self.hw_encoders_for(VideoCodec::H264)
    }

    /// Hardware encoders for this codec that work on this PC, best first (cached).
    pub fn hw_encoders_for(&self, codec: VideoCodec) -> &[Encoder] {
        let i = match codec {
            VideoCodec::H264 => 0,
            VideoCodec::H265 => 1,
            VideoCodec::Av1 => 2,
        };
        self.encoders[i].get_or_init(|| gpu::detect(&self.ff, codec))
    }

    /// The exact frame at `t` seconds (raw RGB), for the trimmer's preview.
    pub fn frame_at(&self, info: &MediaInfo, t: f64, height: u32) -> Result<thumbs::Frames> {
        self.frame_at_cancellable(info, t, height, &AtomicBool::new(false))
    }

    pub fn frame_at_cancellable(
        &self,
        info: &MediaInfo,
        t: f64,
        height: u32,
        stop: &AtomicBool,
    ) -> Result<thumbs::Frames> {
        let size = info.video.as_ref().map_or((0, 0), |v| (v.width, v.height));
        thumbs::frame_at(&self.ff, &info.path, size, t, height, stop)
    }

    /// Decode a small window of source frames for repeated nearby scrubs.
    pub fn decode_scrub_window(
        &self,
        info: &MediaInfo,
        range: std::ops::Range<f64>,
        height: u32,
        stop: &AtomicBool,
        on_frame: &mut dyn FnMut(TimedFrame) -> bool,
    ) -> Result<()> {
        let size = info.video.as_ref().map_or((0, 0), |v| (v.width, v.height));
        thumbs::scrub::decode(
            &self.ff,
            &info.path,
            size,
            range.start,
            range.end,
            height,
            stop,
            on_frame,
        )
    }

    /// Play `start..end` silently in real time as small RGB frames (see `thumbs::play`).
    #[allow(clippy::too_many_arguments)]
    pub fn play(
        &self,
        info: &MediaInfo,
        start: f64,
        end: f64,
        height: u32,
        fps: f64,
        stop: &AtomicBool,
        on_frame: &mut dyn FnMut(u32, u32, Vec<u8>) -> bool,
    ) -> Result<()> {
        let mut began = None;
        self.play_preview(
            info,
            start,
            end,
            height,
            fps,
            stop,
            &mut || {
                Some(
                    began
                        .get_or_insert_with(std::time::Instant::now)
                        .elapsed()
                        .as_secs_f64(),
                )
            },
            &mut |_, w, h, rgb| on_frame(w, h, rgb),
        )
    }

    /// Deliver preview frames against a session clock in seconds relative to `start`.
    /// A clock returning `None` is still starting. Frames carry absolute media time.
    #[allow(clippy::too_many_arguments)]
    pub fn play_preview(
        &self,
        info: &MediaInfo,
        start: f64,
        end: f64,
        height: u32,
        fps: f64,
        stop: &AtomicBool,
        elapsed: &mut dyn FnMut() -> Option<f64>,
        on_frame: &mut dyn FnMut(f64, u32, u32, Vec<u8>) -> bool,
    ) -> Result<()> {
        let video = info
            .video
            .as_ref()
            .ok_or_else(|| Error::Unsupported("not a video".into()))?;
        thumbs::play(
            &self.ff, &info.path, video, start, end, height, fps, stop, elapsed, on_frame,
        )
    }

    /// Stream 48 kHz stereo signed 16-bit PCM for a preview selection.
    pub fn audio_pcm(
        &self,
        info: &MediaInfo,
        start: f64,
        end: f64,
        stop: &AtomicBool,
        on_samples: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<()> {
        thumbs::audio_pcm(&self.ff, &info.path, start, end, stop, on_samples)
    }

    /// The audio of `start..end` as a WAV file, to play along with `play`.
    pub fn audio_wav(&self, info: &MediaInfo, start: f64, end: f64, out: &Path) -> Result<()> {
        self.audio_wav_cancellable(info, start, end, out, &AtomicBool::new(false))
    }

    /// Extract preview audio, stopping early if the playback session ends.
    pub fn audio_wav_cancellable(
        &self,
        info: &MediaInfo,
        start: f64,
        end: f64,
        out: &Path,
        stop: &AtomicBool,
    ) -> Result<()> {
        thumbs::wav(&self.ff, &info.path, start, end, out, stop)
    }

    /// What a folder would become: every file and subfolder with its size now and its
    /// estimated size after (fast: no file is opened). For the before → after view.
    pub fn folder_preview(&self, dir: &Path, target: u64) -> Result<Vec<TreeRow>> {
        jobs::folder::preview(dir, target)
    }

    /// Filmstrip frames for the trimmer (raw RGB).
    pub fn thumbnails(&self, info: &MediaInfo, count: u32, height: u32) -> Result<thumbs::Frames> {
        self.thumbnails_cancellable(info, count, height, &AtomicBool::new(false))
    }

    pub fn thumbnails_cancellable(
        &self,
        info: &MediaInfo,
        count: u32,
        height: u32,
        stop: &AtomicBool,
    ) -> Result<thumbs::Frames> {
        let size = info.video.as_ref().map_or((0, 0), |v| (v.width, v.height));
        thumbs::thumbnails(
            &self.ff,
            &info.path,
            info.duration,
            size,
            count,
            height,
            stop,
        )
    }

    /// Width and frame rate a GIF of `seconds` of this video would likely get (a guess;
    /// the real encode measures). `None` when it isn't a video or nothing would fit.
    pub fn gif_guess(&self, info: &MediaInfo, seconds: f64, target: u64) -> Option<(u32, f64)> {
        jobs::gif::guess(info, seconds, target)
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
                opts.video_codec.efficiency(),
            )),
            _ => None,
        };
        let audio_kbps = (info.kind == MediaKind::Audio)
            .then(|| plan::plan_audio_kbps(target, seconds))
            .flatten();
        let parts = split_count(info, seconds, target, opts.video_codec.efficiency());
        Ok(Estimate {
            kind: info.kind,
            original_bytes: info.size_bytes,
            target_bytes: target,
            seconds,
            // A folder can't be sent as-is: it always gets packed into one file.
            fits_as_is: trim.is_none()
                && info.size_bytes <= target
                && info.kind != MediaKind::Folder,
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

        let as_is = trim.is_none() && opts.mode != Mode::Gif;
        if as_is && info.size_bytes <= target && info.kind != MediaKind::Folder {
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
        let encoders = || {
            if opts.gpu {
                self.hw_encoders_for(opts.video_codec)
            } else {
                &[]
            }
        };
        let ctx = jobs::Ctx {
            ff: &self.ff,
            cancel,
            speed: opts.speed,
            encoders: &encoders,
            codec: opts.video_codec,
            audio_format: opts.audio_format,
            image_format: opts.image_format,
            archive: opts.archive,
        };
        if opts.split && opts.mode == Mode::Fit {
            let parts = match info.kind {
                MediaKind::Video | MediaKind::Audio => {
                    let secs = trim.map_or(info.duration, |t| t.len());
                    split_count(&info, secs, target, opts.video_codec.efficiency()).map(|n| {
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
            (Mode::Gif, trim) if info.kind == MediaKind::Video => {
                jobs::gif::fit(&ctx, &info, trim, target, &out_base, progress)?
            }
            (Mode::Gif, _) => return Err(Error::Unsupported("only videos become GIFs".into())),
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
fn split_count(info: &MediaInfo, seconds: f64, target: u64, efficiency: f64) -> Option<u32> {
    let good = |secs: f64| match (info.kind, &info.video) {
        (MediaKind::Video, Some(v)) => {
            let p = plan::plan_video(
                target,
                secs,
                v.width,
                v.height,
                v.fps,
                info.audio.is_some(),
                efficiency,
            );
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

/// `<out_dir or input folder>/<name>.kompacter` (each job adds its own extension).
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
    Ok(dir.join(format!("{stem}.kompacter")))
}
