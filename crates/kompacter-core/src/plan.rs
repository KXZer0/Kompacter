//! Pure math: how to hit a target size, and live estimates for the trimmer.
//! No ffmpeg calls here, so the UI can run these on every drag.

use serde::Serialize;

/// Share of the target kept free for container overhead and encoder drift.
pub const SAFETY: f64 = 0.97;

/// Bits-per-pixel thresholds for H.264 (rough, but good for a quality badge).
const BPP_GREAT: f64 = 0.10;
const BPP_OKAY: f64 = 0.05;
/// Below this the video is unwatchable, so we report it as not fitting.
const MIN_VIDEO_KBPS: f64 = 60.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Great,
    Okay,
    Low,
}

impl Quality {
    fn from_bpp(bpp: f64) -> Self {
        if bpp >= BPP_GREAT {
            Quality::Great
        } else if bpp >= BPP_OKAY {
            Quality::Okay
        } else {
            Quality::Low
        }
    }
}

/// A selected time range, in seconds from the start of the file.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Trim {
    pub start: f64,
    pub end: f64,
}

impl Trim {
    pub fn new(start: f64, end: f64) -> Result<Self, String> {
        if !(start.is_finite() && end.is_finite()) || start < 0.0 || end <= start {
            return Err(format!("start {start}s must be before end {end}s"));
        }
        Ok(Trim { start, end })
    }

    pub fn len(&self) -> f64 {
        self.end - self.start
    }

    /// Clamp to the file's real duration.
    pub fn clamp(self, duration: f64) -> Result<Self, String> {
        Trim::new(self.start.min(duration), self.end.min(duration))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VideoPlan {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub video_kbps: u32,
    pub audio_kbps: u32,
    pub quality: Quality,
    /// False when even the smallest resolution would be unwatchable.
    pub fits: bool,
}

/// Total bitrate (kbps) that fits `target_bytes` over `seconds`.
pub fn budget_kbps(target_bytes: u64, seconds: f64) -> f64 {
    if seconds <= 0.0 {
        return 0.0;
    }
    target_bytes as f64 * 8.0 * SAFETY / seconds / 1000.0
}

/// Audio gets a small slice of the budget: enough to sound fine, never more than 128 kbps.
pub fn audio_kbps_for(total_kbps: f64) -> u32 {
    (total_kbps * 0.1).clamp(48.0, 128.0).round() as u32
}

/// Pick resolution, frame rate and bitrates so a clip of `seconds` lands under `target_bytes`.
/// Tries the source resolution first, then 1080p, 720p, 480p, 360p, and picks the first
/// that still looks "okay".
pub fn plan_video(
    target_bytes: u64,
    seconds: f64,
    src_w: u32,
    src_h: u32,
    src_fps: f64,
    has_audio: bool,
    // Bits the codec needs for the same look, relative to H.264 (AV1 ≈ 0.55).
    efficiency: f64,
) -> VideoPlan {
    let total = budget_kbps(target_bytes, seconds);
    let audio = if has_audio { audio_kbps_for(total) } else { 0 };
    let video = (total - audio as f64).max(0.0);
    let fps = src_fps.clamp(1.0, 60.0);

    let (sw, sh) = (src_w.max(2), src_h.max(2));
    let mut rungs: Vec<u32> = vec![sh];
    rungs.extend([1080, 720, 480, 360].into_iter().filter(|&h| h < sh));

    let size_for = |h: u32| -> (u32, u32) {
        let w = (sw as f64 * h as f64 / sh as f64).round() as u32;
        (even(w), even(h))
    };
    // Judged as if it were H.264, so a more efficient codec earns a better grade.
    let bpp = |w: u32, h: u32, f: f64| video * 1000.0 / (w as f64 * h as f64 * f) / efficiency;

    for &h in &rungs {
        let (w, h) = size_for(h);
        let q = Quality::from_bpp(bpp(w, h, fps));
        if q != Quality::Low {
            return plan(w, h, fps, video, audio, q);
        }
    }
    // Nothing looked okay: smallest rung, and drop to 30 fps to help a bit.
    let (w, h) = size_for(*rungs.last().unwrap());
    let fps = fps.min(30.0);
    let q = Quality::from_bpp(bpp(w, h, fps));
    plan(w, h, fps, video, audio, q)
}

fn plan(w: u32, h: u32, fps: f64, video: f64, audio: u32, quality: Quality) -> VideoPlan {
    VideoPlan {
        width: w,
        height: h,
        fps,
        video_kbps: video.floor() as u32,
        audio_kbps: audio,
        quality,
        fits: video >= MIN_VIDEO_KBPS,
    }
}

fn even(v: u32) -> u32 {
    (v / 2 * 2).max(2)
}

/// "Keep quality" trim estimate: the cut is a stream copy, so size scales with length.
/// (Later: sum real packet sizes from ffprobe for an exact number.)
pub fn estimate_trim_bytes(size_bytes: u64, duration: f64, trim: Trim) -> u64 {
    if duration <= 0.0 {
        return size_bytes;
    }
    (size_bytes as f64 * (trim.len() / duration).min(1.0)).round() as u64
}

/// Audio bitrate that fits `target_bytes`, capped at 192 kbps. `None` when it can't sound decent.
pub fn plan_audio_kbps(target_bytes: u64, seconds: f64) -> Option<u32> {
    let kbps = budget_kbps(target_bytes, seconds).min(192.0).floor();
    (kbps >= 24.0).then_some(kbps as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::size::MB;

    #[test]
    fn short_clip_keeps_1080p() {
        let p = plan_video(20 * MB, 15.0, 1920, 1080, 30.0, true, 1.0);
        assert_eq!((p.width, p.height), (1920, 1080));
        assert!(p.fits);
        assert_ne!(p.quality, Quality::Low);
    }

    #[test]
    fn long_clip_drops_resolution() {
        let p = plan_video(20 * MB, 180.0, 1920, 1080, 30.0, true, 1.0);
        assert!(p.height < 1080, "got {}p", p.height);
        assert!(p.fits);
    }

    #[test]
    fn huge_clip_does_not_fit() {
        let p = plan_video(20 * MB, 4.0 * 3600.0, 1920, 1080, 60.0, true, 1.0);
        assert!(!p.fits);
        assert_eq!(p.quality, Quality::Low);
    }

    #[test]
    fn trimming_improves_quality() {
        let long = plan_video(20 * MB, 120.0, 1920, 1080, 30.0, true, 1.0);
        let short = plan_video(20 * MB, 20.0, 1920, 1080, 30.0, true, 1.0);
        assert!(short.video_kbps > long.video_kbps);
        assert!(short.height >= long.height);
    }

    #[test]
    fn total_bitrate_stays_under_target() {
        let secs = 62.0;
        let p = plan_video(20 * MB, secs, 1920, 1080, 30.0, true, 1.0);
        let bytes = (p.video_kbps + p.audio_kbps) as f64 * 1000.0 / 8.0 * secs;
        assert!(bytes < (20 * MB) as f64);
    }

    #[test]
    fn trim_estimate_and_validation() {
        let t = Trim::new(10.0, 40.0).unwrap();
        assert_eq!(estimate_trim_bytes(48 * MB, 60.0, t), 24 * MB);
        assert!(Trim::new(5.0, 5.0).is_err());
        assert!(Trim::new(-1.0, 5.0).is_err());
        assert_eq!(
            Trim::new(10.0, 99.0).unwrap().clamp(60.0).unwrap().end,
            60.0
        );
    }

    #[test]
    fn audio_plan() {
        assert_eq!(plan_audio_kbps(20 * MB, 60.0), Some(192));
        assert!(plan_audio_kbps(20 * MB, 3600.0).is_some());
        assert_eq!(plan_audio_kbps(MB, 3.0 * 3600.0), None);
    }
}
