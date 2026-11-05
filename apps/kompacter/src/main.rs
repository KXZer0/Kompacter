// No console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Kompacter: a tray app that shrinks any file to fit an upload limit and puts the result
//! on the clipboard. The window is drawn by Slint (ui/*.slint); this file holds the logic.
//! Heavy work (probing, compressing) runs on worker threads; results come back to the UI
//! thread with `slint::invoke_from_event_loop`. Nothing runs while the window is hidden.

mod format;
mod output;
mod settings;
mod sys;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kompacter_core::size::PRESETS;
use kompacter_core::{Engine, Estimate, MediaInfo, MediaKind, Mode, Options, Outcome};
use slint::winit_030::{winit, EventResult, WinitWindowAccessor};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::format::{bytes, file_name};
use crate::settings::Settings;

slint::include_modules!();

const CARD_W: f64 = 360.0;
const CARD_H: f64 = 472.0;
const SETTINGS_H: f64 = 600.0;

/// A preset as shown: the engine's presets plus "Custom".
struct Target {
    id: String,
    name: String,
    note: String,
    brand: String,
    bytes: u64,
}

fn targets(custom_bytes: u64) -> Vec<Target> {
    let mut list: Vec<Target> = PRESETS
        .iter()
        .map(|p| {
            let (name, note) = match p.id {
                "discord" => ("Discord", "Free"),
                "discord-nitro-basic" => ("Nitro Basic", "Discord"),
                "discord-nitro" => ("Nitro", "Discord"),
                id if id.starts_with("email") => ("Email", "Attachment"),
                _ => (p.label, ""),
            };
            let brand = if p.id.starts_with("discord") {
                "discord"
            } else {
                "mail"
            };
            Target {
                id: p.id.into(),
                name: name.into(),
                note: note.into(),
                brand: brand.into(),
                bytes: p.bytes,
            }
        })
        .collect();
    list.push(Target {
        id: "custom".into(),
        name: "Custom".into(),
        note: String::new(),
        brand: "custom".into(),
        bytes: custom_bytes,
    });
    list
}

/// The file currently in the window.
struct Current {
    path: PathBuf,
    info: MediaInfo,
    estimate: Estimate,
}

struct App {
    ui: slint::Weak<AppWindow>,
    engine: Arc<Engine>,
    cancel: Arc<AtomicBool>,
    settings: RefCell<Settings>,
    current: RefCell<Option<Current>>,
    result: RefCell<Option<PathBuf>>,
    started: Cell<Option<Instant>>,
    /// A file dialog is open: don't auto-hide while the user picks.
    modal: Cell<bool>,
    /// When the window last hid itself on focus loss (a tray click causes that too).
    auto_hidden: Cell<Option<Instant>>,
}

impl App {
    fn ui(&self) -> AppWindow {
        self.ui.upgrade().expect("window")
    }

    fn target(&self) -> Target {
        let s = self.settings.borrow();
        let list = targets(s.custom_bytes);
        let i = list.iter().position(|t| t.id == s.preset).unwrap_or(0);
        list.into_iter().nth(i).expect("presets")
    }

    fn save(&self) {
        settings::save(&self.settings.borrow());
    }

    /// Push settings and presets into the window.
    fn sync_settings(&self) {
        let ui = self.ui();
        let s = self.settings.borrow();
        let list = targets(s.custom_bytes);
        let items: Vec<PresetItem> = list
            .iter()
            .map(|t| PresetItem {
                name: t.name.as_str().into(),
                note: t.note.as_str().into(),
                size: bytes(t.bytes).into(),
                brand: t.brand.as_str().into(),
            })
            .collect();
        let index = list.iter().position(|t| t.id == s.preset).unwrap_or(0);
        ui.set_presets(ModelRc::new(VecModel::from(items)));
        ui.set_preset_index(index as i32);
        ui.set_target_text(bytes(list[index].bytes).into());
        ui.set_theme_index(match s.theme.as_str() {
            "light" => 1,
            "dark" => 2,
            _ => 0,
        });
        ui.set_output_index(match s.output.as_str() {
            "beside" => 1,
            "custom" => 2,
            "replace" => 3,
            _ => 0,
        });
        ui.set_output_dir(s.output_dir.as_str().into());
        ui.set_gpu(s.gpu);
        ui.set_auto_hide(s.auto_hide);
        ui.set_start_with_windows(s.start_with_windows);
        ui.set_custom_mb((s.custom_bytes / (1024 * 1024)) as i32);
        drop(s);
        self.apply_theme();
    }

    fn apply_theme(&self) {
        let dark = match self.settings.borrow().theme.as_str() {
            "light" => false,
            "dark" => true,
            _ => sys::system_dark(),
        };
        self.ui().global::<Theme>().set_dark(dark);
    }

    fn set_view(&self, v: View) {
        let ui = self.ui();
        let was_tall = ui.get_view() == View::Settings;
        ui.set_view(v);
        if was_tall != (v == View::Settings) && ui.window().is_visible() {
            self.place();
        }
    }

    /// Put the window in the bottom-right corner above the taskbar.
    fn place(&self) {
        let ui = self.ui();
        let h = if ui.get_view() == View::Settings {
            SETTINGS_H
        } else {
            CARD_H
        };
        if let Some((right, bottom, scale)) = sys::corner() {
            let margin = 12.0 * scale;
            let x = right as f64 - CARD_W * scale - margin;
            let y = bottom as f64 - h * scale - margin;
            ui.window()
                .set_position(slint::PhysicalPosition::new(x as i32, y as i32));
        }
    }

    fn show(&self) {
        let ui = self.ui();
        self.apply_theme();
        self.place();
        let _ = ui.show();
        // Position again once the window exists (first show), then focus it.
        self.place();
        ui.window().with_winit_window(|w| w.focus_window());
    }

    fn hide(&self) {
        let ui = self.ui();
        if ui.get_view() == View::Settings {
            self.close_settings();
        }
        let _ = ui.hide();
    }

    fn toggle(&self) {
        if self.ui().window().is_visible() {
            self.hide();
        } else if self
            .auto_hidden
            .get()
            .is_some_and(|t| t.elapsed() < Duration::from_millis(400))
        {
            // This click is what made the window lose focus and hide; leave it closed.
        } else {
            self.show();
        }
    }

    fn close_settings(&self) {
        let back = if self.current.borrow().is_some() {
            View::Big
        } else {
            View::Idle
        };
        self.set_view(back);
    }

    /// A file arrived (drop, paste, choose). Read it, then compress right away if that's
    /// the obvious next step, or show the choices.
    fn open(self: &Rc<Self>, path: PathBuf) {
        let ui = self.ui();
        if matches!(ui.get_view(), View::Busy | View::Reading) {
            return;
        }
        *self.current.borrow_mut() = None;
        ui.set_file_name(file_name(&path).into());
        self.set_view(View::Reading);

        let engine = self.engine.clone();
        let target = self.target().bytes;
        let weak = Rc::downgrade(self);
        let p = path.clone();
        run(
            move || {
                let info = engine.probe(&p).map_err(|e| e.to_string())?;
                let estimate = engine
                    .estimate(&info, &Options::new(target))
                    .map_err(|e| e.to_string())?;
                Ok::<_, String>((info, estimate))
            },
            move |r| {
                let Some(app) = weak.upgrade() else { return };
                match r {
                    Err(e) => app.fail(&path, &e),
                    Ok((info, estimate)) => {
                        let direct = estimate.fits_as_is || !matches!(info.kind, MediaKind::Video);
                        *app.current.borrow_mut() = Some(Current {
                            path,
                            info,
                            estimate,
                        });
                        if direct {
                            app.compress();
                        } else {
                            app.show_big();
                        }
                    }
                }
            },
        );
    }

    /// "Too big" view: what the file is and what compressing would give.
    fn show_big(&self) {
        let ui = self.ui();
        let cur = self.current.borrow();
        let Some(c) = cur.as_ref() else { return };
        let target = self.target().bytes;
        let size = c.info.size_bytes;

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
        ui.set_over_text(format!("{} too big", bytes(size.saturating_sub(target))).into());
        // The button says what it will do; a line appears only as a warning.
        let (label, warn) = match &c.estimate.video {
            Some(v) if v.fits => (format!("Compress to {}p", v.height), ""),
            Some(_) => (
                "Compress".to_string(),
                "Quality will be low. Trim it first for a better result.",
            ),
            None => ("Compress".to_string(), ""),
        };
        ui.set_compress_label(label.into());
        ui.set_foot_text(warn.into());
        drop(cur);
        self.set_view(View::Big);
    }

    fn compress(self: &Rc<Self>) {
        let ui = self.ui();
        let cur = self.current.borrow();
        let Some(c) = cur.as_ref() else { return };
        let path = c.path.clone();
        let quick = c.estimate.fits_as_is;
        let height = c.estimate.video.as_ref().map(|v| v.height);
        drop(cur);

        let prefs = self.settings.borrow().clone();
        let target = self.target().bytes;
        let mut opts = Options::new(target);
        opts.mode = Mode::Fit;
        opts.gpu = prefs.gpu;
        opts.out_dir = match output::dir(&prefs, &path) {
            Ok(d) => Some(d),
            Err(e) => return self.fail(&path, &e),
        };

        self.cancel.store(false, Ordering::Relaxed);
        self.started.set(Some(Instant::now()));
        ui.set_progress(0.0);
        ui.set_busy_left("Estimating time…".into());
        if !quick {
            self.set_view(View::Busy);
        }

        let engine = self.engine.clone();
        let cancel = self.cancel.clone();
        let ui_weak = self.ui.clone();
        let weak = Rc::downgrade(self);
        let original = path.clone();
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
                let copied = out.fits && sys::copy_files(std::slice::from_ref(&out.path)).is_ok();
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
                        let delay = if quick { 0 } else { 380 };
                        slint::Timer::single_shot(Duration::from_millis(delay), move || {
                            if let Some(app) = weak.upgrade() {
                                app.done(out, copied, height);
                            }
                        });
                    }
                    Err(_) if app.cancel.load(Ordering::Relaxed) => {
                        if app.current.borrow().as_ref().is_some_and(|c| {
                            c.estimate.fits_as_is || !matches!(c.info.kind, MediaKind::Video)
                        }) {
                            app.another();
                        } else {
                            app.show_big();
                        }
                    }
                    Err(e) => app.fail(&original, &e),
                }
            },
        );
    }

    fn done(&self, out: Outcome, copied: bool, height: Option<u32>) {
        let ui = self.ui();
        let title = if !out.fits {
            "Still too big"
        } else if copied {
            "Copied"
        } else {
            "Saved"
        };
        let sub = if !out.fits {
            format!(
                "{} · trim it or pick a bigger target",
                bytes(out.output_bytes)
            )
        } else if !copied {
            "Couldn't copy it — drag it from the folder".into()
        } else {
            String::new()
        };
        let ratio = out.original_bytes as f64 / out.output_bytes.max(1) as f64;
        let mut tip = vec![bytes(out.output_bytes)];
        if let Some(h) = height {
            tip.push(format!("{h}p"));
        }
        if out.untouched {
            tip.push("already fit".into());
        } else if ratio >= 1.1 {
            tip.push(format!("{ratio:.1}× smaller"));
        }
        ui.set_done_sub(sub.into());
        ui.set_done_tip(tip.join(" · ").into());
        ui.set_done_ok(out.fits);
        ui.set_done_title(title.into());
        *self.result.borrow_mut() = Some(out.path);
        self.set_view(View::Done);
        // If the user already went back to their chat, fold away after a moment.
        if !ui
            .window()
            .with_winit_window(|w| w.has_focus())
            .unwrap_or(true)
        {
            self.auto_hide_later(2500);
        }
    }

    fn fail(&self, path: &Path, e: &str) {
        let ui = self.ui();
        let _ = path;
        ui.set_error_text(e.into());
        self.set_view(View::Error);
    }

    fn another(&self) {
        *self.current.borrow_mut() = None;
        self.set_view(View::Idle);
    }

    fn auto_hide_later(&self, ms: u64) {
        let ui = self.ui.clone();
        let auto = self.settings.borrow().auto_hide;
        if !auto {
            return;
        }
        slint::Timer::single_shot(Duration::from_millis(ms), move || {
            if let Some(ui) = ui.upgrade() {
                let focused = ui
                    .window()
                    .with_winit_window(|w| w.has_focus())
                    .unwrap_or(true);
                if !focused && ui.get_view() != View::Busy && !ui.get_dragging() {
                    let _ = ui.hide();
                }
            }
        });
    }

    /// Native file or folder picker on a worker thread, so the window keeps drawing.
    fn pick(self: &Rc<Self>, folder: bool, then: impl FnOnce(&Rc<App>, PathBuf) + 'static) {
        self.modal.set(true);
        let weak = Rc::downgrade(self);
        run(
            move || {
                let d = rfd::FileDialog::new();
                if folder {
                    d.pick_folder()
                } else {
                    d.pick_file()
                }
            },
            move |picked| {
                let Some(app) = weak.upgrade() else { return };
                app.modal.set(false);
                app.show();
                if let Some(p) = picked {
                    then(&app, p);
                }
            },
        );
    }
}

fn kind_name(k: MediaKind) -> &'static str {
    match k {
        MediaKind::Video => "video",
        MediaKind::Audio => "audio",
        MediaKind::Image => "image",
        MediaKind::Folder => "folder",
        MediaKind::Other => "file",
    }
}

/// Run `work` on a worker thread, then `done` with its result on the UI thread.
fn run<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + 'static,
) {
    let done = send_wrapper(done);
    std::thread::spawn(move || {
        let r = work();
        let _ = slint::invoke_from_event_loop(move || (done.take())(r));
    });
}

/// Lets a UI-thread-only closure ride along to a worker thread and back. It's only ever
/// called on the UI thread (inside `invoke_from_event_loop`).
struct UiOnly<F>(Option<F>, std::thread::ThreadId);
unsafe impl<F> Send for UiOnly<F> {}
impl<F> UiOnly<F> {
    fn take(mut self) -> F {
        assert_eq!(
            std::thread::current().id(),
            self.1,
            "UI closure on another thread"
        );
        self.0.take().expect("called once")
    }
}
impl<F> Drop for UiOnly<F> {
    fn drop(&mut self) {
        // Never drop the closure on a worker thread; leak it instead (only if the event
        // loop is already gone, at exit).
        if std::thread::current().id() != self.1 {
            std::mem::forget(self.0.take());
        }
    }
}
fn send_wrapper<F>(f: F) -> UiOnly<F> {
    UiOnly(Some(f), std::thread::current().id())
}

fn main() -> Result<(), slint::PlatformError> {
    if !sys::first_instance() {
        return Ok(());
    }
    let quiet = std::env::args().any(|a| a == "--tray");

    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .with_winit_window_attributes_hook(|attrs| {
            #[cfg(windows)]
            {
                use winit::platform::windows::{CornerPreference, WindowAttributesExtWindows};
                attrs
                    .with_skip_taskbar(true)
                    .with_undecorated_shadow(true)
                    .with_corner_preference(CornerPreference::Round)
            }
            #[cfg(not(windows))]
            attrs
        })
        .select()?;

    let ui = AppWindow::new()?;
    let tray = Tray::new()?;
    let app = Rc::new(App {
        ui: ui.as_weak(),
        engine: Arc::new(Engine::new(sys::ffmpeg())),
        cancel: Arc::new(AtomicBool::new(false)),
        settings: RefCell::new(settings::load()),
        current: RefCell::new(None),
        result: RefCell::new(None),
        started: Cell::new(None),
        modal: Cell::new(false),
        auto_hidden: Cell::new(None),
    });
    // The Run entry is the truth (it can be turned off in Task Manager).
    app.settings.borrow_mut().start_with_windows = sys::starts_with_windows();
    app.sync_settings();
    ui.set_dash_path(format::dashed_round_rect(308.0, 300.0, 28.0, 6.0, 7.0).into());

    // Find GPU encoders in the background (so the first compress doesn't wait), then
    // tidy old outputs.
    {
        let engine = app.engine.clone();
        let w = ui.as_weak();
        std::thread::spawn(move || {
            let none = engine.hw_encoders().is_empty();
            let _ = w.upgrade_in_event_loop(move |ui| {
                ui.set_gpu_text(
                    if none {
                        "No GPU found — uses the CPU"
                    } else {
                        ""
                    }
                    .into(),
                )
            });
            output::clean(None);
        });
    }

    // ── Tray ──
    {
        let a = app.clone();
        tray.on_open(move || a.toggle());
        tray.on_open_folder(|| {
            if let Ok(d) = output::app_dir() {
                sys::open_folder(&d);
            }
        });
        tray.on_quit(|| {
            let _ = slint::quit_event_loop();
        });
    }

    // ── Window events: focus (auto-hide), files dragged in, theme changes ──
    {
        let a = app.clone();
        ui.window().on_winit_window_event(move |_, event| {
            let ui = a.ui();
            match event {
                winit::event::WindowEvent::Focused(false) => {
                    let s = a.settings.borrow();
                    if s.auto_hide
                        && !a.modal.get()
                        && !ui.get_dragging()
                        && ui.get_view() != View::Busy
                    {
                        drop(s);
                        a.auto_hidden.set(Some(Instant::now()));
                        a.hide();
                    }
                }
                winit::event::WindowEvent::HoveredFile(_) => {
                    if ui.get_view() != View::Busy && !ui.get_dragging() {
                        if !matches!(ui.get_view(), View::Idle | View::Reading) {
                            a.another();
                        }
                        ui.set_dragging(true);
                    }
                }
                winit::event::WindowEvent::HoveredFileCancelled => ui.set_dragging(false),
                winit::event::WindowEvent::DroppedFile(p) => {
                    ui.set_dragging(false);
                    a.open(p.clone());
                }
                winit::event::WindowEvent::ThemeChanged(_) => a.apply_theme(),
                _ => {}
            }
            EventResult::Propagate
        });
    }

    // ── Buttons ──
    {
        let a = app.clone();
        ui.on_choose_file(move || a.pick(false, |app, p| app.open(p)));
        let a = app.clone();
        ui.on_paste(move || {
            if let Some(p) = sys::clipboard_files().into_iter().next() {
                a.open(p);
            } else {
                // Say it in the title for a moment.
                let ui = a.ui();
                ui.set_note("Copy a file first".into());
                let w = ui.as_weak();
                slint::Timer::single_shot(Duration::from_millis(1800), move || {
                    if let Some(ui) = w.upgrade() {
                        ui.set_note(SharedString::new());
                    }
                });
            }
        });
        let a = app.clone();
        ui.on_compress(move || a.compress());
        let a = app.clone();
        ui.on_cancel(move || a.cancel.store(true, Ordering::Relaxed));
        let a = app.clone();
        ui.on_remove_file(move || a.another());
        let a = app.clone();
        ui.on_another(move || a.another());
        let a = app.clone();
        ui.on_show_in_folder(move || {
            if let Some(p) = a.result.borrow().as_ref() {
                sys::show_in_folder(p);
            }
        });
        let a = app.clone();
        ui.on_select_preset(move |i| {
            let list = targets(a.settings.borrow().custom_bytes);
            if let Some(t) = list.get(i as usize) {
                a.settings.borrow_mut().preset = t.id.clone();
                a.save();
                a.sync_settings();
                // Re-check the file against the new limit.
                let path = a.current.borrow().as_ref().map(|c| c.path.clone());
                if let (Some(p), View::Big) = (path, a.ui().get_view()) {
                    a.open(p);
                }
            }
        });
        let a = app.clone();
        ui.on_open_settings(move || {
            if a.ui().get_view() != View::Busy {
                a.set_view(View::Settings);
            }
        });
        let a = app.clone();
        ui.on_close_settings(move || a.close_settings());
        let a = app.clone();
        ui.on_escape(move || {
            if a.ui().get_view() == View::Settings {
                a.close_settings();
            } else {
                a.hide();
            }
        });
        let a = app.clone();
        ui.on_set_theme(move |i| {
            a.settings.borrow_mut().theme =
                ["system", "light", "dark"][i.clamp(0, 2) as usize].into();
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        ui.on_set_output(move |i| {
            let i = i.clamp(0, 3) as usize;
            if i == 2 && a.settings.borrow().output_dir.is_empty() {
                a.pick(true, |app, dir| {
                    let mut s = app.settings.borrow_mut();
                    s.output_dir = dir.display().to_string();
                    s.output = "custom".into();
                    drop(s);
                    app.save();
                    app.sync_settings();
                });
                return;
            }
            a.settings.borrow_mut().output = ["app", "beside", "custom", "replace"][i].into();
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        ui.on_choose_folder(move || {
            a.pick(true, |app, dir| {
                let mut s = app.settings.borrow_mut();
                s.output_dir = dir.display().to_string();
                s.output = "custom".into();
                drop(s);
                app.save();
                app.sync_settings();
            })
        });
        ui.on_open_outputs(|| {
            if let Ok(d) = output::app_dir() {
                sys::open_folder(&d);
            }
        });
        let a = app.clone();
        ui.on_set_gpu(move |on| {
            a.settings.borrow_mut().gpu = on;
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        ui.on_set_auto_hide(move |on| {
            a.settings.borrow_mut().auto_hide = on;
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        ui.on_set_start(move |on| {
            sys::set_start_with_windows(on);
            a.settings.borrow_mut().start_with_windows = sys::starts_with_windows();
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        ui.on_set_custom_mb(move |mb| {
            let mb = (mb as u64).clamp(1, 4096);
            a.settings.borrow_mut().custom_bytes = mb * 1024 * 1024;
            a.save();
            a.sync_settings();
        });
    }

    // Progress text ("About 1 min left") follows the progress value.
    {
        let a = app.clone();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(500),
            move || {
                let ui = a.ui();
                if ui.get_view() == View::Busy {
                    if let Some(t) = a.started.get() {
                        let text =
                            format::time_left(t.elapsed().as_secs_f64(), ui.get_progress() as f64);
                        ui.set_busy_left(text.into());
                    }
                }
            },
        );
        // A second launch (from the Start menu) opens the window.
        let w = ui.as_weak();
        let a2 = Rc::downgrade(&app);
        let _keep = (timer, w);
        sys::listen_for_show(move || {
            let _ = slint::invoke_from_event_loop(|| SHOW.with(|f| (f.borrow())()));
        });
        SHOW.with(|f| {
            *f.borrow_mut() = Box::new(move || {
                if let Some(a) = a2.upgrade() {
                    a.show();
                }
            })
        });

        #[cfg(debug_assertions)]
        if let Ok(v) = std::env::var("KOMPACTER_DEMO") {
            demo(&app, &v);
        }
        if !quiet {
            app.show();
        }
        slint::run_event_loop_until_quit()?;
        drop(_keep);
    }
    drop(tray);
    Ok(())
}

thread_local! {
    /// What a second launch does: open the window (set once in main).
    static SHOW: RefCell<Box<dyn Fn()>> = RefCell::new(Box::new(|| {}));
}

/// Dev builds only: `KOMPACTER_DEMO=big|busy|done|error|settings|drag` shows a screen
/// with sample data, for screenshots.
#[cfg(debug_assertions)]
fn demo(app: &App, which: &str) {
    let ui = app.ui();
    ui.set_file_name("Valorant clutch 4K.mp4".into());
    ui.set_file_meta("1:42 · 2560×1440 · 128 MB".into());
    ui.set_file_kind("video".into());
    ui.set_meter_fill(0.92);
    ui.set_meter_limit(0.15);
    ui.set_over_text("108 MB too big".into());
    ui.set_progress(0.42);
    ui.set_busy_left("About 40 s left".into());
    ui.set_done_ok(true);
    ui.set_done_title("Copied".into());
    ui.set_done_tip("19.4 MB · 720p · 6.6× smaller".into());
    ui.set_compress_label("Compress to 720p".into());
    ui.set_error_text("ffprobe couldn't read this file".into());
    match which {
        "big" => ui.set_view(View::Big),
        "busy" => ui.set_view(View::Busy),
        "done" => ui.set_view(View::Done),
        "error" => ui.set_view(View::Error),
        "settings" => ui.set_view(View::Settings),
        "drag" => ui.set_dragging(true),
        "dark" => ui.global::<Theme>().set_dark(true),
        _ => {}
    }
}
