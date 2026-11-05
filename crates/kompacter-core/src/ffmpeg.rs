//! Runs ffmpeg/ffprobe as child processes, with progress and cancel support.
//! ffmpeg only runs while a job is active, so the app idles at near-zero CPU.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct Ffmpeg {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl Default for Ffmpeg {
    /// Uses `KOMPACTER_FFMPEG` / `KOMPACTER_FFPROBE` if set, otherwise whatever is on PATH.
    /// The desktop app will point these at its bundled sidecar binaries.
    fn default() -> Self {
        let pick = |var: &str, fallback: &str| {
            std::env::var_os(var)
                .map(PathBuf::from)
                .unwrap_or_else(|| fallback.into())
        };
        Ffmpeg {
            ffmpeg: pick("KOMPACTER_FFMPEG", "ffmpeg"),
            ffprobe: pick("KOMPACTER_FFPROBE", "ffprobe"),
        }
    }
}

/// A quiet child process: on Windows no console window pops up, and it runs at
/// below-normal priority so games and the rest of the PC stay smooth while it works.
pub(crate) fn command(program: &Path) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
        cmd.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
    }
    cmd
}

/// Progress callback: receives 0.0..=1.0.
pub type Progress<'a> = &'a mut dyn FnMut(f32);

impl Ffmpeg {
    /// ffprobe as JSON. `Ok(None)` means ffprobe ran but couldn't read the file (not media).
    pub fn probe_json(&self, path: &Path) -> Result<Option<Value>> {
        let out = command(&self.ffprobe)
            .args([
                "-v",
                "error",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
            ])
            .arg(path)
            .stdin(Stdio::null())
            .output()
            .map_err(|_| Error::FfmpegMissing(self.ffprobe.display().to_string()))?;
        if !out.status.success() {
            return Ok(None);
        }
        serde_json::from_slice(&out.stdout)
            .map(Some)
            .map_err(|e| Error::Probe {
                path: path.into(),
                reason: e.to_string(),
            })
    }

    /// Run ffmpeg with `args`. `duration` (seconds of output) turns ffmpeg's progress
    /// output into a 0..1 fraction. Checks `cancel` on every progress tick.
    pub fn run(
        &self,
        args: &[OsString],
        duration: f64,
        progress: Progress,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let mut child = command(&self.ffmpeg)
            .args([
                "-hide_banner",
                "-nostdin",
                "-y",
                "-loglevel",
                "error",
                "-nostats",
            ])
            .args(["-progress", "pipe:1"])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| Error::FfmpegMissing(self.ffmpeg.display().to_string()))?;

        // Drain stderr on a thread so ffmpeg never blocks on a full pipe.
        let mut stderr = child.stderr.take().expect("stderr piped");
        let err_thread = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s);
            s
        });

        let stdout = child.stdout.take().expect("stdout piped");
        for line in BufReader::new(stdout).lines() {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Cancelled);
            }
            let Ok(line) = line else { break };
            if let Some(us) = line.strip_prefix("out_time_us=") {
                if let (Ok(us), true) = (us.trim().parse::<f64>(), duration > 0.0) {
                    progress((us / 1e6 / duration).clamp(0.0, 1.0) as f32);
                }
            } else if line == "progress=end" {
                progress(1.0);
            }
        }

        let status = child.wait()?;
        let stderr = err_thread.join().unwrap_or_default();
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        if !status.success() {
            let msg = stderr.trim();
            return Err(Error::Ffmpeg(if msg.is_empty() {
                status.to_string()
            } else {
                msg.into()
            }));
        }
        Ok(())
    }
}

/// Small helper to build argument lists without `.into()` noise.
pub(crate) fn args<I, S>(items: I) -> Vec<OsString>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    items.into_iter().map(Into::into).collect()
}

/// Maps a sub-step's 0..1 progress into a slice of the overall bar.
pub(crate) fn scaled(progress: Progress<'_>, from: f32, to: f32) -> impl FnMut(f32) + '_ {
    move |p| progress(from + (to - from) * p)
}
