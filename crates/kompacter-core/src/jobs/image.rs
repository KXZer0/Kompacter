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

    let total = (SCALES.len() * JPEG_Q.len()) as f32;
    let mut best = u64::MAX;
    for (si, s) in SCALES.iter().enumerate() {
        let (tw, th) = even(w * base_scale * s, h * base_scale * s);
        for qi in 0..JPEG_Q.len() {
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
            a.push(out.clone().into());
            ctx.ff.run(&a, 0.0, &mut |_| {}, ctx.cancel)?;

            let size = file_size(&out)?;
            progress((si * JPEG_Q.len() + qi + 1) as f32 / total);
            if size <= target {
                progress(1.0);
                return Ok(out);
            }
            best = best.min(size);
        }
    }
    let _ = std::fs::remove_file(&out);
    Err(Error::DoesNotFit {
        what: name_of(&info.path),
        target,
        best,
    })
}

fn even(w: f64, h: f64) -> (u32, u32) {
    let e = |v: f64| ((v.round() as u32) / 2 * 2).max(2);
    (e(w), e(h))
}
