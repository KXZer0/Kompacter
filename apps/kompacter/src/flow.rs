//! The main flow: a file arrives → read it → compress (now, or after the "too big" screen
//! or the trimmer) → copied to the clipboard.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kompacter_core::{MediaKind, Mode, Outcome, Trim};

use crate::format::{bytes, file_name};
use crate::{format, kind_name, output, run, sys, App, AppWindow, Current, DiffRow, View};
use slint::{ComponentHandle, ModelRc, VecModel};

impl App {
    /// A file arrived (drop, paste, choose). Read it, then compress right away if that's
    /// the obvious next step, or show the choices.
    pub fn open(self: &Rc<Self>, path: PathBuf) {
        let ui = self.ui();
        if self.started.get().is_some() || matches!(self.view(), View::Busy | View::Reading) {
            return;
        }
        *self.current.borrow_mut() = None;
        self.batch.borrow_mut().clear();
        ui.set_file_name(file_name(&path).into());
        ui.set_file_icon(slint::Image::default());
        ui.set_chomp(ui.get_chomp() + 1);
        self.set_view(View::Reading);

        let engine = self.engine.clone();
        let target = self.target_bytes();
        let opts = self.options(target);
        let weak = Rc::downgrade(self);
        let p = path.clone();
        run(
            move || {
                let info = engine.probe(&p).map_err(|e| e.to_string())?;
                let estimate = engine.estimate(&info, &opts).map_err(|e| e.to_string())?;
                let mut folder_rows = if info.kind == MediaKind::Folder {
                    engine.folder_preview(&p, target).unwrap_or_default()
                } else {
                    Vec::new()
                };
                folder_rows.truncate(400);
                folder_rows.shrink_to_fit();
                Ok::<_, String>((info, estimate, folder_rows, sys::file_icon(&p)))
            },
            move |r| {
                let Some(app) = weak.upgrade() else { return };
                match r {
                    Err(e) => app.fail(&e),
                    Ok((info, estimate, folder_rows, icon)) => {
                        let kind = info.kind;
                        // The paste guard always offers its choices first.
                        let direct = !matches!(kind, MediaKind::Folder)
                            && (estimate.fits_as_is || !matches!(kind, MediaKind::Video))
                            && app.guard.get().is_none();
                        *app.current.borrow_mut() = Some(Current {
                            path,
                            info,
                            estimate,
                            folder_rows,
                        });
                        app.ui().set_file_icon(file_image(icon));
                        if direct {
                            app.compress(None, Mode::Fit, false);
                        } else {
                            app.show_big();
                        }
                    }
                }
            },
        );
    }

    /// One file opens normally; several get the list screen ("Compress all").
    pub fn open_many(self: &Rc<Self>, files: Vec<PathBuf>) {
        if self.started.get().is_some() || matches!(self.view(), View::Busy | View::Reading) {
            return;
        }
        let mut files: Vec<_> = files
            .into_iter()
            .filter_map(|path| std::fs::metadata(&path).ok().map(|m| (path, m)))
            .collect();
        match files.len() {
            0 => {}
            1 => self.open(files.remove(0).0),
            n => {
                let ui = self.ui();
                *self.current.borrow_mut() = None;
                let target = self.target_bytes();
                let total: u64 = files.iter().map(|(_, m)| m.len()).sum();
                ui.set_file_name(format!("{n} files").into());
                ui.set_file_meta(bytes(total).into());
                ui.set_file_kind("file".into());
                ui.set_file_icon(slint::Image::default());
                let over = files.iter().filter(|(_, m)| m.len() > target).count();
                ui.set_over_ok(over == 0);
                ui.set_over_text(
                    if over == 0 {
                        "All fit".to_string()
                    } else {
                        format!("{over} of {n} too big")
                    }
                    .into(),
                );
                let largest = files.iter().map(|(_, m)| m.len()).max().unwrap_or(0);
                let max = largest.max(target) as f64 * 1.08;
                ui.set_meter_fill((largest as f64 / max) as f32);
                ui.set_meter_limit((target as f64 / max) as f32);
                let rows: Vec<DiffRow> = files
                    .iter()
                    .map(|(p, m)| {
                        let b = m.len();
                        let after = if b > target {
                            format!("≤ {}", bytes(target))
                        } else {
                            bytes(b)
                        };
                        DiffRow {
                            depth: 0,
                            icon: if m.is_dir() { "dir" } else { "file" }.into(),
                            label: file_name(p).into(),
                            before: bytes(b).into(),
                            changed: b > target,
                            after: after.into(),
                        }
                    })
                    .collect();
                let after: u64 = files.iter().map(|(_, m)| m.len().min(target)).sum();
                ui.set_sum_before(bytes(total).into());
                ui.set_sum_after(format!("≤ {}", bytes(after)).into());
                ui.set_diff(ModelRc::new(VecModel::from(rows)));
                ui.set_diff_open(true);
                ui.set_compress_label(format!("Compress all {n}").into());
                ui.set_can_trim(false);
                ui.set_trim_first(false);
                ui.set_split_count(0);
                ui.set_foot_text(slint::SharedString::new());
                ui.set_chomp(ui.get_chomp() + 1);
                *self.batch.borrow_mut() = files.into_iter().map(|(p, _)| p).collect();
                self.set_view(View::Big);
            }
        }
    }

    /// Compress every file in the batch to the limit, one after another, then copy them
    /// all together. A file that can't fit is skipped and counted.
    pub fn compress_batch(self: &Rc<Self>) {
        let files = self.batch.borrow().clone();
        if files.is_empty() || self.started.get().is_some() {
            return;
        }
        let ui = self.ui();
        let prefs = self.settings.borrow().clone();
        let target = self.target_bytes();
        let opts = self.options(target);
        self.cancel.store(false, Ordering::Relaxed);
        self.started.set(Some(Instant::now()));
        ui.set_progress(0.0);
        ui.set_busy_left(Default::default());
        self.set_view(View::Busy);
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
                let mut progress = publish_progress(ui_weak);
                for (i, path) in files.iter().enumerate() {
                    let mut opts = opts.clone();
                    opts.out_dir = output::dir(&prefs, path).ok();
                    let mut on_progress = |p: f32| progress((i as f32 + p) / n as f32);
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
                    app.started.set(None);
                    let files = app.batch.borrow().clone();
                    app.set_view(View::Big);
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
                    app.started.set(None);
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
                    if !app.guard_done(ok && copied) {
                        app.auto_hide_result();
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
        let target = self.target_bytes();
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
        let label = if self.guard.get().is_some() {
            "Compress & paste".into()
        } else if folder {
            format!("Compress & {}", self.settings.borrow().archive_format.ext())
        } else {
            "Compress".into()
        };
        let rows = self.diff_rows(c, target);
        // Big summary: the folder's own row when there's a tree, else the size cap.
        let after = match (folder, rows.first()) {
            (true, Some(r)) => r.after.to_string(),
            _ if size > target => format!("≤ {}", bytes(target)),
            _ => bytes(size),
        };
        ui.set_sum_before(bytes(size).into());
        ui.set_sum_after(after.into());
        ui.set_diff(ModelRc::new(VecModel::from(rows)));
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
            MediaKind::Folder => c
                .folder_rows
                .iter()
                .map(|r| {
                    let icon = if r.is_dir { "dir" } else { "file" };
                    row(
                        r.depth as i32,
                        icon,
                        r.name.clone(),
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

    /// Compress the current file. `range` trims it; `mode` is Fit, KeepQuality (a cut
    /// without re-encoding) or Gif; `split` makes parts that each fit when one can't.
    pub fn compress(self: &Rc<Self>, range: Option<(f64, f64)>, mode: Mode, split: bool) {
        if self.started.get().is_some() {
            return;
        }
        let keep = mode == Mode::KeepQuality;
        let ui = self.ui();
        let cur = self.current.borrow();
        let Some(c) = cur.as_ref() else { return };
        let path = c.path.clone();
        let quick = c.estimate.fits_as_is && range.is_none() && mode != Mode::Gif;
        let height = if keep || mode == Mode::Gif {
            None
        } else {
            c.estimate.video.as_ref().map(|v| v.height)
        };
        drop(cur);

        let prefs = self.settings.borrow().clone();
        let target = self.target_bytes();
        let mut opts = self.options(target);
        opts.mode = mode;
        opts.split = split;
        self.last_range.set(range);
        if let Some((a, b)) = range {
            opts.trim = Trim::new(a, b).ok();
        }

        self.cancel.store(false, Ordering::Relaxed);
        self.started.set(Some(Instant::now()));
        ui.set_progress(0.0);
        ui.set_busy_left(Default::default());
        if !quick {
            self.set_view(View::Busy);
            self.start_eta();
        }

        let engine = self.engine.clone();
        let cancel = self.cancel.clone();
        let ui_weak = self.ui.clone();
        let weak = Rc::downgrade(self);
        let from_trim = range.is_some();
        run(
            move || {
                opts.out_dir = Some(output::dir(&prefs, &path)?);
                let mut on_progress = publish_progress(ui_weak);
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
                ETA.with(|e| *e.borrow_mut() = None);
                match r {
                    Ok((out, copied)) => {
                        let ui = app.ui();
                        ui.set_progress(1.0);
                        ui.set_squeeze(1.0);
                        // Let the press finish its squeeze before the result appears.
                        let weak = Rc::downgrade(&app);
                        let delay = if quick { 0 } else { 420 };
                        slint::Timer::single_shot(Duration::from_millis(delay), move || {
                            if let Some(app) = weak.upgrade() {
                                app.done(out, copied, height, mode);
                            }
                        });
                    }
                    Err(_) if app.cancel.load(Ordering::Relaxed) => {
                        app.started.set(None);
                        if from_trim {
                            app.set_view(View::Trim);
                            app.trim_activate();
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
                if app.view() != View::Busy || !ui.window().is_visible() {
                    return;
                }
                let Some(t) = app.started.get() else { return };
                let (elapsed, p) = (t.elapsed().as_secs_f64(), ui.get_progress() as f64);
                let text = if !(0.03..0.99).contains(&p) || elapsed < 1.0 {
                    String::new()
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

    fn done(self: &Rc<Self>, out: Outcome, copied: bool, height: Option<u32>, mode: Mode) {
        self.started.set(None);
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
        if mode == Mode::Gif {
            details.push("GIF".into());
        } else if mode == Mode::KeepQuality {
            details.push("original quality".into());
        } else if let Some(h) = height.filter(|_| !out.untouched) {
            details.push(format!("{h}p"));
        }
        if out.untouched {
            details.push("already fit".into());
        } else if ratio >= 1.1 {
            details.push(format!("{ratio:.1}× smaller"));
        }
        let target = self.target_bytes();
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
        // Paste guard: straight into Discord. Quick bar: fold away after a moment.
        if self.guard_done(out.fits && copied) {
            return;
        }
        self.auto_hide_result();
    }

    pub fn fail(&self, e: &str) {
        self.started.set(None);
        ETA.with(|t| *t.borrow_mut() = None);
        let ui = self.ui();
        ui.set_error_text(e.into());
        self.set_view(View::Error);
    }
}

fn publish_progress(ui: slint::Weak<AppWindow>) -> impl FnMut(f32) {
    let latest = Arc::new(Mutex::new(None));
    let mut last_step = -1;
    let mut last_sent = None::<Instant>;
    move |progress| {
        let step = (progress * 200.0) as i32;
        if step == last_step
            || (progress < 1.0
                && last_sent.is_some_and(|t| t.elapsed() < Duration::from_millis(80)))
        {
            return;
        }
        last_step = step;
        last_sent = Some(Instant::now());
        let notify = latest.lock().unwrap().replace(progress).is_none();
        if notify {
            let latest = latest.clone();
            let ui = ui.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let progress = latest.lock().unwrap().take();
                if let (Some(ui), Some(p)) = (ui.upgrade(), progress) {
                    ui.set_progress(p);
                    ui.set_squeeze(p);
                }
            });
        }
    }
}

/// The file's own Windows icon (empty elsewhere: the UI draws a line icon).
fn file_image(icon: Option<(u32, u32, Vec<u8>)>) -> slint::Image {
    icon.map(|(w, h, px)| {
        slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(&px, w, h))
    })
    .unwrap_or_default()
}

thread_local! {
    static ETA: std::cell::RefCell<Option<slint::Timer>> = const { std::cell::RefCell::new(None) };
}
