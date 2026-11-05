//! Small evenly spaced frames for the trimmer's filmstrip, made in one ffmpeg run.
//! Frames come back as raw RGB pixels, so the app needs no image decoder.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use crate::error::{Error, Result};
use crate::ffmpeg::{args, Ffmpeg};

/// Filmstrip frames: all the same size, RGB, 3 bytes per pixel, in time order.
#[derive(Debug, Clone)]
pub struct Frames {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<Vec<u8>>,
}

/// Up to `count` frames, `height` px tall (width follows the video's shape).
pub fn thumbnails(
    ff: &Ffmpeg,
    path: &Path,
    duration: f64,
    size: (u32, u32),
    count: u32,
    height: u32,
) -> Result<Frames> {
    let (vw, vh) = size;
    if duration <= 0.0 || count == 0 || vw == 0 || vh == 0 {
        return Err(Error::Unsupported("not a video".into()));
    }
    let height = height.max(2) & !1;
    let width = ((height as f64 * vw as f64 / vh as f64 / 2.0).round() as u32 * 2).max(2);
    let tmp = tempfile::tempdir()?;
    let out = tmp.path().join("frames.rgb");
    let mut a = args(["-skip_frame", "nokey", "-i"]);
    a.push(path.into());
    a.extend(args(["-an", "-vf"]));
    // One frame per slice of the clip, scaled down, keyframes only (fast).
    a.push(format!("fps={count}/{duration:.3},scale={width}:{height}").into());
    a.extend(args([
        "-frames:v",
        &count.to_string(),
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgb24",
    ]));
    a.push(out.clone().into());
    ff.run(&a, duration, &mut |_| {}, &AtomicBool::new(false))?;

    let data = std::fs::read(&out)?;
    let frame = (width * height * 3) as usize;
    let rgb = data.chunks_exact(frame).map(<[u8]>::to_vec).collect();
    Ok(Frames { width, height, rgb })
}
