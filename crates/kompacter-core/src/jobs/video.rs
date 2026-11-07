//! Video: MP4 (H.264, H.265 or AV1 video + AAC audio) aimed at an exact size.
//! Uses a hardware encoder for the chosen codec when one works (single pass, fast);
//! otherwise the CPU: two-pass x264, or x265 / SVT-AV1. If the GPU encode fails for any
//! reason, the CPU path takes over automatically.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::{file_size, input_args, name_of, with_ext, Ctx};
use crate::error::{Error, Result};
use crate::ffmpeg::{args, scaled, Progress};
use crate::gpu::Encoder;
use crate::plan::{plan_video, Trim, VideoPlan};
use crate::probe::MediaInfo;
use crate::{Speed, VideoCodec};

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
    let plan = plan_video(
        target,
        secs,
        v.width,
        v.height,
        v.fps,
        info.audio.is_some(),
        ctx.codec.efficiency(),
    );
    if !plan.fits {
        return Err(Error::TooLong { seconds: secs });
    }

    let mut filters = Vec::new();
    if plan.fps + 0.01 < v.fps {
        // Discard out-of-range frames before resampling, and resample before scaling
        // so Lanczos runs only on the frames that are kept.
        if let Some(trim) = trim {
            filters.push(format!("trim=end={:.6}", trim.len()));
        }
        filters.push(format!("fps={:.3}", plan.fps));
    }
    if (plan.width, plan.height) != (v.width, v.height) {
        filters.push(format!(
            "scale={}:{}:flags=lanczos",
            plan.width, plan.height
        ));
    }
    // Tight budget: grain and sensor noise eat bits without adding anything, so smooth
    // them lightly first (fast, after scaling). Plenty of bits: leave it untouched.
    if plan.quality != crate::Quality::Great {
        filters.push("hqdn3d=1.5:1.5:6:6".into());
    }
    let cpus = std::thread::available_parallelism().map_or(4, |n| n.get());
    let x264_limit = if plan.height <= 1080 { 16 } else { 24 };
    let job = Job {
        ctx,
        info,
        trim,
        secs,
        target,
        filters: filters.join(","),
        decode_threads: cpus.min(12).to_string(),
        filter_threads: cpus.min(8).to_string(),
        x264_threads: (cpus * 3 / 2).min(x264_limit).to_string(),
        out: with_ext(out_base, "mp4"),
    };

    if let Some(&enc) = (ctx.encoders)().first() {
        match job.run_hw(enc, plan.clone(), progress) {
            Ok(p) => return Ok(p),
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            // Driver hiccup or still too big: the CPU path below is the safe fallback.
            Err(_) => progress(0.0),
        }
    }
    match ctx.codec {
        VideoCodec::H264 => job.run_x264(plan, progress),
        codec => job.run_cpu_single(codec, plan, progress),
    }
}

struct Job<'a, 'ctx> {
    ctx: &'a Ctx<'ctx>,
    info: &'a MediaInfo,
    trim: Option<Trim>,
    secs: f64,
    target: u64,
    filters: String,
    decode_threads: String,
    filter_threads: String,
    x264_threads: String,
    out: PathBuf,
}

impl Job<'_, '_> {
    /// Single pass on the GPU, with one retry at a lower bitrate if it overshoots.
    fn run_hw(&self, enc: Encoder, mut plan: VideoPlan, progress: Progress) -> Result<PathBuf> {
        plan.video_kbps = (plan.video_kbps as f64 * HW_HEADROOM) as u32;
        let mut last = 0;
        for attempt in 0..2 {
            let mut a = self.head();
            let name = enc
                .ffmpeg_name(self.ctx.codec)
                .ok_or_else(|| Error::Unsupported("no GPU encoder for this codec".into()))?;
            a.extend(args(["-c:v", name, "-pix_fmt", enc.pix_fmt()]));
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
                    "-c:v",
                    "libx264",
                    "-preset",
                    preset,
                    "-pix_fmt",
                    "yuv420p",
                    "-threads:v",
                    &self.x264_threads,
                    "-b:v",
                ]));
                a.push(format!("{}k", plan.video_kbps).into());
                a.extend(args(["-pass:v", n, "-passlogfile:v"]));
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

    /// H.265 (x265) or AV1 (SVT-AV1) on the CPU: one pass aimed at the bitrate, with one
    /// retry at a lower bitrate if it overshoots.
    fn run_cpu_single(
        &self,
        codec: VideoCodec,
        mut plan: VideoPlan,
        progress: Progress,
    ) -> Result<PathBuf> {
        let mut last = 0;
        // Without a peak cap SVT-AV1 can overshoot more: start a bit lower, allow a third try.
        let tries = if codec == VideoCodec::Av1 {
            plan.video_kbps = (plan.video_kbps as f64 * 0.9) as u32;
            3
        } else {
            2
        };
        for attempt in 0..tries {
            let mut a = self.head();
            let kbps = plan.video_kbps;
            match codec {
                VideoCodec::Av1 => {
                    // SVT-AV1 presets: lower = slower and smaller.
                    let preset = match self.ctx.speed {
                        Speed::Fast => "10",
                        Speed::Balanced => "8",
                        Speed::Quality => "6",
                    };
                    a.extend(args(["-c:v", "libsvtav1", "-preset", preset]));
                    a.extend(args(["-svtav1-params", "rc=1"]));
                }
                _ => {
                    let preset = match self.ctx.speed {
                        Speed::Fast => "veryfast",
                        Speed::Balanced => "fast",
                        Speed::Quality => "slow",
                    };
                    a.extend(args(["-c:v", "libx265", "-preset", preset]));
                    a.extend(args(["-x265-params", "log-level=error"]));
                }
            }
            a.extend(args(["-pix_fmt", "yuv420p", "-b:v"]));
            a.push(format!("{kbps}k").into());
            // x265 takes a peak cap; SVT-AV1 refuses one outside CRF mode (some versions
            // fail to start), so AV1 relies on its own rate control plus the retry below.
            if codec != VideoCodec::Av1 {
                a.extend(args(["-maxrate"]));
                a.push(format!("{}k", kbps * 3 / 2).into());
                a.extend(args(["-bufsize"]));
                a.push(format!("{}k", kbps * 2).into());
            }
            self.tail(&mut a, &plan);
            let (from, to) = if attempt == 0 { (0.0, 1.0) } else { (0.6, 1.0) };
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

    /// Input, trim and the video stream + filters.
    fn head(&self) -> Vec<OsString> {
        // Decoder, filter, and encoder workers run simultaneously. Bound the first
        // two pools independently so large frames don't pile up on many-core PCs.
        let mut a = args([
            "-threads:v",
            &self.decode_threads,
            "-filter_threads",
            &self.filter_threads,
        ]);
        a.extend(input_args(&self.info.path, self.trim));
        a.extend(args(["-map", "0:v:0"]));
        if self.trim.is_some() {
            // The default encoder time base rounds to a whole frame interval. This can
            // shift video against audio and make -t discard the last selected frame.
            a.extend(args([
                "-fps_mode:v",
                "passthrough",
                "-enc_time_base:v",
                "1:90000",
            ]));
        }
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
        if self.ctx.codec == VideoCodec::H265 {
            // The tag Apple devices (and most players) expect for H.265 in MP4.
            a.extend(args(["-tag:v", "hvc1"]));
        }
        if let Some(trim) = self.trim {
            // MP4 uses both presentation and decode timestamps when computing duration.
            // Cap the final sample in both timelines, including with reordered frames.
            let frame = 1.0 / plan.fps;
            let end = trim.len();
            a.push("-bsf:v".into());
            a.push(
                format!(
                    "setts=pts=PTS:dts=DTS:duration='max(1,min(if(gt(DURATION,0),DURATION,{frame:.6}/TB),{end:.6}/TB-max(PTS,DTS+STARTPTS-STARTDTS)))'"
                )
                .into(),
            );
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
