//! Short clips as GIFs that fit the limit. GIF size grows with width² × frame rate, so one
//! test encode in the middle of the ladder predicts which step fits; then at most a few
//! more encodes walk down until it does.

use std::path::{Path, PathBuf};

use super::{file_size, input_args, name_of, with_ext, Ctx};
use crate::error::{Error, Result};
use crate::ffmpeg::{args, scaled, Progress};
use crate::plan::Trim;
use crate::probe::MediaInfo;

/// Longer clips make huge GIFs that look bad: offer video instead.
pub const MAX_GIF_SECONDS: f64 = 30.0;

/// Width and frame rate, best first.
const LADDER: [(u32, f64); 13] = [
    (800, 20.0),
    (720, 15.0),
    (640, 15.0),
    (540, 15.0),
    (480, 15.0),
    (400, 15.0),
    (360, 12.0),
    (320, 12.0),
    (270, 10.0),
    (240, 10.0),
    (200, 10.0),
    (160, 8.0),
    (120, 8.0),
];
/// The test encode: the middle of the ladder.
const PROBE: usize = 5;

/// Rough bytes per pixel per frame of a dithered GIF of everyday footage.
const BYTES_PER_PIXEL_FRAME: f64 = 0.3;

fn cost(width: u32, fps: f64) -> f64 {
    width as f64 * width as f64 * fps
}

/// The ladder for this clip: never wider than the source or faster than its frame rate.
fn ladder(info: &MediaInfo) -> Vec<(u32, f64)> {
    let (sw, sfps) = info
        .video
        .as_ref()
        .map(|v| (v.width, if v.fps > 0.0 { v.fps } else { 30.0 }))
        .unwrap_or((640, 30.0));
    let mut out: Vec<(u32, f64)> = LADDER
        .iter()
        .map(|&(w, f)| (w.min(sw) & !1, f.min(sfps)))
        .collect();
    out.dedup();
    out
}

/// A quick guess for the trimmer: the width and frame rate a GIF of `seconds` would get.
pub fn guess(info: &MediaInfo, seconds: f64, target: u64) -> Option<(u32, f64)> {
    let v = info.video.as_ref()?;
    let aspect = v.height as f64 / v.width.max(1) as f64;
    ladder(info).into_iter().find(|&(w, f)| {
        let px = w as f64 * (w as f64 * aspect);
        px * f * seconds * BYTES_PER_PIXEL_FRAME <= target as f64
    })
}

fn encode(
    ctx: &Ctx,
    info: &MediaInfo,
    trim: Option<Trim>,
    (width, fps): (u32, f64),
    secs: f64,
    out: &Path,
    progress: Progress,
) -> Result<u64> {
    // One pass with a palette made for this clip; "diff" stats and rectangle updates keep
    // still areas cheap.
    let filter = format!(
        "[0:v:0]fps={fps},scale={width}:-2:flags=lanczos,split[a][b];\
         [a]palettegen=max_colors=256:stats_mode=diff[p];\
         [b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle[g]"
    );
    let mut a = input_args(&info.path, trim);
    a.extend(args(["-filter_complex"]));
    a.push(filter.into());
    a.extend(args(["-map", "[g]", "-an"]));
    a.extend(args(["-loop", "0", "-f", "gif"]));
    a.push(out.as_os_str().to_owned());
    ctx.ff.run(&a, secs, progress, ctx.cancel)?;
    file_size(out)
}

pub(crate) fn fit(
    ctx: &Ctx,
    info: &MediaInfo,
    trim: Option<Trim>,
    target: u64,
    out_base: &Path,
    progress: Progress,
) -> Result<PathBuf> {
    if info.video.is_none() {
        return Err(Error::Unsupported("a GIF needs a video".into()));
    }
    let secs = trim.map(|t| t.len()).unwrap_or(info.duration);
    if secs > MAX_GIF_SECONDS + 0.05 {
        return Err(Error::TooLong { seconds: secs });
    }
    let steps = ladder(info);
    let out = with_ext(out_base, "gif");

    // Test encode in the middle, then predict the best step that fits.
    let probe = PROBE.min(steps.len() - 1);
    let size = encode(
        ctx,
        info,
        trim,
        steps[probe],
        secs,
        &out,
        &mut scaled(&mut *progress, 0.0, 0.4),
    )?;
    let per_cost = size as f64 / cost(steps[probe].0, steps[probe].1);
    let pick = steps
        .iter()
        .position(|&(w, f)| per_cost * cost(w, f) <= target as f64 * 0.92)
        .unwrap_or(steps.len() - 1);
    if pick == probe && size <= target {
        progress(1.0);
        return Ok(out);
    }

    // From the predicted step down, at most four more encodes.
    let mut best = size;
    for (attempt, &step) in steps[pick..].iter().take(4).enumerate() {
        let from = 0.4 + 0.15 * attempt as f32;
        let size = encode(
            ctx,
            info,
            trim,
            step,
            secs,
            &out,
            &mut scaled(&mut *progress, from, (from + 0.15).min(1.0)),
        )?;
        best = best.min(size);
        if size <= target {
            progress(1.0);
            return Ok(out);
        }
    }
    let _ = std::fs::remove_file(&out);
    Err(Error::DoesNotFit {
        what: name_of(&info.path),
        target,
        best,
    })
}
