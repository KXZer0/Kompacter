//! Video: H.264 + AAC in MP4 aimed at an exact size. Plays inline everywhere.
//! Uses a hardware encoder when one works (single pass, fast), otherwise two-pass x264.
//! If the GPU encode fails for any reason, it falls back to x264 automatically.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::{file_size, input_args, name_of, with_ext, Ctx};
use crate::error::{Error, Result};
use crate::ffmpeg::{args, scaled, Progress};
use crate::gpu::Encoder;
use crate::plan::{plan_video, Trim, VideoPlan};
use crate::probe::MediaInfo;
use crate::Speed;

/// Hardware encoders tend to overshoot a bit more, so they aim lower at first.
const HW_HEADROOM: f64 = 0.92;

pub(crate) fn fit(
    ctx: &Ctx,
    info: &MediaInfo,
    trim: Option<Trim>,
    target: u64,
    out_base: &Path,
    progress: Progress,
) -> Result<PathBuf> {
    let v = info
        .video
        .as_ref()
        .ok_or_else(|| Error::Unsupported("no video stream".into()))?;
    let secs = trim.map(|t| t.len()).unwrap_or(info.duration);
    let plan = plan_video(target, secs, v.width, v.height, v.fps, info.audio.is_some());
    if !plan.fits {
        return Err(Error::TooLong { seconds: secs });
    }

    let mut filters = Vec::new();
    if (plan.width, plan.height) != (v.width, v.height) {
        filters.push(format!(
            "scale={}:{}:flags=lanczos",
            plan.width, plan.height
        ));
    }
    if plan.fps + 0.01 < v.fps {
        filters.push(format!("fps={:.3}", plan.fps));
    }
    let job = Job {
        ctx,
        info,
        trim,
        secs,
        target,
        filters: filters.join(","),
        out: with_ext(out_base, "mp4"),
    };

    if let Some(&enc) = ctx.encoders.first() {
        match job.run_hw(enc, plan.clone(), progress) {
            Ok(p) => return Ok(p),
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            // Driver hiccup or still too big: the CPU path below is the safe fallback.
            Err(_) => progress(0.0),
        }
    }
    job.run_x264(plan, progress)
}

struct Job<'a> {
    ctx: &'a Ctx<'a>,
    info: &'a MediaInfo,
    trim: Option<Trim>,
    secs: f64,
    target: u64,
    filters: String,
    out: PathBuf,
}

impl Job<'_> {
    /// Single pass on the GPU, with one retry at a lower bitrate if it overshoots.
    fn run_hw(&self, enc: Encoder, mut plan: VideoPlan, progress: Progress) -> Result<PathBuf> {
        plan.video_kbps = (plan.video_kbps as f64 * HW_HEADROOM) as u32;
        let mut last = 0;
        for attempt in 0..2 {
            let mut a = self.head();
            a.extend(args(["-c:v", enc.ffmpeg_name(), "-pix_fmt", enc.pix_fmt()]));
            a.extend(args(enc.rate_args(plan.video_kbps)));
            self.tail(&mut a, &plan);
            let (from, to) = if attempt == 0 { (0.0, 1.0) } else { (0.5, 1.0) };
            self.ctx.ff.run(
                &a,
                self.secs,
                &mut scaled(&mut *progress, from, to),
                self.ctx.cancel,
            )?;
            match self.check(&mut plan)? {
                Ok(p) => return Ok(p),
                Err(size) => last = size,
            }
        }
        Err(self.too_big(last))
    }

    /// Two-pass x264 on the CPU, with one retry at a lower bitrate if it overshoots.
    fn run_x264(&self, mut plan: VideoPlan, progress: Progress) -> Result<PathBuf> {
        let tmp = tempfile::tempdir()?;
        let passlog = tmp.path().join("x264");
        let preset = match self.ctx.speed {
            Speed::Fast => "veryfast",
            Speed::Balanced => "fast",
            Speed::Quality => "slow",
        };
        let mut last = 0;
        for attempt in 0..2 {
            let pass = |n: &str, plan: &VideoPlan| {
                let mut a = self.head();
                a.extend(args([
                    "-c:v", "libx264", "-preset", preset, "-pix_fmt", "yuv420p", "-b:v",
                ]));
                a.push(format!("{}k", plan.video_kbps).into());
                a.extend(args(["-pass", n, "-passlogfile"]));
                a.push(passlog.clone().into());
                a
            };
            let (p1, p2) = if attempt == 0 {
                ((0.0, 0.4), (0.4, 1.0))
            } else {
                ((0.9, 0.95), (0.95, 1.0))
            };

            let mut first = pass("1", &plan);
            first.extend(args(["-an", "-f", "null", "-"]));
            self.ctx.ff.run(
                &first,
                self.secs,
                &mut scaled(&mut *progress, p1.0, p1.1),
                self.ctx.cancel,
            )?;

            let mut second = pass("2", &plan);
            self.tail(&mut second, &plan);
            self.ctx.ff.run(
                &second,
                self.secs,
                &mut scaled(&mut *progress, p2.0, p2.1),
                self.ctx.cancel,
            )?;

            match self.check(&mut plan)? {
                Ok(p) => return Ok(p),
                Err(size) => last = size,
            }
        }
        Err(self.too_big(last))
    }

    /// Input, trim and the video stream + filters.
    fn head(&self) -> Vec<OsString> {
        let mut a = input_args(&self.info.path, self.trim);
        a.extend(args(["-map", "0:v:0"]));
        if !self.filters.is_empty() {
            a.extend(args(["-vf", self.filters.as_str()]));
        }
        a
    }

    /// Audio, container flags and output path.
    fn tail(&self, a: &mut Vec<OsString>, plan: &VideoPlan) {
        if self.info.audio.is_some() {
            a.extend(args(["-map", "0:a:0", "-c:a", "aac", "-b:a"]));
            a.push(format!("{}k", plan.audio_kbps).into());
        }
        a.extend(args(["-movflags", "+faststart"]));
        a.push(self.out.clone().into());
    }

    /// `Ok(Ok(path))` if it fits; `Ok(Err(size))` after lowering the bitrate for a retry.
    fn check(&self, plan: &mut VideoPlan) -> Result<std::result::Result<PathBuf, u64>> {
        let size = file_size(&self.out)?;
        if size <= self.target {
            return Ok(Ok(self.out.clone()));
        }
        let ratio = self.target as f64 / size as f64 * 0.95;
        plan.video_kbps = (plan.video_kbps as f64 * ratio) as u32;
        Ok(Err(size))
    }

    fn too_big(&self, best: u64) -> Error {
        let _ = std::fs::remove_file(&self.out);
        Error::DoesNotFit {
            what: name_of(&self.info.path),
            target: self.target,
            best,
        }
    }
}
