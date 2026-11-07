//! The trimmer: a filmstrip with two handles, a big preview of the frame under the handle,
//! and a live readout. The estimate is pure math in the engine (the probe is cached), so it
//! updates on every drag without any background work.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kompacter_core::{MediaInfo, MediaKind, Mode, Quality};
use slint::{Image, ModelRc, Rgb8Pixel, SharedPixelBuffer, Timer, TimerMode, VecModel};

use crate::format::{bytes, clock_ms, parse_time, rate};
use crate::trim_cache::FrameCache;
use crate::{run, sys, with_app, App, DetailRow, View};

#[derive(Default)]
pub struct Trim {
    /// Filmstrip frames for `path` (kept so going back and forth doesn't redo them).
    frames: RefCell<Vec<Image>>,
    path: RefCell<Option<PathBuf>>,
    session: RefCell<Option<MediaInfo>>,
    strip_needed: Cell<bool>,
    strip_work: RefCell<Option<Arc<AtomicBool>>>,
    /// "Keep quality" fits the limit at the current selection.
    keep_ok: Cell<bool>,
    /// Seeks and playback share a single worker slot, even across trimmer sessions.
    preview_work: RefCell<Option<Arc<AtomicBool>>>,
    scrub_window: RefCell<Option<Arc<ScrubWindow>>>,
    scrub_painted: Cell<Option<u64>>,
    scrub_unavailable: Cell<bool>,
    want: Cell<Option<f64>>,
    seek_at: Cell<Option<f64>>,
    seek_timer: Timer,
    playback: RefCell<Option<Arc<Playback>>>,
    dragging: Cell<bool>,
    /// Was playing when a drag started (resume from the new position on release).
    resume: Cell<bool>,
    /// Handles at the last update, so only a handle move jumps the preview to it.
    last_ab: Cell<(f64, f64)>,
}

struct Playback {
    start: f64,
    end: f64,
    clock_us: AtomicU64,
    sound: AtomicBool,
    stop: Arc<AtomicBool>,
}

impl Playback {
    fn position(&self) -> f64 {
        (self.start + self.clock_us.load(Ordering::Relaxed) as f64 / 1_000_000.0).min(self.end)
    }
}

struct PreviewFrame {
    time: f64,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

struct ScrubWindow {
    range: Range<f64>,
    frames: Mutex<FrameCache>,
    stop: Arc<AtomicBool>,
    update_queued: AtomicBool,
}

impl ScrubWindow {
    fn contains(&self, t: f64) -> bool {
        t >= self.range.start && t <= self.range.end
    }
}

fn image(w: u32, h: u32, px: &[u8]) -> Image {
    Image::from_rgb8(SharedPixelBuffer::<Rgb8Pixel>::clone_from_slice(px, w, h))
}

impl App {
    pub fn open_trim(self: &Rc<Self>) {
        self.trim_halt();
        let ui = self.ui();
        let cur = self.current.borrow();
        let Some(c) = cur.as_ref() else { return };
        let dur = c.info.duration as f32;
        ui.set_trim_dur(dur);
        ui.set_trim_a(0.0);
        ui.set_trim_b(dur);
        ui.set_trim_pos(0.0);
        self.trim.last_ab.set((0.0, f64::from(dur)));
        ui.set_trim_active(0);
        ui.set_trim_mode(0);

        if self.trim.path.borrow().as_ref() != Some(&c.path) {
            self.trim.frames.borrow_mut().clear();
            *self.trim.path.borrow_mut() = Some(c.path.clone());
        }
        ui.set_thumbs(ModelRc::new(VecModel::from(
            self.trim.frames.borrow().clone(),
        )));
        ui.set_trim_frame(Image::default());
        drop(cur);
        self.set_view(View::Trim);
        self.trim_activate();
        self.trim_update();
    }

    /// Restore the existing selection when returning from settings, a hidden window, or a job.
    pub fn trim_activate(self: &Rc<Self>) {
        if self.view() != View::Trim || self.trim.session.borrow().is_some() {
            return;
        }
        let Some(info) = self.current.borrow().as_ref().map(|c| c.info.clone()) else {
            return;
        };
        self.trim
            .strip_needed
            .set(info.kind == MediaKind::Video && self.trim.frames.borrow().is_empty());
        *self.trim.session.borrow_mut() = Some(info);
        self.show_frame(f64::from(self.ui().get_trim_pos()), false);
    }

    fn trim_filmstrip(self: &Rc<Self>) {
        if self.trim.dragging.get()
            || self.trim.playback.borrow().is_some()
            || self.trim.preview_work.borrow().is_some()
            || self.trim.strip_work.borrow().is_some()
            || !self.trim.strip_needed.take()
        {
            return;
        }
        let Some(info) = self.trim.session.borrow().clone() else {
            return;
        };
        let stop = Arc::new(AtomicBool::new(false));
        *self.trim.strip_work.borrow_mut() = Some(stop.clone());
        let engine = self.engine.clone();
        let weak = Rc::downgrade(self);
        let mine = stop.clone();
        run(
            move || engine.thumbnails_cancellable(&info, 10, 90, &stop).ok(),
            move |frames| {
                let Some(app) = weak.upgrade() else { return };
                app.trim.strip_work.borrow_mut().take();
                if !mine.load(Ordering::Relaxed) {
                    if let Some(frames) = frames {
                        let images: Vec<Image> = frames
                            .rgb
                            .iter()
                            .map(|px| image(frames.width, frames.height, px))
                            .collect();
                        app.ui()
                            .set_thumbs(ModelRc::new(VecModel::from(images.clone())));
                        *app.trim.frames.borrow_mut() = images;
                    }
                }
                app.trim_filmstrip();
            },
        );
    }

    /// Recompute the readout for the current handles and mode.
    pub fn trim_update(self: &Rc<Self>) {
        let ui = self.ui();
        let cur = self.current.borrow();
        let Some(c) = cur.as_ref() else { return };
        let (a, b) = (ui.get_trim_a() as f64, ui.get_trim_b() as f64);
        let dur = c.info.duration;
        let sec = (b - a).max(0.0);
        let target = self.target_bytes();
        ui.set_trim_range(format!("{sec:.1} s").into());
        ui.set_trim_a_text(clock_ms(a).into());
        ui.set_trim_b_text(clock_ms(b).into());

        // Preview: the exact frame under the handle being moved (while not playing).
        // Other updates (mode, thumbnails) leave the playhead where it is.
        let moved = self.trim.last_ab.replace((a, b)) != (a, b);
        if moved && !ui.get_trim_playing() {
            let t = if ui.get_trim_active() == 0 { a } else { b };
            self.show_frame(t, true);
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

        if ui.get_trim_mode() == 2 {
            // GIF: short clips only; the size is a guess until it's made.
            let max = kompacter_core::MAX_GIF_SECONDS;
            let guess = self.engine.gif_guess(&c.info, sec, target);
            let ok = video.is_some() && sec <= max + 0.05 && guess.is_some();
            ui.set_trim_big("GIF".into());
            ui.set_trim_grade(0);
            ui.set_trim_ok(ok);
            ui.set_trim_status(
                if video.is_none() {
                    "GIFs need a video".to_string()
                } else if sec > max + 0.05 {
                    format!("Up to {max:.0} s · cut {:.0} s more", (sec - max).ceil())
                } else if let Some((w, fps)) = guess {
                    format!("About {w} px wide · {fps:.0} fps")
                } else {
                    "Too long for a GIF this small".into()
                }
                .into(),
            );
            self.set_details(Vec::new());
            ui.set_trim_button("Make GIF".into());
        } else if ui.get_trim_mode() == 0 {
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

    /// Update the playhead immediately and keep only the newest frame request.
    fn show_frame(self: &Rc<Self>, t: f64, debounce: bool) {
        self.trim_position(t);
        let t = {
            let session = self.trim.session.borrow();
            let Some(info) = session
                .as_ref()
                .filter(|info| info.kind == MediaKind::Video)
            else {
                return;
            };
            // Match the precision of FFmpeg's seek and scrub timestamps.
            (t.max(0.0).min(info.duration) * 1e6).round() / 1e6
        };
        if self.trim.seek_at.replace(Some(t)) == Some(t) {
            return;
        }
        self.trim.want.set(Some(t));

        let window = self.trim.scrub_window.borrow().clone();
        let nearby = window.as_ref().filter(|window| window.contains(t));
        let reuse_decoder = nearby.is_some_and(|window| {
            self.trim
                .preview_work
                .borrow()
                .as_ref()
                .is_some_and(|work| {
                    Arc::ptr_eq(work, &window.stop) && !work.load(Ordering::Relaxed)
                })
        });
        if !reuse_decoder {
            self.cancel_preview_work();
        }
        if let Some(window) = nearby {
            if self.present_scrub_frame(window) || reuse_decoder {
                return;
            }
        } else {
            self.clear_scrub_window();
        }
        if debounce && !self.trim.seek_timer.running() {
            let weak = Rc::downgrade(self);
            self.trim.seek_timer.start(
                TimerMode::SingleShot,
                Duration::from_millis(24),
                move || {
                    if let Some(app) = weak.upgrade() {
                        app.trim.seek_timer.stop();
                        app.trim_preview();
                    }
                },
            );
        } else if !debounce {
            self.trim.seek_timer.stop();
            self.trim_preview();
        }
    }

    fn trim_position(&self, t: f64) {
        let ui = self.ui();
        ui.set_trim_time(clock_ms(t).into());
        ui.set_trim_pos(t as f32);
    }

    fn cancel_preview_work(&self) {
        if let Some(stop) = self.trim.preview_work.borrow().as_ref() {
            stop.store(true, Ordering::Relaxed);
        }
    }

    fn clear_scrub_window(&self) {
        self.trim.scrub_window.borrow_mut().take();
        self.trim.scrub_painted.take();
    }

    /// Paint only a frame that covers the current request, even if decoding has
    /// moved on to other frames by the time this callback reaches the UI.
    fn present_scrub_frame(&self, window: &Arc<ScrubWindow>) -> bool {
        if !self.is_current_scrub_window(window) {
            return false;
        }
        let Some(t) = self.trim.want.get().or(self.trim.seek_at.get()) else {
            return false;
        };
        let frames = window.frames.lock().unwrap();
        let Some(frame) = frames.get(t) else {
            return false;
        };
        if self.trim.scrub_painted.replace(Some(frame.id)) != Some(frame.id) {
            let preview = image(frame.width, frame.height, &frame.pixels);
            drop(frames);
            self.ui().set_trim_frame(preview);
        }
        self.trim.want.take();
        self.trim.seek_timer.stop();
        true
    }

    fn is_current_scrub_window(&self, window: &Arc<ScrubWindow>) -> bool {
        !window.stop.load(Ordering::Relaxed)
            && self
                .trim
                .scrub_window
                .borrow()
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, window))
    }

    fn trim_preview(self: &Rc<Self>) {
        if self.trim.preview_work.borrow().is_some() {
            return;
        }
        let Some(info) = self.trim.session.borrow().clone() else {
            return;
        };
        let playback = self.trim.playback.borrow().clone();
        if let Some(playback) = playback {
            self.run_playback(info, playback);
            return;
        }
        if self.trim.seek_timer.running() {
            return;
        }
        let Some(t) = self.trim.want.get() else {
            return;
        };
        if self.trim.scrub_unavailable.get() {
            self.run_exact_seek(info, t);
        } else {
            self.run_scrub_window(info, t);
        }
    }

    fn run_scrub_window(self: &Rc<Self>, info: MediaInfo, t: f64) {
        let fps = info
            .video
            .as_ref()
            .map_or(30.0, |v| v.fps.clamp(15.0, 120.0));
        // Leave cache space for the initial held frame and the frame that
        // confirms coverage at the end of the window.
        let range = (t - 3.0 / fps).max(0.0)..(t + 7.0 / fps).min(info.duration);
        if range.start >= range.end {
            self.run_exact_seek(info, t);
            return;
        }
        let window = Arc::new(ScrubWindow {
            range,
            frames: Mutex::new(FrameCache::default()),
            stop: Arc::new(AtomicBool::new(false)),
            update_queued: AtomicBool::new(false),
        });
        self.trim.scrub_painted.take();
        *self.trim.scrub_window.borrow_mut() = Some(window.clone());
        *self.trim.preview_work.borrow_mut() = Some(window.stop.clone());
        let weak = Rc::downgrade(self);
        let engine = self.engine.clone();
        let worker = window.clone();
        let source = info.clone();
        run(
            move || {
                let result = engine.decode_scrub_window(
                    &source,
                    worker.range.clone(),
                    360,
                    &worker.stop,
                    &mut |frame| {
                        worker.frames.lock().unwrap().insert(
                            frame.time,
                            frame.width,
                            frame.height,
                            frame.pixels,
                        );
                        if !worker.update_queued.swap(true, Ordering::Relaxed) {
                            let worker = worker.clone();
                            with_app(move |app| {
                                worker.update_queued.store(false, Ordering::Relaxed);
                                app.present_scrub_frame(&worker);
                            });
                        }
                        true
                    },
                );
                if result.is_ok() {
                    worker.frames.lock().unwrap().finish(worker.range.end);
                }
                result
            },
            move |result| {
                let Some(app) = weak.upgrade() else { return };
                app.trim.preview_work.borrow_mut().take();
                if app.is_current_scrub_window(&window) {
                    app.present_scrub_frame(&window);
                    if result.is_err() {
                        app.trim.scrub_unavailable.set(true);
                        app.clear_scrub_window();
                    }
                    // An unusual timestamp layout or burst can leave the requested
                    // frame outside the bounded cache. Keep the exact seek as a fallback.
                    if let Some(t) = app.trim.want.get() {
                        app.run_exact_seek(info, t);
                        return;
                    }
                }
                app.trim_preview();
                app.trim_filmstrip();
            },
        );
    }

    fn run_exact_seek(self: &Rc<Self>, info: MediaInfo, t: f64) {
        self.trim.want.take();
        let engine = self.engine.clone();
        let weak = Rc::downgrade(self);
        let stop = Arc::new(AtomicBool::new(false));
        *self.trim.preview_work.borrow_mut() = Some(stop.clone());
        let mine = stop.clone();
        run(
            move || engine.frame_at_cancellable(&info, t, 360, &stop).ok(),
            move |frame| {
                let Some(app) = weak.upgrade() else { return };
                app.trim.preview_work.borrow_mut().take();
                if !mine.load(Ordering::Relaxed) {
                    if let Some(frame) = frame {
                        app.trim.scrub_painted.take();
                        app.ui()
                            .set_trim_frame(image(frame.width, frame.height, &frame.rgb[0]));
                    } else {
                        app.trim.seek_at.take();
                    }
                }
                app.trim_preview();
                app.trim_filmstrip();
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
        self.trim_activate();
        let (a, b) = (f64::from(ui.get_trim_a()), f64::from(ui.get_trim_b()));
        let pos = f64::from(ui.get_trim_pos());
        let start = if pos >= a && pos < b { pos } else { a };
        self.trim_play_from(start, b);
    }

    pub fn trim_sound_changed(&self) {
        if let Some(playback) = self.trim.playback.borrow().as_ref() {
            playback
                .sound
                .store(self.ui().get_trim_sound(), Ordering::Relaxed);
        }
    }

    /// Pause while a handle or playhead is being dragged.
    pub fn trim_drag_start(self: &Rc<Self>) {
        self.trim_activate();
        self.trim.dragging.set(true);
        self.cancel_filmstrip();
        self.trim.resume.set(self.pause_playback());
    }

    /// Resume the playhead or start handle where it was released. Leave a short
    /// lead-in when resuming from the end handle so the cut can be reviewed.
    pub fn trim_drag_end(self: &Rc<Self>, playhead: bool) {
        self.trim.dragging.set(false);
        if !self.trim.resume.take() {
            self.trim.seek_timer.stop();
            self.trim_preview();
            self.trim_filmstrip();
            return;
        }
        let ui = self.ui();
        let (a, b) = (f64::from(ui.get_trim_a()), f64::from(ui.get_trim_b()));
        let from = if playhead {
            f64::from(ui.get_trim_pos())
        } else if ui.get_trim_active() == 0 {
            a
        } else {
            (b - 3.0).max(a)
        };
        if from >= b {
            self.trim.seek_timer.stop();
            self.trim_preview();
            self.trim_filmstrip();
        } else {
            self.trim_play_from(from.max(a), b);
        }
    }

    /// Move the playhead inside the selection and request its frame.
    pub fn trim_seek(self: &Rc<Self>, pos: f64) {
        self.trim_activate();
        self.pause_playback();
        let ui = self.ui();
        let (a, b) = (f64::from(ui.get_trim_a()), f64::from(ui.get_trim_b()));
        self.show_frame(pos.clamp(a, b), true);
    }

    fn trim_play_from(self: &Rc<Self>, from: f64, until: f64) {
        if self.trim.session.borrow().as_ref().map(|info| info.kind) != Some(MediaKind::Video)
            || from >= until
        {
            return;
        }
        self.cancel_preview_work();
        self.clear_scrub_window();
        self.cancel_filmstrip();
        self.trim.seek_timer.stop();
        self.trim.want.take();
        self.trim.seek_at.take();
        let ui = self.ui();
        self.trim_position(from);
        *self.trim.playback.borrow_mut() = Some(Arc::new(Playback {
            start: from,
            end: until,
            clock_us: AtomicU64::new(0),
            sound: AtomicBool::new(ui.get_trim_sound()),
            stop: Arc::new(AtomicBool::new(false)),
        }));
        ui.set_trim_playing(true);
        self.trim_preview();
    }

    fn run_playback(self: &Rc<Self>, info: MediaInfo, playback: Arc<Playback>) {
        *self.trim.preview_work.borrow_mut() = Some(playback.stop.clone());
        let engine = self.engine.clone();
        let w = self.ui.clone();
        let weak = Rc::downgrade(self);
        let mine = playback.clone();
        run(
            move || {
                // Replace the queued frame if Slint is busy. A stalled event loop
                // consumes one fresh frame when it catches up.
                let latest = Arc::new(Mutex::new(None));
                sys::play_preview(
                    &engine,
                    &info,
                    playback.start..playback.end,
                    &playback.stop,
                    &playback.sound,
                    &playback.clock_us,
                    &mut |time, width, height, pixels| {
                        if playback.stop.load(Ordering::Relaxed) {
                            return false;
                        }
                        let should_notify = latest
                            .lock()
                            .unwrap()
                            .replace(PreviewFrame {
                                time,
                                width,
                                height,
                                pixels,
                            })
                            .is_none();
                        if !should_notify {
                            return true;
                        }
                        let w = w.clone();
                        let latest = latest.clone();
                        let playback = playback.clone();
                        slint::invoke_from_event_loop(move || {
                            let frame = latest.lock().unwrap().take();
                            if !playback.stop.load(Ordering::Relaxed) {
                                if let (Some(ui), Some(f)) = (w.upgrade(), frame) {
                                    ui.set_trim_frame(image(f.width, f.height, &f.pixels));
                                    let t = playback.position().max(f.time).min(playback.end);
                                    ui.set_trim_time(clock_ms(t).into());
                                    ui.set_trim_pos(t as f32);
                                }
                            }
                        })
                        .is_ok()
                    },
                )
            },
            move |result| {
                let Some(app) = weak.upgrade() else { return };
                app.trim.preview_work.borrow_mut().take();
                if !mine.stop.load(Ordering::Relaxed) {
                    app.trim.playback.borrow_mut().take();
                    app.ui().set_trim_playing(false);
                    app.trim_position(if result.is_ok() {
                        mine.end
                    } else {
                        mine.position()
                    });
                }
                app.trim_preview();
                app.trim_filmstrip();
            },
        );
    }

    /// Pause playback at the clock position and request its exact frame.
    pub fn trim_stop(self: &Rc<Self>) {
        if self.pause_playback() {
            self.show_frame(f64::from(self.ui().get_trim_pos()), false);
        }
    }

    fn pause_playback(&self) -> bool {
        let Some(playback) = self.trim.playback.borrow_mut().take() else {
            return false;
        };
        playback.stop.store(true, Ordering::Relaxed);
        self.ui().set_trim_playing(false);
        self.trim_position(playback.position());
        true
    }

    fn cancel_filmstrip(&self) {
        if let Some(stop) = self.trim.strip_work.borrow().as_ref() {
            stop.store(true, Ordering::Relaxed);
            self.trim.strip_needed.set(true);
        }
    }

    /// Cancel all preview work when leaving the trimmer.
    pub fn trim_halt(&self) {
        self.pause_playback();
        self.cancel_preview_work();
        self.clear_scrub_window();
        self.trim.scrub_unavailable.set(false);
        self.cancel_filmstrip();
        self.trim.strip_needed.set(false);
        self.trim.seek_timer.stop();
        self.trim.want.take();
        self.trim.seek_at.take();
        self.trim.session.borrow_mut().take();
        self.trim.dragging.set(false);
        self.trim.resume.set(false);
    }

    pub fn trim_quit(self: &Rc<Self>) {
        self.trim_halt();
        // Let the cancellation monitors kill and reap FFmpeg before the process exits.
        if self.trim.preview_work.borrow().is_some() || self.trim.strip_work.borrow().is_some() {
            let app = self.clone();
            Timer::single_shot(Duration::from_millis(10), move || app.trim_quit());
        } else {
            let _ = slint::quit_event_loop();
        }
    }

    /// A typed start (side 0) or end (side 1) time.
    pub fn trim_set_time(self: &Rc<Self>, side: i32, text: &str) {
        if let Some(t) = parse_time(text).filter(|t| t.is_finite()) {
            self.trim_set_handle(side, t);
        } else {
            self.trim_update();
        }
    }

    fn trim_set_handle(self: &Rc<Self>, side: i32, t: f64) {
        let ui = self.ui();
        let dur = f64::from(ui.get_trim_dur());
        let gap = dur.min(0.1);
        let (a, b) = (f64::from(ui.get_trim_a()), f64::from(ui.get_trim_b()));
        let next = if side == 0 {
            t.clamp(0.0, (b - gap).max(0.0))
        } else {
            t.clamp((a + gap).min(dur), dur)
        } as f32;
        ui.set_trim_active(side);
        let current = if side == 0 {
            ui.get_trim_a()
        } else {
            ui.get_trim_b()
        };
        let resume = next != current && ui.get_trim_playing();
        if resume {
            self.trim_drag_start();
        }
        if side == 0 {
            ui.set_trim_a(next);
        } else {
            ui.set_trim_b(next);
        }
        self.trim_update();
        if resume {
            self.trim_drag_end(false);
        }
    }

    /// Move the active handle one frame (or one second with Shift).
    pub fn trim_nudge(self: &Rc<Self>, dir: i32, big: bool) {
        let ui = self.ui();
        let fps = self
            .current
            .borrow()
            .as_ref()
            .and_then(|c| c.info.video.as_ref().map(|v| v.fps))
            .filter(|f| f.is_finite() && *f > 0.0)
            .unwrap_or(30.0);
        let step = if big { 1.0 } else { 1.0 / fps } * f64::from(dir);
        let side = ui.get_trim_active();
        let time = if side == 0 {
            ui.get_trim_a()
        } else {
            ui.get_trim_b()
        };
        self.trim_set_handle(side, f64::from(time) + step);
    }

    fn set_details(&self, rows: Vec<DetailRow>) {
        self.ui()
            .set_trim_details(ModelRc::new(VecModel::from(rows)));
    }

    pub fn trim_compress(self: &Rc<Self>) {
        let ui = self.ui();
        let range = (ui.get_trim_a() as f64, ui.get_trim_b() as f64);
        if ui.get_trim_mode() == 2 {
            if ui.get_trim_ok() {
                self.compress(Some(range), Mode::Gif, false);
            }
            return;
        }
        let keep = ui.get_trim_mode() == 0;
        if keep && !self.trim.keep_ok.get() {
            // Doesn't fit at original quality: switch to Fit, so the user sees what it gives.
            ui.set_trim_mode(1);
            self.trim_update();
            return;
        }
        let mode = if keep { Mode::KeepQuality } else { Mode::Fit };
        self.compress(Some(range), mode, false);
    }
}

fn row(label: &str, value: String) -> DetailRow {
    DetailRow {
        label: label.into(),
        value: value.into(),
    }
}
