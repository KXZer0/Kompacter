//! The main flow: a file arrives → read it → compress (now, or after the "too big" screen
//! or the trimmer) → copied to the clipboard.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use kompacter_core::{MediaKind, Mode, Outcome, Trim};
use slint::winit_030::WinitWindowAccessor;
use slint::ComponentHandle;

use crate::format::{bytes, file_name};
use crate::{format, kind_name, output, run, sys, App, Current, View};

impl App {
    /// A file arrived (drop, paste, choose). Read it, then compress right away if that's
    /// the obvious next step, or show the choices.
    pub fn open(self: &Rc<Self>, path: PathBuf) {
        let ui = self.ui();
        if matches!(ui.get_view(), View::Busy | View::Reading) {
            return;
        }
        *self.current.borrow_mut() = None;
        ui.set_file_name(file_name(&path).into());
        ui.set_chomp(ui.get_chomp() + 1);
        self.set_view(View::Reading);

        let engine = self.engine.clone();
        let target = self.target().bytes;
        let opts = self.options(target);
        let weak = Rc::downgrade(self);
        let p = path.clone();
        run(
            move || {
                let info = engine.probe(&p).map_err(|e| e.to_string())?;
                let estimate = engine.estimate(&info, &opts).map_err(|e| e.to_string())?;
                Ok::<_, String>((info, estimate))
            },
            move |r| {
                let Some(app) = weak.upgrade() else { return };
                match r {
                    Err(e) => app.fail(&e),
                    Ok((info, estimate)) => {
                        let kind = info.kind;
                        let direct = !matches!(kind, MediaKind::Folder)
                            && (estimate.fits_as_is || !matches!(kind, MediaKind::Video));
                        *app.current.borrow_mut() = Some(Current {
                            path,
                            info,
                            estimate,
                        });
                        if direct {
                            app.compress(None, false, false);
                        } else {
                            app.show_big();
                        }
                    }
                }
            },
        );
    }

    /// "Too big" screen: what the file is, how far over, and what Compress will do.
    pub fn show_big(&self) {
        let ui = self.ui();
        let cur = self.current.borrow();
        let Some(c) = cur.as_ref() else { return };
        let target = self.target().bytes;
        let size = c.info.size_bytes;
        let folder = matches!(c.info.kind, MediaKind::Folder);

        let mut meta = Vec::new();
        if c.info.duration > 0.0 {
            meta.push(format::clock(c.info.duration));
        }
        if let Some(v) = &c.info.video {
            meta.push(format!("{}×{}", v.width, v.height));
        }
        meta.push(bytes(size));
        ui.set_file_name(file_name(&c.path).into());
        ui.set_file_meta(meta.join(" · ").into());
        ui.set_file_kind(kind_name(c.info.kind).into());
        ui.set_file_icon(
            sys::file_icon(&c.path)
                .map(|(w, h, px)| {
                    slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(&px, w, h))
                })
                .unwrap_or_default(),
        );

        let max = size.max(target) as f64 * 1.08;
        ui.set_meter_fill((size as f64 / max) as f32);
        ui.set_meter_limit((target as f64 / max) as f32);
        let over = size > target;
        ui.set_over_ok(!over);
        ui.set_over_text(
            if over {
                format!("{} too big", bytes(size - target))
            } else {
                "Fits".into()
            }
            .into(),
        );

        // The button says what it will do; a line appears only as a warning.
        let plan = c.estimate.video.as_ref();
        let label = if folder {
            format!("Compress & {}", self.settings.borrow().archive_format.ext())
        } else {
            match plan {
                Some(v) if v.fits => format!("Compress to {}p", v.height),
                _ => "Compress".into(),
            }
        };
        let too_long = plan.is_some_and(|v| !v.fits);
        let can_trim =
            matches!(c.info.kind, MediaKind::Video | MediaKind::Audio) && c.info.duration > 1.0;
        ui.set_compress_label(label.into());
        ui.set_can_trim(can_trim);
        ui.set_trim_first(can_trim && too_long);
        // Too long to look good in one file: offer splitting it into parts.
        ui.set_split_count(c.estimate.parts.unwrap_or(0) as i32);
        ui.set_foot_text(
            if too_long {
                "Too long for good quality"
            } else {
                ""
            }
            .into(),
        );
        drop(cur);
        self.set_view(View::Big);
    }

    /// Compress the current file. `range` trims it; `keep` keeps the original quality
    /// (a cut without re-encoding); `split` makes parts that each fit when one can't.
    pub fn compress(self: &Rc<Self>, range: Option<(f64, f64)>, keep: bool, split: bool) {
        let ui = self.ui();
        let cur = self.current.borrow();
        let Some(c) = cur.as_ref() else { return };
        let path = c.path.clone();
        let quick = c.estimate.fits_as_is && range.is_none();
        let height = if keep {
            None
        } else {
            c.estimate.video.as_ref().map(|v| v.height)
        };
        drop(cur);

        let prefs = self.settings.borrow().clone();
        let target = self.target().bytes;
        let mut opts = self.options(target);
        opts.mode = if keep { Mode::KeepQuality } else { Mode::Fit };
        opts.split = split;
        self.last_range.set(range);
        if let Some((a, b)) = range {
            opts.trim = Trim::new(a, b).ok();
        }
        opts.out_dir = match output::dir(&prefs, &path) {
            Ok(d) => Some(d),
            Err(e) => return self.fail(&e),
        };

        self.cancel.store(false, Ordering::Relaxed);
        self.started.set(Some(Instant::now()));
        ui.set_progress(0.0);
        ui.set_busy_left("Estimating time…".into());
        if !quick {
            self.set_view(View::Busy);
            self.wake();
            self.start_eta();
        }

        let engine = self.engine.clone();
        let cancel = self.cancel.clone();
        let ui_weak = self.ui.clone();
        let weak = Rc::downgrade(self);
        let from_trim = range.is_some();
        run(
            move || {
                let mut last = -1;
                let mut on_progress = |p: f32| {
                    let step = (p * 200.0) as i32; // ~0.5% steps
                    if step != last {
                        last = step;
                        let w = ui_weak.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = w.upgrade() {
                                ui.set_progress(p);
                            }
                        });
                    }
                };
                let mut out = engine
                    .compress(&path, &opts, &mut on_progress, &cancel)
                    .map_err(|e| e.to_string())?;
                if !out.untouched && prefs.output == "replace" {
                    out.path = output::replace_original(&out.path, &path)?;
                }
                // All parts go on the clipboard together: one paste attaches them all.
                let files = if out.parts.is_empty() {
                    vec![out.path.clone()]
                } else {
                    out.parts.clone()
                };
                let copied = out.fits && sys::copy_files(&files).is_ok();
                output::clean(Some(&out.path));
                Ok::<_, String>((out, copied))
            },
            move |r| {
                let Some(app) = weak.upgrade() else { return };
                match r {
                    Ok((out, copied)) => {
                        app.ui().set_progress(1.0);
                        // Let the press finish its squeeze before the result appears.
                        let weak = Rc::downgrade(&app);
                        let delay = if quick { 0 } else { 420 };
                        slint::Timer::single_shot(Duration::from_millis(delay), move || {
                            if let Some(app) = weak.upgrade() {
                                app.done(out, copied, height, keep);
                            }
                        });
                    }
                    Err(_) if app.cancel.load(Ordering::Relaxed) => {
                        if from_trim {
                            app.set_view(View::Trim);
                        } else if app.current.borrow().as_ref().is_some_and(|c| {
                            !matches!(c.info.kind, MediaKind::Video | MediaKind::Folder)
                        }) {
                            app.another();
                        } else {
                            app.show_big();
                        }
                    }
                    Err(e) => app.fail(&e),
                }
            },
        );
    }

    /// "About 40 s left", refreshed twice a second while compressing.
    fn start_eta(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(500),
            move || {
                let Some(app) = weak.upgrade() else { return };
                let ui = app.ui();
                if ui.get_view() != View::Busy {
                    return;
                }
                if let Some(t) = app.started.get() {
                    let text =
                        format::time_left(t.elapsed().as_secs_f64(), ui.get_progress() as f64);
                    ui.set_busy_left(text.into());
                }
            },
        );
        // Keep the timer alive for this run; it ends itself when the view changes.
        ETA.with(|e| *e.borrow_mut() = Some(timer));
    }

    fn done(self: &Rc<Self>, out: Outcome, copied: bool, height: Option<u32>, keep: bool) {
        ETA.with(|e| *e.borrow_mut() = None);
        let ui = self.ui();
        let parts = out.parts.len();
        let title = match (out.fits, copied, parts) {
            (false, _, _) => "Still too big".to_string(),
            (true, true, 0) => "Copied".into(),
            (true, true, n) => format!("Copied {n} parts"),
            (true, false, 0) => "Saved".into(),
            (true, false, n) => format!("Saved {n} parts"),
        };
        // A folder, video or audio that's still too big can be split into parts.
        let can_split = !out.fits
            && parts == 0
            && matches!(
                out.kind,
                MediaKind::Video | MediaKind::Audio | MediaKind::Folder
            );
        ui.set_done_split(can_split);
        let ratio = out.original_bytes as f64 / out.output_bytes.max(1) as f64;
        let mut details = vec![bytes(out.output_bytes)];
        if keep {
            details.push("original quality".into());
        } else if let Some(h) = height.filter(|_| !out.untouched) {
            details.push(format!("{h}p"));
        }
        if out.untouched {
            details.push("already fit".into());
        } else if ratio >= 1.1 {
            details.push(format!("{ratio:.1}× smaller"));
        }
        let target = self.target().bytes;
        let sub = if !out.fits && can_split {
            format!("{} · too big for one file", bytes(out.output_bytes))
        } else if !out.fits {
            format!(
                "{} · trim it or pick a bigger target",
                bytes(out.output_bytes)
            )
        } else if parts > 0 {
            format!("{parts} files · each under {}", bytes(target))
        } else if !copied {
            "Couldn't copy it, drag it from the folder".into()
        } else {
            details.join(" · ")
        };
        ui.set_done_ok(out.fits);
        ui.set_done_title(title.into());
        ui.set_done_sub(sub.into());
        *self.result.borrow_mut() = Some(out.path);
        self.set_view(View::Done);
        if out.fits {
            ui.set_hop(ui.get_hop() + 1);
        }
        // If the user already went back to their chat, fold away after a moment.
        if !ui
            .window()
            .with_winit_window(|w| w.has_focus())
            .unwrap_or(true)
        {
            self.auto_hide_later(2500);
        }
    }

    pub fn fail(&self, e: &str) {
        ETA.with(|t| *t.borrow_mut() = None);
        let ui = self.ui();
        ui.set_error_text(e.into());
        self.set_view(View::Error);
    }

    fn auto_hide_later(self: &Rc<Self>, ms: u64) {
        if !self.settings.borrow().auto_hide {
            return;
        }
        let weak = Rc::downgrade(self);
        slint::Timer::single_shot(Duration::from_millis(ms), move || {
            let Some(app) = weak.upgrade() else { return };
            let ui = app.ui();
            let focused = ui
                .window()
                .with_winit_window(|w| w.has_focus())
                .unwrap_or(true);
            if !focused && ui.get_view() != View::Busy && !ui.get_dragging() {
                app.close();
            }
        });
    }
}

thread_local! {
    static ETA: std::cell::RefCell<Option<slint::Timer>> = const { std::cell::RefCell::new(None) };
}
