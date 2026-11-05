//! Images: JPEG (or WebP when there's transparency), stepping quality and size down
//! until the result fits. Huge photos are capped at 4096 px on the long side first.

use std::path::{Path, PathBuf};

use super::{file_size, name_of, with_ext, Ctx};
use crate::error::{Error, Result};
use crate::ffmpeg::{args, Progress};
use crate::probe::MediaInfo;

const MAX_SIDE: f64 = 4096.0;
const SCALES: [f64; 5] = [1.0, 0.75, 0.5, 0.35, 0.25];
/// mjpeg `-q:v` (lower is better).
const JPEG_Q: [u32; 4] = [3, 6, 10, 15];
/// libwebp `-quality` (higher is better).
const WEBP_Q: [u32; 4] = [85, 70, 55, 40];

pub(crate) fn fit(
    ctx: &Ctx,
    info: &MediaInfo,
    target: u64,
    out_base: &Path,
    progress: Progress,
) -> Result<PathBuf> {
    let v = info
        .video
        .as_ref()
        .ok_or_else(|| Error::Unsupported("unreadable image".into()))?;
    let (w, h) = (v.width.max(1) as f64, v.height.max(1) as f64);
    let base_scale = (MAX_SIDE / w.max(h)).min(1.0);
    let webp = info.has_alpha;
    let out = with_ext(out_base, if webp { "webp" } else { "jpg" });

    // Every (scale, quality) pair, best-looking first. Size shrinks (roughly) along this
    // ladder, so a binary search finds the best one that fits in ~5 encodes instead of 20.
    let ladder: Vec<(f64, usize)> = SCALES
        .iter()
        .flat_map(|&s| (0..JPEG_Q.len()).map(move |q| (s, q)))
        .collect();
    let encode = |i: usize, dest: &Path| -> Result<u64> {
        let (s, qi) = ladder[i];
        let (tw, th) = even(w * base_scale * s, h * base_scale * s);
        let mut a = args(["-i".into(), info.path.clone().into_os_string()]);
        a.extend(args(["-frames:v", "1", "-vf"]));
        a.push(format!("scale={tw}:{th}:flags=lanczos").into());
        if webp {
            a.extend(args(["-c:v", "libwebp", "-quality"]));
            a.push(WEBP_Q[qi].to_string().into());
        } else {
            a.extend(args(["-c:v", "mjpeg", "-pix_fmt", "yuvj420p", "-q:v"]));
            a.push(JPEG_Q[qi].to_string().into());
        }
        a.push(dest.into());
        ctx.ff.run(&a, 0.0, &mut |_| {}, ctx.cancel)?;
        file_size(dest)
    };

    let tmp = tempfile::tempdir()?;
    let probe_out = tmp.path().join(out.file_name().unwrap_or_default());
    let steps = (ladder.len() as f32).log2().ceil() + 1.0;
    let (mut lo, mut hi) = (0usize, ladder.len() - 1);
    let mut best_fit: Option<usize> = None;
    let mut smallest = u64::MAX;
    let mut done = 0.0;
    while lo <= hi {
        let mid = (lo + hi) / 2;
        let size = encode(mid, &probe_out)?;
        done += 1.0;
        progress((done / steps).min(0.95));
        smallest = smallest.min(size);
        if size <= target {
            best_fit = Some(mid);
            std::fs::copy(&probe_out, &out)?;
            if mid == 0 {
                break;
            }
            hi = mid - 1;
        } else {
            lo = mid + 1;
        }
    }
    if best_fit.is_some() {
        progress(1.0);
        return Ok(out);
    }
    let _ = std::fs::remove_file(&out);
    Err(Error::DoesNotFit {
        what: name_of(&info.path),
        target,
        best: smallest,
    })
}

fn even(w: f64, h: f64) -> (u32, u32) {
    let e = |v: f64| ((v.round() as u32) / 2 * 2).max(2);
    (e(w), e(h))
}
