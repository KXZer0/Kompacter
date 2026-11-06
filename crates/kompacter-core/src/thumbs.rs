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
    if duration <= 0.0 || count == 0 {
        return Err(Error::Unsupported("not a video".into()));
    }
    let (width, height) = fit_size((vw, vh), height)?;
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

/// One frame at exactly `t` seconds, `height` px tall (for the trimmer's preview).
pub fn frame_at(ff: &Ffmpeg, path: &Path, size: (u32, u32), t: f64, height: u32) -> Result<Frames> {
    let (width, height) = fit_size(size, height)?;
    let tmp = tempfile::tempdir()?;
    let out = tmp.path().join("frame.rgb");
    // `-ss` before `-i` seeks fast and, since ffmpeg 2.1, still lands on the exact frame.
    let mut a = args(["-ss"]);
    a.push(format!("{:.3}", t.max(0.0)).into());
    a.extend(args(["-i"]));
    a.push(path.into());
    a.extend(args(["-an", "-frames:v", "1", "-vf"]));
    a.push(format!("scale={width}:{height}").into());
    a.extend(args(["-f", "rawvideo", "-pix_fmt", "rgb24"]));
    a.push(out.clone().into());
    ff.run(&a, 0.0, &mut |_| {}, &AtomicBool::new(false))?;
    let data = std::fs::read(&out)?;
    let frame = (width * height * 3) as usize;
    if data.len() < frame {
        return Err(Error::Unsupported("no frame there".into()));
    }
    Ok(Frames {
        width,
        height,
        rgb: vec![data[..frame].to_vec()],
    })
}

/// Play `start..end` as small RGB frames in real time (silent), for the trimmer. Calls
/// `on_frame(width, height, pixels)` for each; return false (or set `stop`) to stop.
#[allow(clippy::too_many_arguments)]
pub fn play(
    ff: &Ffmpeg,
    path: &Path,
    size: (u32, u32),
    start: f64,
    end: f64,
    height: u32,
    fps: f64,
    stop: &AtomicBool,
    on_frame: &mut dyn FnMut(u32, u32, Vec<u8>) -> bool,
) -> Result<()> {
    use std::io::Read;
    use std::process::Stdio;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    let (width, height) = fit_size(size, height)?;
    let mut child = crate::ffmpeg::command(&ff.ffmpeg)
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-ss"])
        .arg(format!("{:.3}", start.max(0.0)))
        .arg("-t")
        .arg(format!("{:.3}", (end - start).max(0.1)))
        .arg("-i")
        .arg(path)
        .args(["-an", "-vf"])
        .arg(format!("fps={fps},scale={width}:{height}"))
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Error::FfmpegMissing(ff.ffmpeg.display().to_string()))?;
    let mut out = child.stdout.take().expect("stdout piped");
    let frame = (width * height * 3) as usize;
    let began = Instant::now();
    let mut n = 0u32;
    loop {
        let mut buf = vec![0u8; frame];
        if out.read_exact(&mut buf).is_err() || stop.load(Ordering::Relaxed) {
            break;
        }
        // Keep real time: wait until this frame is due.
        let due = Duration::from_secs_f64(n as f64 / fps);
        if let Some(wait) = due.checked_sub(began.elapsed()) {
            std::thread::sleep(wait);
        }
        n += 1;
        if !on_frame(width, height, buf) {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

/// Even width × `height` that keeps the video's shape.
fn fit_size(size: (u32, u32), height: u32) -> Result<(u32, u32)> {
    let (vw, vh) = size;
    if vw == 0 || vh == 0 {
        return Err(Error::Unsupported("not a video".into()));
    }
    let height = height.max(2) & !1;
    let width = ((height as f64 * vw as f64 / vh as f64 / 2.0).round() as u32 * 2).max(2);
    Ok((width, height))
}

/// The audio of `start..end` as a WAV file (for playing along in the trimmer).
pub fn wav(ff: &Ffmpeg, path: &Path, start: f64, end: f64, out: &Path) -> Result<()> {
    let mut a = args(["-ss"]);
    a.push(format!("{:.3}", start.max(0.0)).into());
    a.extend(args(["-t"]));
    a.push(format!("{:.3}", (end - start).max(0.1)).into());
    a.extend(args(["-i"]));
    a.push(path.into());
    a.extend(args([
        "-vn",
        "-ac",
        "2",
        "-ar",
        "44100",
        "-c:a",
        "pcm_s16le",
    ]));
    a.push(out.into());
    ff.run(&a, end - start, &mut |_| {}, &AtomicBool::new(false))
}
