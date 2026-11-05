//! Audio: MP3 for compatibility; Opus when the budget is tiny (it sounds far better there).

use std::path::{Path, PathBuf};

use super::{file_size, input_args, name_of, with_ext, Ctx};
use crate::error::{Error, Result};
use crate::ffmpeg::{args, Progress};
use crate::plan::{plan_audio_kbps, Trim};
use crate::probe::MediaInfo;

pub(crate) fn fit(
    ctx: &Ctx,
    info: &MediaInfo,
    trim: Option<Trim>,
    target: u64,
    out_base: &Path,
    progress: Progress,
) -> Result<PathBuf> {
    let secs = trim.map(|t| t.len()).unwrap_or(info.duration);
    let kbps = plan_audio_kbps(target, secs).ok_or(Error::TooLong { seconds: secs })?;
    let (codec, ext) = if kbps >= 64 {
        ("libmp3lame", "mp3")
    } else {
        ("libopus", "ogg")
    };

    let out = with_ext(out_base, ext);
    // Two tries: if the first result overshoots, lower the bitrate and go again.
    let mut kbps = kbps;
    let mut size = 0;
    for _ in 0..2 {
        let mut a = input_args(&info.path, trim);
        a.extend(args(["-map", "0:a:0", "-vn", "-c:a", codec, "-b:a"]));
        a.push(format!("{kbps}k").into());
        if codec == "libopus" {
            // Plain VBR can overshoot a lot on simple audio.
            a.extend(args(["-vbr", "constrained"]));
        }
        a.push(out.clone().into());
        ctx.ff.run(&a, secs, &mut *progress, ctx.cancel)?;

        size = file_size(&out)?;
        if size <= target {
            return Ok(out);
        }
        kbps = ((kbps as f64 * target as f64 / size as f64 * 0.95) as u32).max(8);
    }
    let _ = std::fs::remove_file(&out);
    Err(Error::DoesNotFit {
        what: name_of(&info.path),
        target,
        best: size,
    })
}
