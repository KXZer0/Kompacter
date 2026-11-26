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

/// A video's audio track: less sounds broken, more isn't heard.
const MIN_AUDIO_KBPS: f64 = 48.0;
const MAX_AUDIO_KBPS: f64 = 128.0;

/// Below this, one clean channel sounds better than two starved ones.
pub const MONO_BELOW_KBPS: u32 = 64;

/// The untouched video's size is only known from an average (or worked out from the
/// file's), so copying it keeps 5% spare on top of `SAFETY`.
const COPY_HEADROOM: f64 = 1.05;

/// Audio gets a small slice of the budget: enough to sound fine, never more than 128 kbps.
pub fn audio_kbps_for(total_kbps: f64) -> u32 {
    (total_kbps * 0.1)
        .clamp(MIN_AUDIO_KBPS, MAX_AUDIO_KBPS)
        .round() as u32
}

/// Fold stereo (or surround) down to mono when the audio bitrate is too low for it to
/// sound good. Never adds channels: mono stays mono.
pub fn mono(audio_kbps: u32, channels: u32) -> bool {
    audio_kbps < MONO_BELOW_KBPS && channels > 1
}

/// Video copied as-is, only the audio re-encoded: the audio bitrate that still fits next
/// to a `video_kbps` stream. `None` when less than a decent 48 kbps would be left.
pub fn copy_audio_kbps(target_bytes: u64, seconds: f64, video_kbps: u32) -> Option<u32> {
    let left = budget_kbps(target_bytes, seconds) - video_kbps as f64 * COPY_HEADROOM;
    let kbps = left.min(MAX_AUDIO_KBPS).floor();
    (kbps >= MIN_AUDIO_KBPS).then_some(kbps as u32)
}

/// Recalculate how much audio a copied video can afford after measuring an overshoot.
pub(crate) fn retry_audio_kbps(target: u64, measured: u64, seconds: f64, kbps: u32) -> Option<u32> {
    let excess = measured.saturating_sub(target) as f64 * 8.0 / seconds / 1000.0;
    let next = (kbps as f64 - excess - 2.0).min(MAX_AUDIO_KBPS).floor();
    (next >= MIN_AUDIO_KBPS).then_some(next as u32)
}

/// Recover the overshoot from the video budget after reserving audio and mux overhead.
/// Return zero when another attempt would go below the minimum video bitrate.
pub(crate) fn retry_video_kbps(
    target: u64,
    measured: u64,
    seconds: f64,
    video_kbps: u32,
    audio_kbps: u32,
) -> u32 {
    let audio = audio_kbps as f64 * seconds * 1000.0 / 8.0;
    let old_video = measured as f64 - audio;
    let new_video = target as f64 * SAFETY - audio;
    if old_video <= 0.0 || new_video <= 0.0 {
        return 0;
    }
    let next = (video_kbps as f64 * (new_video / old_video).min(0.95)).floor();
    if next < MIN_VIDEO_KBPS {
        return 0;
    }
    next as u32
}

/// Pick resolution, frame rate and bitrates so a clip of `seconds` lands under `target_bytes`.
/// Tries the source size first, then a short edge of 1080, 720, 480, or 360 pixels.
/// Each size tries the source cadence before dropping high rates to half cadence.
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
    let short_side = sw.min(sh);
    let mut rungs = vec![short_side];
    rungs.extend(
        [1080, 720, 480, 360]
            .into_iter()
            .filter(|&side| side < short_side),
    );

    let size_for = |side: u32| -> (u32, u32) {
        let scale = side as f64 / short_side as f64;
        let dimension = |src| even((src as f64 * scale).round() as u32);
        (dimension(sw), dimension(sh))
    };
    // Judged as if it were H.264, so a more efficient codec earns a better grade.
    let bpp = |w: u32, h: u32, f: f64| video * 1000.0 / (w as f64 * h as f64 * f) / efficiency;

    // For high-frame-rate clips, compare full cadence and half cadence before
    // discarding resolution. Halving 50/59.94/60 fps keeps even frame intervals.
    let lower_fps = if fps >= 48.0 { fps / 2.0 } else { fps };
    for &side in &rungs {
        let (w, h) = size_for(side);
        for candidate_fps in [fps, lower_fps] {
            let q = Quality::from_bpp(bpp(w, h, candidate_fps));
            if q != Quality::Low {
                return plan(w, h, candidate_fps, video, audio, q);
            }
        }
    }
    // Nothing looked okay: smallest rung, and drop to 30 fps to help a bit.
    let (w, h) = size_for(*rungs.last().unwrap());
    let fps = lower_fps.min(30.0);
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
    fn tries_half_cadence_before_losing_detail_and_treats_portrait_the_same() {
        let landscape = plan_video(MB, 2.0, 1920, 1080, 60.0, false, 1.0);
        let portrait = plan_video(MB, 2.0, 1080, 1920, 60.0, false, 1.0);
        assert_eq!(
            (landscape.width, landscape.height, landscape.fps),
            (1920, 1080, 30.0)
        );
        assert_eq!(
            (portrait.width, portrait.height, portrait.fps),
            (1080, 1920, 30.0)
        );
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
    fn mono_only_below_64_kbps_and_never_upmixed() {
        assert!(mono(48, 2));
        assert!(mono(63, 6));
        assert!(!mono(64, 2));
        assert!(!mono(128, 2));
        assert!(!mono(32, 1));
    }

    #[test]
    fn copying_the_video_leaves_room_for_the_audio() {
        // 10 s, 1 MB: a 217 kbps video leaves plenty, so the audio gets the full 128.
        assert_eq!(copy_audio_kbps(MB, 10.0, 217), Some(128));
        // A big video leaves just a little.
        let kbps = copy_audio_kbps(MB, 10.0, 700).unwrap();
        assert!((48..128).contains(&kbps), "{kbps}");
        let bytes = (700.0 * COPY_HEADROOM + kbps as f64) * 1000.0 / 8.0 * 10.0;
        assert!(bytes <= MB as f64, "{bytes}");
        // The video alone (almost) fills it, or there's no length: re-encode instead.
        assert_eq!(copy_audio_kbps(MB, 10.0, 760), None);
        assert_eq!(copy_audio_kbps(MB, 0.0, 100), None);
    }

    #[test]
    fn audio_plan() {
        assert_eq!(plan_audio_kbps(20 * MB, 60.0), Some(192));
        assert!(plan_audio_kbps(20 * MB, 3600.0).is_some());
        assert_eq!(plan_audio_kbps(MB, 3.0 * 3600.0), None);
    }
}
