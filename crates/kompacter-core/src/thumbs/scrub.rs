//! Decode a nearby scrub window with one seek and one FFmpeg child.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{decode_pipes, fit_size, preview_command, read_block, scale_filter, video_output};
use crate::error::{Error, Result};
use crate::ffmpeg::Ffmpeg;

/// RGB24 preview frame with a timestamp on the media timeline.
#[derive(Debug)]
pub struct TimedFrame {
    pub time: f64,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Emit the frame visible at `start`, then original presentation timestamps
/// through `end`. Also emit the first frame at or after `end` when it is within
/// the lookahead. A successful decode covers the window unless the callback
/// asked to stop early. At video EOF the final frame covers the remainder.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode(
    ff: &Ffmpeg,
    path: &Path,
    size: (u32, u32),
    start: f64,
    end: f64,
    height: u32,
    stop: &AtomicBool,
    on_frame: &mut dyn FnMut(TimedFrame) -> bool,
) -> Result<()> {
    if !start.is_finite() || !end.is_finite() || start < 0.0 || end < start {
        return Err(Error::Unsupported("invalid scrub window".into()));
    }
    let (width, height) = fit_size(size, height)?;
    let scale = scale_filter(width, height);
    let start_us = (start * 1e6).round() as i64;
    let mut cmd = preview_command(ff, "4");
    cmd.args([
        "-an",
        "-copyts",
        "-start_at_zero",
        "-noaccurate_seek",
        "-ss",
        &format!("{start:.6}"),
        // Leave room for one frame beyond the window without resampling VFR.
        // With copied timestamps this input limit is on the media timeline.
        "-t",
        &format!("{:.6}", end + 0.5),
        "-i",
    ])
    .arg(path);
    // Keep only the latest preroll frame at zero. The other branch keeps
    // source timestamps intact. Scale after discarding preroll and before
    // interleaving so its queues only hold preview-sized frames.
    let filter = format!(
        "settb=AVTB,setpts=PTS-{start_us},split[preroll][stream];\
         [preroll]setpts=max(PTS\\,0),fps=1:start_time=0:round=up:eof_action=pass,trim=end_frame=1,{scale}[held];\
         [stream]select=gt(t\\,0),{scale}[later];\
         [held][later]interleave=nb_inputs=2:duration=longest"
    );
    video_output(&mut cmd, &filter);
    cmd.args([
        "-enc_time_base",
        "filter",
        "-stats_mux_pre",
        "pipe:2",
        "-stats_mux_pre_fmt",
        "kompacter {n} {pts} {tb}",
        "pipe:1",
    ]);

    let bytes = (width * height * 3) as usize;
    let mut index = 0;
    let mut publish = |relative: f64, pixels| {
        let time = if relative == 0.0 {
            start
        } else {
            (start_us as f64 + relative * 1e6).round() / 1e6
        };
        !stop.load(Ordering::Relaxed)
            && on_frame(TimedFrame {
                time,
                width,
                height,
                pixels,
            })
            && !stop.load(Ordering::Relaxed)
            && time < end
    };
    decode_pipes(ff, &mut cmd, stop, Stdio::piped(), |stdout, stderr| {
        let mut timestamps = BufReader::new(stderr.expect("stderr piped"));
        let mut line = Vec::new();
        let mut pending = None;
        loop {
            line.clear();
            if timestamps.read_until(b'\n', &mut line)? == 0 {
                return Ok(pending.is_none_or(|(relative, pixels)| publish(relative, pixels)));
            }
            let Some(fields) = line.strip_prefix(b"kompacter ") else {
                continue;
            };
            let relative = packet_time(fields, index)
                .ok_or_else(|| Error::Ffmpeg("invalid scrub timestamp".into()))?;
            let mut pixels = match pending.take() {
                Some((previous, pixels)) if previous == relative => pixels,
                Some((previous, pixels)) if previous < relative => {
                    if !publish(previous, pixels) {
                        return Ok(false);
                    }
                    vec![0; bytes]
                }
                Some(_) => return Err(Error::Ffmpeg("unordered scrub timestamps".into())),
                None => vec![0; bytes],
            };
            // FFmpeg flushes each mux stat before its corresponding RGB packet.
            // Reading the two pipes in that order keeps them in lockstep without
            // a frame queue, stderr thread, or dependence on OS pipe capacity.
            if read_block(stdout, &mut pixels)? != bytes {
                return Err(Error::Ffmpeg("incomplete scrub frame".into()));
            }
            index += 1;
            if relative == 0.0 {
                // The preroll branch already resolved ties at the window start.
                if !publish(relative, pixels) {
                    return Ok(false);
                }
            } else {
                // A source can repeat a PTS. Only the last frame at that PTS
                // becomes visible, so resolve ties before publishing it.
                pending = Some((relative, pixels));
            }
        }
    })?;
    if index == 0 {
        return Err(Error::Unsupported("no frame there".into()));
    }
    Ok(())
}

fn packet_time(fields: &[u8], index: u64) -> Option<f64> {
    let mut fields = std::str::from_utf8(fields).ok()?.split_ascii_whitespace();
    let packet = fields.next()?.parse::<u64>().ok()?;
    let pts = fields.next()?.parse::<i64>().ok()?;
    let (num, den) = fields.next()?.split_once('/')?;
    let num = num.parse::<u32>().ok()?;
    let den = den.parse::<u32>().ok()?;
    (packet == index && pts >= 0 && num > 0 && den > 0)
        .then_some(pts as f64 * f64::from(num) / f64::from(den))
}
