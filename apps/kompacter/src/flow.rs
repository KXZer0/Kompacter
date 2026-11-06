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
use crate::{format, kind_name, output, run, sys, App, Current, DiffRow, View};
use slint::{ModelRc, VecModel};

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

    /// One file opens normally; several get the list screen ("Compress all").
    pub fn open_many(self: &Rc<Self>, mut files: Vec<PathBuf>) {
        files.retain(|p| p.exists());
        match files.len() {
            0 => {}
            1 => self.open(files.remove(0)),
            n => {
                let ui = self.ui();
                if matches!(ui.get_view(), View::Busy | View::Reading) {
                    return;
                }
                *self.current.borrow_mut() = None;
                let target = self.target().bytes;
                let size = |p: &PathBuf| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                let total: u64 = files.iter().map(size).sum();
                ui.set_file_name(format!("{n} files").into());
                ui.set_file_meta(bytes(total).into());
                ui.set_file_kind("file".into());
                ui.set_file_icon(slint::Image::default());
                let over = files.iter().filter(|p| size(p) > target).count();
                ui.set_over_ok(over == 0);
                ui.set_over_text(
                    if over == 0 {
                        "All fit".to_string()
                    } else {
                        format!("{over} of {n} too big")
                    }
                    .into(),
                );
                let max = files.iter().map(size).max().unwrap_or(0).max(target) as f64 * 1.08;
                ui.set_meter_fill((files.iter().map(size).max().unwrap_or(0) as f64 / max) as f32);
                ui.set_meter_limit((target as f64 / max) as f32);
                let rows: Vec<DiffRow> = files
                    .iter()
                    .map(|p| {
                        let b = size(p);
                        let after = if b > target {
                            format!("≤ {}", bytes(target))
                        } else {
                            bytes(b)
                        };
                        DiffRow {
                            depth: 0,
                            icon: if p.is_dir() { "dir" } else { "file" }.into(),
                            label: file_name(p).into(),
                            before: bytes(b).into(),
                            changed: b > target,
                            after: after.into(),
                        }
                    })
                    .collect();
                ui.set_diff(ModelRc::new(VecModel::from(rows)));
                ui.set_diff_open(true);
                ui.set_compress_label(format!("Compress all {n}").into());
                ui.set_can_trim(false);
                ui.set_trim_first(false);
                ui.set_split_count(0);
                ui.set_foot_text(slint::SharedString::new());
                ui.set_chomp(ui.get_chomp() + 1);
                *self.batch.borrow_mut() = files;
                self.set_view(View::Big);
            }
        }
    }

    /// Compress every file in the batch to the limit, one after another, then copy them
    /// all together. A file that can't fit is skipped and counted.
    pub fn compress_batch(self: &Rc<Self>) {
        let files = self.batch.borrow().clone();
        if files.is_empty() {
            return;
        }
        let ui = self.ui();
        let prefs = self.settings.borrow().clone();
        let target = self.target().bytes;
        let opts = self.options(target);
        self.cancel.store(false, Ordering::Relaxed);
        self.started.set(Some(Instant::now()));
        ui.set_progress(0.0);
        ui.set_busy_left("Estimating time…".into());
        self.set_view(View::Busy);
        self.wake();
        self.start_eta();

        let engine = self.engine.clone();
        let cancel = self.cancel.clone();
        let ui_weak = self.ui.clone();
        let weak = Rc::downgrade(self);
        let n = files.len();
        run(
            move || {
                let mut made = Vec::new();
                let mut failed = 0;
                for (i, path) in files.iter().enumerate() {
                    let mut opts = opts.clone();
                    opts.out_dir = output::dir(&prefs, path).ok();
                    let mut last = -1;
                    let mut on_progress = |p: f32| {
                        let total = (i as f32 + p) / n as f32;
                        let step = (total * 200.0) as i32;
                        if step != last {
                            last = step;
                            let w = ui_weak.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(ui) = w.upgrade() {
                                    ui.set_progress(total);
                                    ui.set_squeeze(total);
                                }
                            });
                        }
                    };
                    match engine.compress(path, &opts, &mut on_progress, &cancel) {
                        Ok(out) if out.fits => made.push(out.path),
                        _ if cancel.load(Ordering::Relaxed) => return Err(()),
                        _ => failed += 1,
                    }
                }
                let copied = !made.is_empty() && sys::copy_files(&made).is_ok();
                if let Some(last) = made.last() {
                    output::clean(Some(last));
                }
                Ok((made, failed, copied))
            },
            move |r| {
                let Some(app) = weak.upgrade() else { return };
                ETA.with(|e| *e.borrow_mut() = None);
                let ui = app.ui();
                let Ok((made, failed, copied)) = r else {
                    // Cancelled: back to the list.
                    let files = app.batch.borrow().clone();
                    app.open_many(files);
                    return;
                };
                ui.set_progress(1.0);
                ui.set_squeeze(1.0);
                let ok = !made.is_empty();
                let title = match (ok, copied) {
                    (false, _) => "Still too big".to_string(),
                    (true, true) => format!("Copied {} files", made.len()),
                    (true, false) => format!("Saved {} files", made.len()),
                };
                let sub = if failed > 0 {
                    format!("{failed} of {n} couldn't fit")
                } else {
                    format!("Each under {}", bytes(target))
                };
                let weak = Rc::downgrade(&app);
                slint::Timer::single_shot(Duration::from_millis(420), move || {
                    let Some(app) = weak.upgrade() else { return };
                    let ui = app.ui();
                    ui.set_done_ok(ok);
                    ui.set_done_split(false);
                    ui.set_done_title(title.into());
                    ui.set_done_sub(sub.into());
                    *app.result.borrow_mut() = made.first().cloned();
                    app.batch.borrow_mut().clear();
                    app.set_view(View::Done);
                    if ok {
                        ui.set_hop(ui.get_hop() + 1);
                    }
                });
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

        // The button just says Compress; "Show more" has the before → after details.
        let plan = c.estimate.video.as_ref();
        let label = if folder {
            format!("Compress & {}", self.settings.borrow().archive_format.ext())
        } else {
            "Compress".into()
        };
        ui.set_diff(ModelRc::new(VecModel::from(self.diff_rows(c, target))));
        ui.set_diff_open(false);
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

    /// Before → after lines for "Show more": a folder's whole tree, or a video's stats.
    fn diff_rows(&self, c: &Current, target: u64) -> Vec<DiffRow> {
        let row = |depth: i32, icon: &str, label: String, before: String, after: String| DiffRow {
            changed: before != after,
            depth,
            icon: icon.into(),
            label: label.into(),
            before: before.into(),
            after: after.into(),
        };
        match c.info.kind {
            MediaKind::Folder => self
                .engine
                .folder_preview(&c.path, target)
                .unwrap_or_default()
                .into_iter()
                .take(400)
                .map(|r| {
                    let icon = if r.is_dir { "dir" } else { "file" };
                    row(
                        r.depth as i32,
                        icon,
                        r.name,
                        bytes(r.before),
                        bytes(r.after),
                    )
                })
                .collect(),
            MediaKind::Video | MediaKind::Audio => {
                let mut rows = Vec::new();
                let codec = self.settings.borrow().video_codec;
                if let (Some(v), Some(p)) = (&c.info.video, &c.estimate.video) {
                    rows.push(row(
                        0,
                        "",
                        "Resolution".into(),
                        format!("{}×{}", v.width, v.height),
                        format!("{}×{}", p.width, p.height),
                    ));
                    rows.push(row(
                        0,
                        "",
                        "Frame rate".into(),
                        format!("{} fps", v.fps.round()),
                        format!("{} fps", p.fps.round()),
                    ));
                    rows.push(row(
                        0,
                        "",
                        "Codec".into(),
                        v.codec
                            .to_uppercase()
                            .replace("HEVC", "H.265")
                            .replace("H264", "H.264"),
                        codec.label().into(),
                    ));
                    let kbps = if c.info.duration > 0.0 {
                        (c.info.size_bytes as f64 * 8.0 / c.info.duration / 1000.0) as u32
                    } else {
                        0
                    };
                    rows.push(row(
                        0,
                        "",
                        "Bitrate".into(),
                        format::rate(kbps),
                        format::rate(p.video_kbps + p.audio_kbps),
                    ));
                } else if let Some(k) = c.estimate.audio_kbps {
                    rows.push(row(
                        0,
                        "",
                        "Bitrate".into(),
                        "Original".into(),
                        format!("{k} kbps"),
                    ));
                }
                let after = c.info.size_bytes.min(target);
                rows.push(row(
                    0,
                    "",
                    "Size".into(),
                    bytes(c.info.size_bytes),
                    format!("≤ {}", bytes(after)),
                ));
                rows
            }
            _ => Vec::new(),
        }
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
                                // Slint glides the press and the % to this.
                                ui.set_squeeze(p);
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
                        app.ui().set_squeeze(1.0);
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

    /// "About 40 s left": counts down smoothly instead of jumping with every estimate.
    /// The raw estimate is noisy, so the shown value ticks down with the clock and only
    /// drifts toward the estimate.
    fn start_eta(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let timer = slint::Timer::default();
        let shown = std::cell::Cell::new(None::<f64>);
        let step = 0.25;
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(250),
            move || {
                let Some(app) = weak.upgrade() else { return };
                let ui = app.ui();
                if ui.get_view() != View::Busy {
                    return;
                }
                let Some(t) = app.started.get() else { return };
                let (elapsed, p) = (t.elapsed().as_secs_f64(), ui.get_progress() as f64);
                let text = if p >= 0.995 {
                    "Finishing…".to_string()
                } else if p < 0.03 || elapsed < 1.0 {
                    "Estimating time…".to_string()
                } else {
                    let estimate = elapsed * (1.0 - p) / p;
                    let next = match shown.get() {
                        None => estimate,
                        Some(s) => {
                            let ticked = (s - step).max(0.0);
                            ticked + (estimate - ticked) * 0.15
                        }
                    };
                    shown.set(Some(next));
                    format::seconds_left(next)
                };
                ui.set_busy_left(text.into());
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
