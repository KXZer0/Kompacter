//! The trimmer: a filmstrip with two handles, a big preview of the frame under the handle,
//! and a live readout. The estimate is pure math in the engine (the probe is cached), so it
//! updates on every drag without any background work.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use kompacter_core::{MediaKind, Quality};
use slint::{Image, ModelRc, Rgb8Pixel, SharedPixelBuffer, VecModel};

use crate::format::{bytes, clock_ms, parse_time, rate};
use crate::{run, sys, App, DetailRow, View};

#[derive(Default)]
pub struct Trim {
    /// Filmstrip frames for `path` (kept so going back and forth doesn't redo them).
    frames: RefCell<Vec<Image>>,
    path: RefCell<Option<PathBuf>>,
    /// "Keep quality" fits the limit at the current selection.
    keep_ok: Cell<bool>,
    /// An exact frame is being fetched; `want` is the newest time asked for meanwhile.
    fetching: Cell<bool>,
    want: Cell<Option<f64>>,
    /// Stops the playback in progress.
    stop: RefCell<Option<Arc<AtomicBool>>>,
    /// Was playing when a trim drag started (resume from the new position on release).
    resume: Cell<bool>,
}

fn image(w: u32, h: u32, px: &[u8]) -> Image {
    Image::from_rgb8(SharedPixelBuffer::<Rgb8Pixel>::clone_from_slice(px, w, h))
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
        ui.set_trim_pos(0.0);
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
                            .map(|px| image(f.width, f.height, px))
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
    pub fn trim_update(self: &Rc<Self>) {
        let ui = self.ui();
        let cur = self.current.borrow();
        let Some(c) = cur.as_ref() else { return };
        let (a, b) = (ui.get_trim_a() as f64, ui.get_trim_b() as f64);
        let dur = c.info.duration.max(1.0);
        let sec = (b - a).max(0.0);
        let target = self.target().bytes;
        ui.set_trim_range(format!("{sec:.1} s").into());
        ui.set_trim_a_text(clock_ms(a).into());
        ui.set_trim_b_text(clock_ms(b).into());

        // Preview: the exact frame under the handle being moved (while not playing).
        if !ui.get_trim_playing() {
            let t = if ui.get_trim_active() == 0 { a } else { b };
            ui.set_trim_pos(t as f32);
            self.show_frame(t);
        }

        let mut opts = self.options(target);
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
            rows
        };

        if ui.get_trim_mode() == 0 {
            self.set_details(original());
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
            self.set_details(original());
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
            self.set_details(rows);
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
                    "Too long, trim more".into()
                }
                .into(),
            );
            ui.set_trim_button("Compress".into());
        } else {
            let ok = est.audio_kbps.is_some();
            let k = est.audio_kbps.unwrap_or(0);
            let rows = vec![row("Audio bitrate", format!("{k} kbps"))];
            self.set_details(rows);
            ui.set_trim_big(
                est.audio_kbps
                    .map_or("Too long".into(), |k| format!("{k} kbps"))
                    .into(),
            );
            ui.set_trim_grade(0);
            ui.set_trim_ok(ok);
            ui.set_trim_status(
                if ok {
                    format!("Fits {}", bytes(target))
                } else {
                    "Too long, trim more".into()
                }
                .into(),
            );
            ui.set_trim_button("Compress".into());
        }
    }

    /// Show the exact frame at `t`. One fetch at a time; while one runs, only the newest
    /// request is kept, so dragging stays smooth and always ends on the right frame.
    fn show_frame(self: &Rc<Self>, t: f64) {
        let ui = self.ui();
        ui.set_trim_time(clock_ms(t).into());
        if !ui.get_trim_playing() {
            ui.set_trim_pos(t as f32);
        }
        if self.trim.fetching.get() {
            self.trim.want.set(Some(t));
            return;
        }
        let Some(info) = self.current.borrow().as_ref().map(|c| c.info.clone()) else {
            return;
        };
        if !matches!(info.kind, MediaKind::Video) {
            return;
        }
        self.trim.fetching.set(true);
        let engine = self.engine.clone();
        let weak = Rc::downgrade(self);
        run(
            move || engine.frame_at(&info, t, 360).ok(),
            move |f| {
                let Some(app) = weak.upgrade() else { return };
                app.trim.fetching.set(false);
                let ui = app.ui();
                if let Some(f) = f.filter(|_| !ui.get_trim_playing()) {
                    ui.set_trim_frame(image(f.width, f.height, &f.rgb[0]));
                }
                if let Some(next) = app.trim.want.take() {
                    app.show_frame(next);
                }
            },
        );
    }

    /// Play or pause the selection (with sound unless it's muted).
    pub fn trim_play(self: &Rc<Self>) {
        let ui = self.ui();
        if ui.get_trim_playing() {
            self.trim_stop();
            return;
        }
        let (a, b) = (ui.get_trim_a() as f64, ui.get_trim_b() as f64);
        let pos = ui.get_trim_pos() as f64;
        let start = if pos >= a && pos < b { pos } else { a };
        self.trim_play_from(start, b);
    }

    /// Dragging a handle while playing: stop now (preview follows handle), resume later.
    pub fn trim_drag_start(self: &Rc<Self>) {
        if self.ui().get_trim_playing() {
            self.trim.resume.set(true);
            self.trim_stop();
        }
    }

    pub fn trim_drag_end(self: &Rc<Self>) {
        if self.trim.resume.take() {
            let ui = self.ui();
            let (a, b) = (ui.get_trim_a() as f64, ui.get_trim_b() as f64);
            let pos = ui.get_trim_pos() as f64;
            self.trim_play_from(pos.clamp(a, (b - 0.1).max(a)), b);
        }
    }

    /// Seek with the playhead. Restarts playback from the new point when playing;
    /// otherwise just shows that frame.
    pub fn trim_seek(self: &Rc<Self>, pos: f64) {
        let ui = self.ui();
        let (a, b) = (ui.get_trim_a() as f64, ui.get_trim_b() as f64);
        let pos = pos.clamp(a, b);
        ui.set_trim_pos(pos as f32);
        if ui.get_trim_playing() {
            self.trim_stop();
            self.trim_play_from(pos, b);
        } else {
            self.show_frame(pos);
        }
    }

    fn trim_play_from(self: &Rc<Self>, from: f64, until: f64) {
        let ui = self.ui();
        let Some(info) = self.current.borrow().as_ref().map(|c| c.info.clone()) else {
            return;
        };
        if !matches!(info.kind, MediaKind::Video) {
            return;
        }
        let (a, b) = (from, until);
        let sound = ui.get_trim_sound() && info.audio.is_some();
        let stop = Arc::new(AtomicBool::new(false));
        *self.trim.stop.borrow_mut() = Some(stop.clone());
        ui.set_trim_playing(true);
        let engine = self.engine.clone();
        let w = self.ui.clone();
        let weak = Rc::downgrade(self);
        let fps = 24.0;
        run(
            move || {
                let tmp = tempfile::tempdir().ok();
                let wav = tmp.as_ref().map(|d| d.path().join("sound.wav"));
                if let (true, Some(wav)) = (sound, &wav) {
                    if engine.audio_wav(&info, a, b, wav).is_ok() {
                        sys::play_wav(wav);
                    }
                }
                let mut n = 0u32;
                let _ = engine.play(&info, a, b, 360, fps, &stop, &mut |fw, fh, px| {
                    let t = a + n as f64 / fps;
                    n += 1;
                    let w = w.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = w.upgrade() {
                            if ui.get_trim_playing() {
                                ui.set_trim_frame(image(fw, fh, &px));
                                ui.set_trim_time(clock_ms(t).into());
                                ui.set_trim_pos(t as f32);
                            }
                        }
                    });
                    !stop.load(Ordering::Relaxed)
                });
                sys::stop_sound();
                drop(tmp);
            },
            move |_| {
                if let Some(app) = weak.upgrade() {
                    app.trim_stop();
                }
            },
        );
    }

    /// Stop playback and go back to the frame under the active handle.
    pub fn trim_stop(self: &Rc<Self>) {
        if let Some(stop) = self.trim.stop.borrow_mut().take() {
            stop.store(true, Ordering::Relaxed);
        }
        sys::stop_sound();
        let ui = self.ui();
        if ui.get_trim_playing() {
            ui.set_trim_playing(false);
            let t = if ui.get_trim_active() == 0 {
                ui.get_trim_a()
            } else {
                ui.get_trim_b()
            };
            self.show_frame(t as f64);
        }
    }

    /// A typed start (side 0) or end (side 1) time.
    pub fn trim_set_time(self: &Rc<Self>, side: i32, text: &str) {
        let ui = self.ui();
        let dur = ui.get_trim_dur() as f64;
        if let Some(t) = parse_time(text) {
            let (a, b) = (ui.get_trim_a() as f64, ui.get_trim_b() as f64);
            if side == 0 {
                ui.set_trim_a(t.clamp(0.0, (b - 0.1).max(0.0)) as f32);
            } else {
                ui.set_trim_b(t.clamp(a + 0.1, dur) as f32);
            }
            ui.set_trim_active(side);
        }
        self.trim_update();
    }

    /// Move the active handle one frame (or one second with Shift).
    pub fn trim_nudge(self: &Rc<Self>, dir: i32, big: bool) {
        let ui = self.ui();
        let fps = self
            .current
            .borrow()
            .as_ref()
            .and_then(|c| c.info.video.as_ref().map(|v| v.fps))
            .filter(|f| *f > 0.0)
            .unwrap_or(30.0);
        let step = if big { 1.0 } else { 1.0 / fps } * dir as f64;
        let text = if ui.get_trim_active() == 0 {
            clock_ms(ui.get_trim_a() as f64 + step)
        } else {
            clock_ms(ui.get_trim_b() as f64 + step)
        };
        self.trim_set_time(ui.get_trim_active(), &text);
    }

    fn set_details(&self, rows: Vec<DetailRow>) {
        self.ui()
            .set_trim_details(ModelRc::new(VecModel::from(rows)));
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
        self.compress(Some(range), keep, false);
    }
}

fn row(label: &str, value: String) -> DetailRow {
    DetailRow {
        label: label.into(),
        value: value.into(),
    }
}
