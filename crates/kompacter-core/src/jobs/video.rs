//! Video: MP4 (H.264, H.265 or AV1 video + AAC audio) aimed at an exact size.
//! Uses a hardware encoder for the chosen codec when one works (single pass, fast);
//! otherwise the CPU: two-pass x264, or x265 / SVT-AV1. If the GPU encode fails for any
//! reason, the CPU path takes over automatically. When only the audio makes a file too
//! big, the video is copied untouched and just the audio is re-encoded. HDR sources are
//! tone-mapped to SDR when ffmpeg can.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::{file_size, input_args, name_of, with_ext, Ctx};
use crate::error::{Error, Result};
use crate::ffmpeg::{args, scaled, Progress};
use crate::gpu::Encoder;
use crate::plan::{self, plan_video, Trim, VideoPlan};
use crate::probe::{MediaInfo, VideoStream};
use crate::{hdr, Speed, VideoCodec};

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
    let out = with_ext(out_base, "mp4");
    if let Some(audio_kbps) = copy_plan(info, trim, ctx.codec, target) {
        match copy_video(ctx, info, audio_kbps, target, &out, progress) {
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            // Bigger than the guess, or a stream MP4 won't take as-is: re-encode it all.
            Err(_) => progress(0.0),
            ok => return ok,
        }
    }
    let tonemap = super::tonemap(ctx, info);
    match encode(ctx, info, trim, target, &out, tonemap, progress) {
        // The tone-map filters can still refuse an oddly tagged file: encode it as before.
        Err(Error::Ffmpeg(_)) if tonemap => {
            progress(0.0);
            encode(ctx, info, trim, target, &out, false, progress)
        }
        result => result,
    }
}

/// Re-encode the video per the plan: GPU first, then the CPU.
fn encode(
    ctx: &Ctx,
    info: &MediaInfo,
    trim: Option<Trim>,
    target: u64,
    out: &Path,
    tonemap: bool,
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

    let cpus = std::thread::available_parallelism().map_or(4, |n| n.get());
    let x264_limit = if plan.height <= 1080 { 16 } else { 24 };
    let job = Job {
        ctx,
        info,
        trim,
        secs,
        target,
        filters: filters(v, &plan, trim, tonemap),
        tonemap,
        decode_threads: cpus.min(12).to_string(),
        filter_threads: cpus.min(8).to_string(),
        x264_threads: (cpus * 3 / 2).min(x264_limit).to_string(),
        out: out.to_path_buf(),
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

/// The video filters, in order: frame-rate cap, scale, HDR → SDR, light denoise.
fn filters(v: &VideoStream, plan: &VideoPlan, trim: Option<Trim>, tonemap: bool) -> String {
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
    // Tone-mapping works in 32-bit float per pixel, so it runs on the smaller picture:
    // 4K → 1080p is over twice as fast this way. Scaling the 10-bit HDR first looks the same.
    if tonemap {
        filters.push(hdr::TONEMAP.into());
    }
    // Tight budget: grain and sensor noise eat bits without adding anything, so smooth
    // them lightly first (fast, after scaling). Plenty of bits: leave it untouched.
    if plan.quality != crate::Quality::Great {
        filters.push("hqdn3d=1.5:1.5:6:6".into());
    }
    filters.join(",")
}

/// The audio bitrate to use when the video can be copied as-is: the whole file (no trim),
/// already what a re-encode would make (the chosen codec, plain 8-bit 4:2:0 SDR, upright,
/// at most 60 fps), and small enough that it plus a re-encoded audio track fits. The
/// planner only scales to save bits, so a video that fits needs no scaling. `None` when
/// the normal re-encode is the way.
fn copy_plan(info: &MediaInfo, trim: Option<Trim>, codec: VideoCodec, target: u64) -> Option<u32> {
    let v = info.video.as_ref()?;
    let plain = v.codec == codec.probe_name()
        && matches!(v.pix_fmt.as_str(), "yuv420p" | "yuvj420p")
        && v.hdr.is_none()
        && !v.rotated
        && v.fps <= 60.01;
    if trim.is_some() || info.audio.is_none() || !plain {
        return None;
    }
    plan::copy_audio_kbps(target, info.duration, v.bitrate_kbps?)
}

/// Only the audio makes it too big: copy the video stream untouched (fast, and the picture
/// keeps every bit) and re-encode just the audio. Errors when it doesn't fit after all.
fn copy_video(
    ctx: &Ctx,
    info: &MediaInfo,
    audio_kbps: u32,
    target: u64,
    out: &Path,
    progress: Progress,
) -> Result<PathBuf> {
    let mut a = input_args(&info.path, None);
    a.extend(args(["-map", "0:v:0", "-c:v", "copy"]));
    audio_args(&mut a, info, audio_kbps);
    finish(&mut a, ctx.codec, out);
    ctx.ff.run(&a, info.duration, progress, ctx.cancel)?;
    let size = file_size(out)?;
    if size <= target {
        return Ok(out.to_path_buf());
    }
    let _ = std::fs::remove_file(out);
    Err(Error::DoesNotFit {
        what: name_of(&info.path),
        target,
        best: size,
    })
}

/// AAC for the first audio track; mono when the bitrate is too low for stereo.
fn audio_args(a: &mut Vec<OsString>, info: &MediaInfo, kbps: u32) {
    if let Some(audio) = &info.audio {
        a.extend(args(["-map", "0:a:0", "-c:a", "aac", "-b:a"]));
        a.push(format!("{kbps}k").into());
        if plan::mono(kbps, audio.channels) {
            a.extend(args(["-ac", "1"]));
        }
    }
}

/// The H.265 tag, faststart and the output path.
fn finish(a: &mut Vec<OsString>, codec: VideoCodec, out: &Path) {
    if codec == VideoCodec::H265 {
        // The tag Apple devices (and most players) expect for H.265 in MP4.
        a.extend(args(["-tag:v", "hvc1"]));
    }
    a.extend(args(["-movflags", "+faststart"]));
    a.push(out.into());
}

struct Job<'a, 'ctx> {
    ctx: &'a Ctx<'ctx>,
    info: &'a MediaInfo,
    trim: Option<Trim>,
    secs: f64,
    target: u64,
    filters: String,
    /// HDR tone-mapped to SDR (the output gets BT.709 tags).
    tonemap: bool,
    decode_threads: String,
    filter_threads: String,
    x264_threads: String,
    out: PathBuf,
}

impl Job<'_, '_> {
    /// Single pass on the GPU, with one retry at a lower bitrate if it overshoots.
    fn run_hw(&self, enc: Encoder, mut plan: VideoPlan, progress: Progress) -> Result<PathBuf> {
        plan.video_kbps = (plan.video_kbps as f64 * HW_HEADROOM) as u32;
        let mut extras = enc.quality_args(self.ctx.codec);
        let mut last = 0;
        for attempt in 0..2 {
            let (from, to) = if attempt == 0 { (0.0, 1.0) } else { (0.5, 1.0) };
            let mut p = scaled(&mut *progress, from, to);
            match self.hw_pass(enc, extras, &plan, &mut p) {
                // An older card or driver that refuses AQ or lookahead: the same encode
                // without them, still on the GPU (see `gpu::works` for why).
                Err(Error::Ffmpeg(_)) if !extras.is_empty() => {
                    extras = &[];
                    self.hw_pass(enc, extras, &plan, &mut p)?;
                }
                result => result?,
            }
            match self.check(&mut plan)? {
                Ok(p) => return Ok(p),
                Err(size) => last = size,
            }
        }
        Err(self.too_big(last))
    }

    /// One GPU encode at the plan's bitrate, plus `extras` (`Encoder::quality_args`).
    fn hw_pass(
        &self,
        enc: Encoder,
        extras: &[&str],
        plan: &VideoPlan,
        progress: Progress,
    ) -> Result<()> {
        let name = enc
            .ffmpeg_name(self.ctx.codec)
            .ok_or_else(|| Error::Unsupported("no GPU encoder for this codec".into()))?;
        let mut a = self.head();
        a.extend(args(["-c:v", name, "-pix_fmt", enc.pix_fmt()]));
        a.extend(args(enc.rate_args(plan.video_kbps)));
        a.extend(args(extras));
        self.tail(&mut a, plan);
        self.ctx.ff.run(&a, self.secs, progress, self.ctx.cancel)
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

    /// Audio, colour tags, container flags and output path.
    fn tail(&self, a: &mut Vec<OsString>, plan: &VideoPlan) {
        audio_args(a, self.info, plan.audio_kbps);
        if self.tonemap {
            a.extend(hdr::sdr_tags());
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
        finish(a, self.ctx.codec, &self.out);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{AudioStream, Hdr, MediaKind};
    use crate::size::MB;

    /// 10 s of 1080p30 H.264 at `video_kbps`, with stereo audio.
    fn clip(video_kbps: u32) -> MediaInfo {
        MediaInfo {
            path: "clip.mkv".into(),
            size_bytes: 30 * MB,
            kind: MediaKind::Video,
            duration: 10.0,
            video: Some(VideoStream {
                width: 1920,
                height: 1080,
                fps: 30.0,
                codec: "h264".into(),
                pix_fmt: "yuv420p".into(),
                bitrate_kbps: Some(video_kbps),
                hdr: None,
                rotated: false,
            }),
            audio: Some(AudioStream {
                codec: "pcm_s16le".into(),
                bitrate_kbps: Some(1536),
                channels: 2,
            }),
            has_alpha: false,
        }
    }

    #[test]
    fn copies_the_video_only_when_it_is_already_what_we_would_make() {
        let target = 2 * MB;
        let ok = clip(1200);
        assert_eq!(copy_plan(&ok, None, VideoCodec::H264, target), Some(128));

        let with = |change: fn(&mut VideoStream)| {
            let mut info = clip(1200);
            change(info.video.as_mut().unwrap());
            copy_plan(&info, None, VideoCodec::H264, target)
        };
        // Too big on its own, size unknown, or no audio to blame.
        assert_eq!(copy_plan(&clip(1700), None, VideoCodec::H264, target), None);
        assert_eq!(with(|v| v.bitrate_kbps = None), None);
        let mut silent = clip(1200);
        silent.audio = None;
        assert_eq!(copy_plan(&silent, None, VideoCodec::H264, target), None);
        // Not what a re-encode would make: other codec, 10-bit, HDR, rotated, over 60 fps.
        assert_eq!(copy_plan(&ok, None, VideoCodec::H265, target), None);
        assert_eq!(with(|v| v.codec = "vp9".into()), None);
        assert_eq!(with(|v| v.pix_fmt = "yuv420p10le".into()), None);
        assert_eq!(with(|v| v.hdr = Some(Hdr::Pq)), None);
        assert_eq!(with(|v| v.rotated = true), None);
        assert_eq!(with(|v| v.fps = 120.0), None);
        // A trimmed part is cut and re-encoded.
        let trim = Trim::new(1.0, 5.0).ok();
        assert_eq!(copy_plan(&ok, trim, VideoCodec::H264, target), None);
    }

    #[test]
    fn tone_map_runs_on_the_kept_frames_at_the_final_size() {
        let mut v = clip(1000).video.unwrap();
        v.fps = 60.0;
        let plan = VideoPlan {
            width: 1280,
            height: 720,
            fps: 30.0,
            video_kbps: 1000,
            audio_kbps: 64,
            quality: crate::Quality::Okay,
            fits: true,
        };
        let f = filters(&v, &plan, None, true);
        let at = |s: &str| f.find(s).unwrap_or_else(|| panic!("{s} missing in {f}"));
        assert!(at("fps=30.000") < at("scale=1280:720"));
        assert!(at("scale=1280:720") < at("zscale=t=linear"));
        assert!(at("format=yuv420p") < at("hqdn3d"));
        assert!(!filters(&v, &plan, None, false).contains("zscale"));
    }
}
