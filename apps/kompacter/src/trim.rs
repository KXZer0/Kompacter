//! The trimmer: a filmstrip with two handles, a big preview of the frame under the handle,
//! and a live readout. The estimate is pure math in the engine (the probe is cached), so it
//! updates on every drag without any background work.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use kompacter_core::{MediaKind, Options, Quality};
use slint::{Image, ModelRc, Rgb8Pixel, SharedPixelBuffer, VecModel};

use crate::format::{bytes, clock};
use crate::{run, App, DetailRow, View};

#[derive(Default)]
pub struct Trim {
    /// Filmstrip frames for `path` (kept so going back and forth doesn't redo them).
    frames: RefCell<Vec<Image>>,
    path: RefCell<Option<PathBuf>>,
    /// "Keep quality" fits the limit at the current selection.
    keep_ok: Cell<bool>,
}

impl App {
    pub fn open_trim(self: &Rc<Self>) {
        let ui = self.ui();
        let cur = self.current.borrow();
        let Some(c) = cur.as_ref() else { return };
        let dur = c.info.duration.max(1.0) as f32;
        ui.set_trim_dur(dur);
        ui.set_trim_a(0.0);
        ui.set_trim_b(dur);
        ui.set_trim_active(0);
        ui.set_trim_mode(0);

        let same = self.trim.path.borrow().as_ref() == Some(&c.path);
        if !same {
            self.trim.frames.borrow_mut().clear();
            *self.trim.path.borrow_mut() = Some(c.path.clone());
            ui.set_thumbs(ModelRc::new(VecModel::<Image>::default()));
            ui.set_trim_frame(Image::default());
            if matches!(c.info.kind, MediaKind::Video) {
                let engine = self.engine.clone();
                let info = c.info.clone();
                let weak = Rc::downgrade(self);
                run(
                    move || engine.thumbnails(&info, 10, 180).ok(),
                    move |frames| {
                        let (Some(app), Some(f)) = (weak.upgrade(), frames) else {
                            return;
                        };
                        let images: Vec<Image> = f
                            .rgb
                            .iter()
                            .map(|px| {
                                Image::from_rgb8(SharedPixelBuffer::<Rgb8Pixel>::clone_from_slice(
                                    px, f.width, f.height,
                                ))
                            })
                            .collect();
                        app.ui()
                            .set_thumbs(ModelRc::new(VecModel::from(images.clone())));
                        *app.trim.frames.borrow_mut() = images;
                        app.trim_update();
                    },
                );
            }
        } else {
            let frames = self.trim.frames.borrow().clone();
            ui.set_thumbs(ModelRc::new(VecModel::from(frames)));
        }
        drop(cur);
        self.set_view(View::Trim);
        self.trim_update();
    }

    /// Recompute the readout for the current handles and mode.
    pub fn trim_update(&self) {
        let ui = self.ui();
        let cur = self.current.borrow();
        let Some(c) = cur.as_ref() else { return };
        let (a, b) = (ui.get_trim_a() as f64, ui.get_trim_b() as f64);
        let dur = c.info.duration.max(1.0);
        let sec = (b - a).max(0.0);
        let target = self.target().bytes;
        ui.set_trim_range(format!("{} – {} · {} s", clock(a), clock(b), sec.round()).into());

        // Preview: the frame under the handle being moved.
        let frames = self.trim.frames.borrow();
        if !frames.is_empty() {
            let t = if ui.get_trim_active() == 0 { a } else { b };
            let i = ((t / dur) * (frames.len() - 1) as f64).round() as usize;
            ui.set_trim_frame(frames[i.min(frames.len() - 1)].clone());
        }

        let mut opts = Options::new(target);
        opts.trim = kompacter_core::Trim::new(a, b).ok();
        let Ok(est) = self.engine.estimate(&c.info, &opts) else {
            return;
        };
        let keep_bytes = est
            .keep_quality_bytes
            .unwrap_or((c.info.size_bytes as f64 * sec / dur) as u64);
        let keep_ok = keep_bytes <= target;
        self.trim.keep_ok.set(keep_ok);

        let original_kbps = if c.info.duration > 0.0 {
            (c.info.size_bytes as f64 * 8.0 / c.info.duration / 1000.0) as u32
        } else {
            0
        };
        let video = c.info.video.as_ref();
        let original = || {
            let mut rows = Vec::new();
            if let Some(v) = video {
                rows.push(row(
                    "Resolution",
                    format!("{}×{} (original)", v.width, v.height),
                ));
                rows.push(row("Frame rate", format!("{} fps", v.fps.round())));
            }
            rows.push(row(
                "Bitrate",
                format!("{} (original)", rate(original_kbps)),
            ));
            if let Some(v) = video {
                rows.extend(compare(v.width, v.height, v.fps, original_kbps));
            }
            rows
        };
        let same = "";

        if ui.get_trim_mode() == 0 {
            self.set_details(original(), same);
            // Keep quality: a cut, no re-encode.
            ui.set_trim_big(format!("≈ {}", bytes(keep_bytes)).into());
            ui.set_trim_grade(3);
            ui.set_trim_ok(keep_ok);
            if keep_ok {
                ui.set_trim_status(format!("Fits {}", bytes(target)).into());
                ui.set_trim_button("Trim & copy".into());
            } else {
                let per_sec = keep_bytes as f64 / sec.max(0.1);
                let cut = ((keep_bytes - target) as f64 / per_sec).ceil();
                ui.set_trim_status(
                    format!("{} over · cut {cut} s more", bytes(keep_bytes - target)).into(),
                );
                ui.set_trim_button(format!("Fit to {} instead", bytes(target)).into());
            }
        } else if est.fits_as_is {
            self.set_details(original(), same);
            ui.set_trim_big("Original".into());
            ui.set_trim_grade(3);
            ui.set_trim_ok(true);
            ui.set_trim_status(format!("Fits {} · no re-encode", bytes(target)).into());
            ui.set_trim_button("Compress".into());
        } else if let Some(v) = &est.video {
            let mut rows = Vec::new();
            let src = video.map(|s| (s.width, s.height, s.fps));
            rows.push(row(
                "Resolution",
                match src {
                    Some((w, h, _)) if (w, h) != (v.width, v.height) => {
                        format!("{w}×{h} → {}×{}", v.width, v.height)
                    }
                    _ => format!("{}×{}", v.width, v.height),
                },
            ));
            rows.push(row(
                "Frame rate",
                match src {
                    Some((_, _, f)) if (f - v.fps).abs() > 0.5 => {
                        format!("{} → {} fps", f.round(), v.fps.round())
                    }
                    _ => format!("{} fps", v.fps.round()),
                },
            ));
            rows.push(row(
                "Bitrate",
                format!("{} video · {} kbps audio", rate(v.video_kbps), v.audio_kbps),
            ));
            rows.extend(compare(v.width, v.height, v.fps, v.video_kbps));
            self.set_details(rows, "");
            ui.set_trim_big(format!("{}p", v.height).into());
            ui.set_trim_grade(match v.quality {
                Quality::Great => 3,
                Quality::Okay => 2,
                Quality::Low => 1,
            });
            ui.set_trim_ok(v.fits);
            ui.set_trim_status(
                if v.fits {
                    format!("Fits {}", bytes(target))
                } else {
                    "Too long — trim more".into()
                }
                .into(),
            );
            ui.set_trim_button("Compress".into());
        } else {
            let ok = est.audio_kbps.is_some();
            let k = est.audio_kbps.unwrap_or(0);
            let mut rows = vec![row("Audio bitrate", format!("{k} kbps"))];
            rows.push(row("Average audio", "128 kbps".into()));
            rows.push(row("This one", versus(k as f64 / 128.0).into()));
            self.set_details(rows, "");
            ui.set_trim_big(
                est.audio_kbps
                    .map_or("—".into(), |k| format!("{k} kbps"))
                    .into(),
            );
            ui.set_trim_grade(0);
            ui.set_trim_ok(ok);
            ui.set_trim_status(
                if ok {
                    format!("Fits {}", bytes(target))
                } else {
                    "Too long — trim more".into()
                }
                .into(),
            );
            ui.set_trim_button("Compress".into());
        }
    }

    fn set_details(&self, rows: Vec<DetailRow>, verdict: &str) {
        let ui = self.ui();
        ui.set_trim_details(ModelRc::new(VecModel::from(rows)));
        ui.set_trim_verdict(verdict.into());
    }

    pub fn trim_compress(self: &Rc<Self>) {
        let ui = self.ui();
        let keep = ui.get_trim_mode() == 0;
        if keep && !self.trim.keep_ok.get() {
            // Doesn't fit at original quality: switch to Fit, so the user sees what it gives.
            ui.set_trim_mode(1);
            self.trim_update();
            return;
        }
        let range = (ui.get_trim_a() as f64, ui.get_trim_b() as f64);
        self.compress(Some(range), keep);
    }
}

fn row(label: &str, value: String) -> DetailRow {
    DetailRow {
        label: label.into(),
        value: value.into(),
    }
}

/// "2.4 Mbps" / "900 kbps".
fn rate(kbps: u32) -> String {
    if kbps >= 1000 {
        let m = format!("{:.1}", kbps as f64 / 1000.0);
        format!("{} Mbps", m.trim_end_matches(".0"))
    } else {
        format!("{kbps} kbps")
    }
}

/// Bitrate of an average H.264 video of this size and frame rate. Works for any
/// resolution (and vertical videos): about 0.08 bits per pixel per frame at 30 fps, which
/// gives the usual streaming numbers (720p ≈ 2.2 Mbps, 1080p ≈ 5 Mbps, 4K ≈ 20 Mbps).
/// Higher frame rates need more, but not double (frames are more alike).
fn average_kbps(width: u32, height: u32, fps: f64) -> u32 {
    let fps = fps.clamp(10.0, 120.0);
    let per_frame = width as f64 * height as f64 * 0.08;
    let kbps = per_frame * 30.0 * (fps / 30.0).powf(0.6) / 1000.0;
    // Round to a friendly number.
    let step = if kbps >= 1000.0 { 100.0 } else { 50.0 };
    ((kbps / step).round() * step).max(50.0) as u32
}

/// Baseline for an average video at this size, and how this one compares.
fn compare(width: u32, height: u32, fps: f64, kbps: u32) -> [DetailRow; 2] {
    let avg = average_kbps(width, height, fps);
    let p = width.min(height); // "720p" means the short side, also for vertical video
    let what = format!(
        "{p}p{}",
        if fps > 40.0 {
            fps.round().to_string()
        } else {
            String::new()
        }
    );
    [
        row(&format!("Average {what} video"), rate(avg)),
        row("This one", versus(kbps as f64 / avg as f64).into()),
    ]
}

fn versus(ratio: f64) -> &'static str {
    if ratio >= 1.15 {
        "Above average"
    } else if ratio >= 0.8 {
        "About average"
    } else {
        "Below average"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn average_bitrate_scales_with_size_and_fps() {
        assert_eq!(average_kbps(1280, 720, 30.0), 2200);
        assert_eq!(average_kbps(1920, 1080, 30.0), 5000);
        assert_eq!(average_kbps(3840, 2160, 30.0), 19900);
        assert_eq!(average_kbps(854, 480, 30.0), 1000);
        // Vertical phone video: same pixels, same baseline.
        assert_eq!(average_kbps(1080, 1920, 30.0), 5000);
        // 60 fps needs more, but not double.
        let (a, b) = (
            average_kbps(1920, 1080, 30.0),
            average_kbps(1920, 1080, 60.0),
        );
        assert!(b > a && b < 2 * a);
        assert_eq!(versus(1.3), "Above average");
        assert_eq!(versus(0.9), "About average");
        assert_eq!(versus(0.5), "Below average");
    }
}
