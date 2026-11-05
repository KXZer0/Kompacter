//! Small evenly spaced JPEG frames for the trimmer's filmstrip, made in one ffmpeg run.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use crate::error::{Error, Result};
use crate::ffmpeg::{args, Ffmpeg};

/// `count` frames, `height` px tall, as JPEG bytes in time order.
pub fn thumbnails(
    ff: &Ffmpeg,
    path: &Path,
    duration: f64,
    count: u32,
    height: u32,
) -> Result<Vec<Vec<u8>>> {
    if duration <= 0.0 || count == 0 {
        return Err(Error::Unsupported("not a video".into()));
    }
    let tmp = tempfile::tempdir()?;
    let pattern = tmp.path().join("t%03d.jpg");
    let mut a = args(["-skip_frame", "nokey", "-i"]);
    a.push(path.into());
    a.extend(args(["-an", "-vf"]));
    // One frame per slice of the clip, scaled down, keyframes only (fast).
    a.push(format!("fps={}/{duration:.3},scale=-2:{height}", count).into());
    a.extend(args(["-frames:v", &count.to_string(), "-q:v", "6"]));
    a.push(pattern.into());
    ff.run(&a, duration, &mut |_| {}, &AtomicBool::new(false))?;

    let mut frames: Vec<_> = std::fs::read_dir(tmp.path())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    frames.sort();
    frames.iter().map(|p| Ok(std::fs::read(p)?)).collect()
}
