//! Video: two-pass H.264 + AAC in MP4 aimed at an exact size. Plays inline everywhere.

use std::path::{Path, PathBuf};

use super::{file_size, input_args, name_of, with_ext, Ctx};
use crate::error::{Error, Result};
use crate::ffmpeg::{args, scaled, Progress};
use crate::plan::{plan_video, Trim};
use crate::probe::MediaInfo;
use crate::Speed;

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
    let mut plan = plan_video(target, secs, v.width, v.height, v.fps, info.audio.is_some());
    if !plan.fits {
        return Err(Error::TooLong { seconds: secs });
    }

    let out = with_ext(out_base, "mp4");
    let tmp = tempfile::tempdir()?;
    let passlog = tmp.path().join("x264");
    let preset = match ctx.speed {
        Speed::Fast => "veryfast",
        Speed::Balanced => "fast",
        Speed::Quality => "slow",
    };

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

    // Two tries: if the first result overshoots, scale the bitrate down and go again.
    let mut last = 0;
    for attempt in 0..2 {
        let common = |pass: &str| {
            let mut a = input_args(&info.path, trim);
            a.extend(args([
                "-map", "0:v:0", "-c:v", "libx264", "-preset", preset,
            ]));
            a.extend(args(["-pix_fmt", "yuv420p", "-b:v"]));
            a.push(format!("{}k", plan.video_kbps).into());
            if !filters.is_empty() {
                a.extend(args(["-vf".to_string(), filters.join(",")]));
            }
            a.extend(args(["-pass", pass, "-passlogfile"]));
            a.push(passlog.clone().into());
            a
        };

        let (p1_end, p2_start) = if attempt == 0 {
            (0.4, 0.4)
        } else {
            (0.95, 0.95)
        };
        let mut pass1 = common("1");
        pass1.extend(args(["-an", "-f", "null", "-"]));
        let p1_start = if attempt == 0 { 0.0 } else { 0.9 };
        ctx.ff.run(
            &pass1,
            secs,
            &mut scaled(&mut *progress, p1_start, p1_end),
            ctx.cancel,
        )?;

        let mut pass2 = common("2");
        if info.audio.is_some() {
            pass2.extend(args(["-map", "0:a:0", "-c:a", "aac", "-b:a"]));
            pass2.push(format!("{}k", plan.audio_kbps).into());
        }
        pass2.extend(args(["-movflags", "+faststart"]));
        pass2.push(out.clone().into());
        ctx.ff.run(
            &pass2,
            secs,
            &mut scaled(&mut *progress, p2_start, 1.0),
            ctx.cancel,
        )?;

        last = file_size(&out)?;
        if last <= target {
            return Ok(out);
        }
        let ratio = target as f64 / last as f64 * 0.95;
        plan.video_kbps = (plan.video_kbps as f64 * ratio) as u32;
    }

    let _ = std::fs::remove_file(&out);
    Err(Error::DoesNotFit {
        what: name_of(&info.path),
        target,
        best: last,
    })
}
