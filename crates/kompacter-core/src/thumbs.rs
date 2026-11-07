//! Bounded, cancellable FFmpeg decodes for the trimmer.

use std::io::{self, Read};
use std::path::Path;
use std::process::{ChildStderr, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::ffmpeg::{args, command, Ffmpeg};
use crate::probe::VideoStream;

pub(crate) mod scrub;

const POLL: Duration = Duration::from_millis(5);
pub const PCM_RATE: u32 = 48_000;
const PCM_BLOCK: usize = PCM_RATE as usize / 50 * 4; // 20 ms, stereo s16le

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
    stop: &AtomicBool,
) -> Result<Frames> {
    if duration <= 0.0 || count == 0 {
        return Err(Error::Unsupported("not a video".into()));
    }
    let (width, height) = fit_size(size, height)?;
    let mut cmd = preview_command(ff, "2");
    cmd.args(["-skip_frame", "nokey", "-i"]).arg(path);
    let scale = scale_filter(width, height);
    let fps = f64::from(count) / duration;
    video_output(&mut cmd, &format!("fps={fps}:start_time=0,{scale}"));
    cmd.args(["-frames:v", &count.to_string(), "pipe:1"]);
    let mut rgb = Vec::new();
    read_frames(ff, &mut cmd, width, height, stop, |frame| {
        rgb.push(frame.to_vec());
        true
    })?;
    Ok(Frames { width, height, rgb })
}

/// One frame at `t` seconds (or the last available video frame at the end).
pub fn frame_at(
    ff: &Ffmpeg,
    path: &Path,
    size: (u32, u32),
    t: f64,
    height: u32,
    stop: &AtomicBool,
) -> Result<Frames> {
    let (width, height) = fit_size(size, height)?;
    let scale = scale_filter(width, height);
    let seek = format!("{:.6}", t.max(0.0));
    let mut frame = Vec::new();

    let mut cmd = preview_command(ff, "4");
    cmd.args(["-noaccurate_seek", "-ss", &seek, "-i"]).arg(path);
    // Keep keyframe preroll. Fold frames at or before the seek point onto time
    // zero and round later frames up, so the first tick gets the last frame
    // that was actually visible. This also holds the final frame through EOF.
    video_output(
        &mut cmd,
        &format!("setpts=max(PTS\\,0),fps=1:start_time=0:round=up:eof_action=pass,trim=end_frame=1,{scale}"),
    );
    cmd.args(["-frames:v", "1", "pipe:1"]);
    read_frames(ff, &mut cmd, width, height, stop, |pixels| {
        frame.extend_from_slice(pixels);
        true
    })?;

    if frame.is_empty() {
        return Err(Error::Unsupported("no frame there".into()));
    }
    Ok(Frames {
        width,
        height,
        rgb: vec![frame],
    })
}

/// Play against the supplied clock. FFmpeg samples source presentation timestamps
/// onto a regular preview cadence, and only frames close to their deadline are sent.
#[allow(clippy::too_many_arguments)]
pub fn play(
    ff: &Ffmpeg,
    path: &Path,
    video: &VideoStream,
    start: f64,
    end: f64,
    height: u32,
    fps: f64,
    stop: &AtomicBool,
    elapsed: &mut dyn FnMut() -> Option<f64>,
    on_frame: &mut dyn FnMut(f64, u32, u32, Vec<u8>) -> bool,
) -> Result<()> {
    let (width, height) = fit_size((video.width, video.height), height)?;
    let duration = end - start;
    if !duration.is_finite() || duration <= 0.0 || !fps.is_finite() || fps <= 0.0 {
        return Err(Error::Unsupported(
            "invalid preview range or frame rate".into(),
        ));
    }
    // Frame-threaded decoders allocate a full-resolution surface per thread.
    // Keep extra headroom for more expensive codecs and high-frame-rate clips.
    let pixels = u64::from(video.width) * u64::from(video.height);
    let h264 = video.codec == "h264";
    let threads = if pixels <= 1280 * 720 || (h264 && video.fps <= 30.0) {
        "2"
    } else {
        "4"
    };
    let mut cmd = preview_command(ff, threads);
    if h264 && pixels > 1920 * 1080 {
        // Non-reference pictures cannot propagate deblocking differences to
        // later frames. Scaling the preview down hides nearly all of them.
        cmd.args(["-skip_loop_filter", "noref"]);
    }
    cmd.args([
        "-noaccurate_seek",
        "-ss",
        &format!("{:.6}", start.max(0.0)),
        "-i",
    ])
    .arg(path)
    .args(["-t", &format!("{duration:.6}")]);
    let scale = scale_filter(width, height);
    video_output(
        &mut cmd,
        &format!("setpts=max(PTS\\,0),fps={fps}:start_time=0:round=up:eof_action=pass,{scale}"),
    );
    cmd.arg("pipe:1");

    let mut index = 0u64;
    let mut reached_end = false;
    let mut consumer_stopped = false;
    read_frames(ff, &mut cmd, width, height, stop, |frame| {
        let due = index as f64 / fps;
        index += 1;
        let Some(now) = wait_until(elapsed, due.min(duration), stop) else {
            return false;
        };
        reached_end = now >= duration;
        if !reached_end && now - due < 1.0 / fps {
            consumer_stopped = !on_frame(start + due, width, height, frame.to_vec());
        }
        !reached_end && !consumer_stopped
    })?;
    // The last frame can precede the selection end, especially for VFR media.
    if !reached_end && !consumer_stopped {
        wait_until(elapsed, duration, stop).ok_or(Error::Cancelled)?;
    }
    Ok(())
}

fn wait_until(
    elapsed: &mut dyn FnMut() -> Option<f64>,
    due: f64,
    stop: &AtomicBool,
) -> Option<f64> {
    while !stop.load(Ordering::Relaxed) {
        let wait = if let Some(now) = elapsed() {
            if now >= due {
                return Some(now);
            }
            Duration::from_secs_f64((due - now).min(POLL.as_secs_f64()))
        } else {
            POLL
        };
        std::thread::sleep(wait);
    }
    None
}

/// Stream 48 kHz stereo s16le aligned to the requested timeline. Silence fills
/// stream gaps and extends a short audio track to the selected end.
pub fn audio_pcm(
    ff: &Ffmpeg,
    path: &Path,
    start: f64,
    end: f64,
    stop: &AtomicBool,
    on_samples: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<()> {
    let mut cmd = preview_command(ff, "1");
    cmd.args(["-vn", "-ss", &format!("{:.6}", start.max(0.0)), "-i"])
        .arg(path)
        .args([
            "-t",
            &format!("{:.6}", end - start),
            "-map",
            "0:a:0",
            "-af",
            "aresample=48000:async=1:first_pts=0,apad",
            "-ac",
            "2",
            "-c:a",
            "pcm_s16le",
            "-f",
            "s16le",
            "pipe:1",
        ]);
    decode(ff, &mut cmd, stop, |stdout| {
        let mut pcm = [0; PCM_BLOCK];
        loop {
            let n = read_block(stdout, &mut pcm)?;
            let samples = &pcm[..n - n % 4];
            if !samples.is_empty() && !on_samples(samples) {
                return Ok(false);
            }
            if n < pcm.len() {
                return Ok(true);
            }
        }
    })
}

fn read_frames(
    ff: &Ffmpeg,
    cmd: &mut Command,
    width: u32,
    height: u32,
    stop: &AtomicBool,
    mut on_frame: impl FnMut(&[u8]) -> bool,
) -> Result<()> {
    decode(ff, cmd, stop, |stdout| {
        let mut frame = vec![0; (width * height * 3) as usize];
        while read_block(stdout, &mut frame)? == frame.len() {
            if !on_frame(&frame) {
                return Ok(false);
            }
        }
        Ok(true)
    })
}

fn read_block(stdout: &mut ChildStdout, mut buf: &mut [u8]) -> io::Result<usize> {
    let len = buf.len();
    while !buf.is_empty() {
        match stdout.read(buf) {
            Ok(0) => break,
            Ok(n) => buf = &mut buf[n..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(len - buf.len())
}

/// The monitor owns the child so cancellation interrupts a blocked pipe read.
/// A consumer returning false stops the child immediately and still reaps it.
fn decode(
    ff: &Ffmpeg,
    cmd: &mut Command,
    stop: &AtomicBool,
    read: impl FnOnce(&mut ChildStdout) -> Result<bool>,
) -> Result<()> {
    decode_pipes(ff, cmd, stop, Stdio::null(), |stdout, _| read(stdout))
}

fn decode_pipes(
    ff: &Ffmpeg,
    cmd: &mut Command,
    stop: &AtomicBool,
    stderr: Stdio,
    read: impl FnOnce(&mut ChildStdout, Option<ChildStderr>) -> Result<bool>,
) -> Result<()> {
    if stop.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(stderr)
        .spawn()
        .map_err(|_| Error::FfmpegMissing(ff.ffmpeg.display().to_string()))?;
    let mut stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take();
    let abort = AtomicBool::new(false);
    let (output, status) = std::thread::scope(|scope| {
        let monitor = scope.spawn(|| loop {
            if stop.load(Ordering::Relaxed) || abort.load(Ordering::Relaxed) {
                let _ = child.kill();
                return child.wait();
            }
            match child.try_wait() {
                Ok(Some(status)) => return Ok(status),
                Ok(None) => std::thread::park_timeout(POLL),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            }
        });
        let output = read(&mut stdout, stderr);
        abort.store(!matches!(output, Ok(true)), Ordering::Relaxed);
        monitor.thread().unpark();
        (output, monitor.join().expect("preview monitor panicked"))
    });
    if stop.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    if output? && !status?.success() {
        return Err(Error::Ffmpeg("preview decode failed".into()));
    }
    Ok(())
}

fn preview_command(ff: &Ffmpeg, decoder_threads: &str) -> Command {
    let mut cmd = command(&ff.ffmpeg);
    cmd.args([
        "-hide_banner",
        "-nostdin",
        "-loglevel",
        "error",
        "-filter_threads",
        "1",
        "-threads",
        decoder_threads,
    ]);
    cmd
}

fn video_output(cmd: &mut Command, filter: &str) {
    cmd.args([
        "-map",
        "0:V:0",
        "-an",
        "-vf",
        filter,
        "-fps_mode",
        "passthrough",
        "-threads",
        "1",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgb24",
    ]);
}

fn scale_filter(width: u32, height: u32) -> String {
    format!("scale={width}:{height}:flags=fast_bilinear")
}

/// Even dimensions that keep the video's shape within the preview pixel budget.
fn fit_size((width, height): (u32, u32), max_height: u32) -> Result<(u32, u32)> {
    if width == 0 || height == 0 {
        return Err(Error::Unsupported("not a video".into()));
    }
    let max_height = max_height.clamp(2, 360);
    let max_width = max_height * 16 / 9;
    let factor = (f64::from(max_height) / f64::from(height))
        .min(f64::from(max_width) / f64::from(width))
        .min(1.0);
    let even = |n| ((f64::from(n) * factor / 2.0).round() as u32 * 2).max(2);
    Ok((even(width), even(height)))
}

/// Extract a WAV for existing core callers. Preview playback streams PCM instead.
pub fn wav(
    ff: &Ffmpeg,
    path: &Path,
    start: f64,
    end: f64,
    out: &Path,
    stop: &AtomicBool,
) -> Result<()> {
    let mut a = args([
        "-ss",
        &format!("{:.3}", start.max(0.0)),
        "-t",
        &format!("{:.3}", (end - start).max(0.1)),
        "-i",
    ]);
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
    ff.run(&a, end - start, &mut |_| {}, stop)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn ffmpeg() -> Option<Ffmpeg> {
        let mut ff = Ffmpeg::default();
        if std::env::var_os("KOMPACTER_FFMPEG").is_none() {
            let bundled = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/release")
                .join(format!("ffmpeg{}", std::env::consts::EXE_SUFFIX));
            if bundled.is_file() {
                ff.ffmpeg = bundled;
            }
        }
        command(&ff.ffmpeg)
            .arg("-version")
            .stdout(Stdio::null())
            .status()
            .ok()
            .filter(|status| status.success())
            .map(|_| ff)
    }

    fn fixture(ff: &Ffmpeg) -> (tempfile::TempDir, std::path::PathBuf) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        std::fs::create_dir_all(&root).unwrap();
        let dir = tempfile::tempdir_in(root).unwrap();
        let path = dir.path().join("preview.mkv");
        let status = command(&ff.ffmpeg)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=96x64:r=8:d=1",
                "-itsoffset",
                "0.35",
                "-f",
                "lavfi",
                "-i",
                "sine=d=0.8",
                "-c:v",
                "ffv1",
                "-g",
                "4",
                "-c:a",
                "pcm_s16le",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        (dir, path)
    }

    #[test]
    fn preview_seek_at_eof_returns_last_video_frame() {
        let Some(ff) = ffmpeg() else { return };
        let (_dir, path) = fixture(&ff);
        let stop = AtomicBool::new(false);
        let before = frame_at(&ff, &path, (96, 64), 0.875, 64, &stop).unwrap();
        let eof = frame_at(&ff, &path, (96, 64), 1.15, 64, &stop).unwrap();
        assert_eq!(before.rgb, eof.rgb);
    }

    #[test]
    fn preview_seek_keeps_the_visible_frame_inside_a_vfr_gap() {
        let Some(ff) = ffmpeg() else { return };
        let (_dir, input) = fixture(&ff);
        let path = input.with_file_name("vfr.mkv");
        let status = command(&ff.ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(input)
            .args([
                "-an",
                "-vf",
                "select=eq(n\\,0)+eq(n\\,6)",
                "-fps_mode",
                "vfr",
                "-c:v",
                "ffv1",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        let stop = AtomicBool::new(false);
        let seek = |time| frame_at(&ff, &path, (96, 64), time, 64, &stop).unwrap().rgb;
        assert_eq!(seek(0.5), seek(0.0));
        assert_ne!(seek(0.75), seek(0.5));
    }

    #[test]
    fn scrub_window_brackets_vfr_targets_and_holds_video_through_audio_eof() {
        let Some(ff) = ffmpeg() else { return };
        let (_dir, input) = fixture(&ff);
        let path = input.with_file_name("vfr-audio.mkv");
        let status = command(&ff.ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(input)
            .args([
                "-vf",
                "select=eq(n\\,0)+eq(n\\,6)",
                "-fps_mode",
                "vfr",
                "-c:v",
                "ffv1",
                "-c:a",
                "copy",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());

        let stop = AtomicBool::new(false);
        let decode = |start, end| {
            let mut frames = Vec::new();
            scrub::decode(&ff, &path, (96, 64), start, end, 64, &stop, &mut |frame| {
                frames.push(frame);
                true
            })
            .unwrap();
            frames
        };
        let nearby = decode(0.5, 0.7);
        assert_eq!(
            nearby.iter().map(|f| f.time).collect::<Vec<_>>(),
            [0.5, 0.75]
        );
        assert_ne!(nearby[0].pixels, nearby[1].pixels);
        let seek = frame_at(&ff, &path, (96, 64), 0.5, 64, &stop).unwrap();
        assert_eq!(nearby[0].pixels, seek.rgb[0]);
        let tail = decode(1.05, 1.15);
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].time, 1.05);
        assert_eq!(tail[0].pixels, nearby[1].pixels);
    }

    #[test]
    fn scrub_window_prefers_last_frame_at_equal_timestamps() {
        let Some(ff) = ffmpeg() else { return };
        let (_dir, input) = fixture(&ff);
        let path = input.with_file_name("duplicate-pts.mkv");
        let status = command(&ff.ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(&input)
            .args([
                "-an",
                "-vf",
                "setpts=if(eq(N\\,2)\\,1/(8*TB)\\,PTS)",
                "-fps_mode",
                "passthrough",
                "-enc_time_base",
                "filter",
                "-c:v",
                "ffv1",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());

        let stop = AtomicBool::new(false);
        let mut frames = Vec::new();
        scrub::decode(&ff, &path, (96, 64), 0.05, 0.3, 64, &stop, &mut |frame| {
            frames.push(frame);
            true
        })
        .unwrap();
        assert_eq!(
            frames.iter().map(|f| f.time).collect::<Vec<_>>(),
            [0.05, 0.125, 0.375]
        );
        let last = frame_at(&ff, &input, (96, 64), 0.25, 64, &stop).unwrap();
        assert_eq!(frames[1].pixels, last.rgb[0]);
    }

    #[test]
    fn scrub_window_cancels_during_frame_delivery() {
        let Some(ff) = ffmpeg() else { return };
        let (_dir, path) = fixture(&ff);
        let stop = AtomicBool::new(false);
        let mut delivered = 0;
        let result = scrub::decode(&ff, &path, (96, 64), 0.0, 0.9, 64, &stop, &mut |_| {
            delivered += 1;
            stop.store(true, Ordering::Relaxed);
            true
        });
        assert!(matches!(result, Err(Error::Cancelled)));
        assert_eq!(delivered, 1);
    }

    #[test]
    fn preview_pcm_keeps_audio_cues_on_the_selection_timeline() {
        let Some(ff) = ffmpeg() else { return };
        let (_dir, path) = fixture(&ff);
        let mut pcm = Vec::new();
        audio_pcm(
            &ff,
            &path,
            0.1,
            1.2,
            &AtomicBool::new(false),
            &mut |samples| {
                assert!(samples.len() <= PCM_BLOCK);
                pcm.extend_from_slice(samples);
                true
            },
        )
        .unwrap();
        let samples = |seconds: f64| (seconds * f64::from(PCM_RATE)).round() as usize * 4;
        assert_eq!(pcm.len(), samples(1.1));
        assert!(pcm[..samples(0.24)].iter().all(|&byte| byte == 0));
        assert!(pcm[samples(0.26)..samples(0.27)]
            .iter()
            .any(|&byte| byte != 0));
        assert!(pcm[samples(1.06)..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn preview_play_drops_late_frames_and_finishes_at_range_end() {
        let Some(ff) = ffmpeg() else { return };
        let (_dir, path) = fixture(&ff);
        let mut polls = 0;
        let mut shown = Vec::new();
        let video = VideoStream {
            width: 96,
            height: 64,
            fps: 8.0,
            codec: "ffv1".into(),
        };
        play(
            &ff,
            &path,
            &video,
            0.0,
            0.9,
            64,
            8.0,
            &AtomicBool::new(false),
            &mut || {
                polls += 1;
                Some(match polls {
                    1 => 0.0,
                    2..=5 => 0.5,
                    _ => 0.9,
                })
            },
            &mut |time, _, _, _| {
                shown.push(time);
                true
            },
        )
        .unwrap();
        assert_eq!(shown, [0.0, 0.5]);
        assert_eq!(polls, 6);
    }

    #[test]
    fn preview_cancel_interrupts_a_blocked_read() {
        let Some(ff) = ffmpeg() else { return };
        let stop = AtomicBool::new(false);
        let began = Instant::now();
        let mut cmd = preview_command(&ff, "1");
        cmd.args([
            "-f",
            "lavfi",
            "-i",
            "color=s=32x32:d=4,realtime,select='gt(t,3)'",
        ]);
        video_output(&mut cmd, "null");
        cmd.arg("pipe:1");
        let result = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(120));
                stop.store(true, Ordering::Relaxed);
            });
            read_frames(&ff, &mut cmd, 32, 32, &stop, |_| panic!("unexpected frame"))
        });
        assert!(matches!(result, Err(Error::Cancelled)));
        assert!(began.elapsed() < Duration::from_secs(2));
    }
}
