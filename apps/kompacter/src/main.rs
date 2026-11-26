// No console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Kompacter: a tray app that shrinks any file to fit an upload limit and puts the result
//! on the clipboard. The window is drawn by Slint (ui/*.slint); the logic lives here.
//! Heavy work (probing, compressing, filmstrips) runs on worker threads; results come
//! back to the UI thread with `slint::invoke_from_event_loop`. Nothing runs while the
//! window is hidden.

mod flow;
mod format;
mod motion;
mod news;
mod output;
mod quick;
mod settings;
mod sys;
mod trim;
mod trim_cache;
mod update;
mod watch;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kompacter_core::size::PRESETS;
use kompacter_core::{
    ArchiveFormat, AudioFormat, Engine, Estimate, ImageFormat, MediaInfo, MediaKind, Mode, Options,
    VideoCodec,
};
use slint::winit_030::{winit, EventResult, WinitWindowAccessor};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::format::bytes;
use crate::settings::Settings;

slint::include_modules!();

/// A preset as shown: the engine's presets plus "Custom".
pub struct Target {
    pub id: String,
    pub name: String,
    pub brand: String,
    pub bytes: u64,
}

pub fn targets(custom_bytes: u64) -> Vec<Target> {
    let mut list: Vec<Target> = PRESETS
        .iter()
        .map(|p| {
            let name = match p.id {
                "discord" => "Discord",
                "discord-nitro-basic" => "Nitro Basic",
                "discord-nitro" => "Nitro",
                id if id.starts_with("email") => "Email",
                _ => p.label,
            };
            // The logo shown next to it (see AppLogo in theme.slint).
            let brand = match p.id {
                id if id.starts_with("discord") => "discord",
                id @ ("slack" | "whatsapp" | "telegram" | "signal") => id,
                _ => "mail",
            };
            Target {
                id: p.id.into(),
                name: name.into(),
                brand: brand.into(),
                bytes: p.bytes,
            }
        })
        .collect();
    list.push(Target {
        id: "custom".into(),
        name: "Custom".into(),
        brand: "custom".into(),
        bytes: custom_bytes,
    });
    list
}

/// True when `model` already holds exactly `rows`.
pub fn same_rows<T: Clone + PartialEq + 'static>(model: &ModelRc<T>, rows: &[T]) -> bool {
    model.row_count() == rows.len()
        && rows
            .iter()
            .enumerate()
            .all(|(i, r)| model.row_data(i).as_ref() == Some(r))
}

/// The file currently in the window.
pub struct Current {
    pub path: PathBuf,
    pub info: MediaInfo,
    pub estimate: Estimate,
    pub folder_rows: Vec<kompacter_core::TreeRow>,
}

pub struct App {
    pub ui: slint::Weak<AppWindow>,
    pub engine: Arc<Engine>,
    pub cancel: Arc<AtomicBool>,
    pub settings: RefCell<Settings>,
    pub current: RefCell<Option<Current>>,
    pub result: RefCell<Option<PathBuf>>,
    /// The trim range of the last compress (so "Split into parts" can reuse it).
    pub last_range: Cell<Option<(f64, f64)>>,
    /// Several files at once: each is compressed to the limit on its own.
    pub batch: RefCell<Vec<PathBuf>>,
    /// Files dropped together arrive one event each; they're collected briefly.
    pub dropped: RefCell<Vec<PathBuf>>,
    pub started: Cell<Option<Instant>>,
    /// A file dialog is open: don't auto-hide while the user picks.
    pub modal: Cell<bool>,
    /// When the window last hid itself on focus loss (a tray click causes that too).
    pub auto_hidden: Cell<Option<Instant>>,
    /// All animations (off in Low-memory mode or when Windows animations are off).
    pub rich: bool,
    /// Floating see-through card; otherwise a plain window.
    pub glass: bool,
    pub motion: motion::Motion,
    pub trim: trim::Trim,
    pub tray: slint::Weak<Tray>,
    /// The quick bar is showing (see quick.rs); `quick_real` is the flow's screen.
    pub quick: Cell<bool>,
    pub quick_real: Cell<View>,
    /// Paste guard: the chat window whose Ctrl+V was held back.
    pub guard: Cell<Option<isize>>,
    /// Paste guard: which chat app that was (its preset id); its limit is the target
    /// while the guard's bar is open.
    pub guard_app: Cell<Option<&'static str>>,
    /// The shortcut currently registered (so it's only re-registered when it changes).
    pub shortcut_set: RefCell<String>,
    pub watch: watch::Watch,
    /// New clips waiting to shrink on their own, one at a time.
    pub auto_queue: RefCell<std::collections::VecDeque<PathBuf>>,
    pub auto_busy: Cell<bool>,
    /// "Install updates" (read by the background check).
    pub update_on: Arc<AtomicBool>,
    /// A downloaded, checked update; installed when Kompacter quits.
    pub update_ready: RefCell<Option<update::Ready>>,
}

impl App {
    fn new(
        ui: &AppWindow,
        tray: slint::Weak<Tray>,
        prefs: Settings,
        rich: bool,
        glass: bool,
    ) -> Rc<Self> {
        let prefs_auto_update = prefs.auto_update;
        Rc::new(Self {
            ui: ui.as_weak(),
            engine: Arc::new(Engine::new(sys::ffmpeg())),
            cancel: Arc::new(AtomicBool::new(false)),
            settings: RefCell::new(prefs),
            current: RefCell::new(None),
            result: RefCell::new(None),
            last_range: Cell::new(None),
            batch: RefCell::new(Vec::new()),
            dropped: RefCell::new(Vec::new()),
            started: Cell::new(None),
            modal: Cell::new(false),
            auto_hidden: Cell::new(None),
            rich,
            glass,
            motion: motion::Motion::default(),
            trim: trim::Trim::default(),
            tray,
            quick: Cell::new(false),
            quick_real: Cell::new(View::Idle),
            guard: Cell::new(None),
            guard_app: Cell::new(None),
            shortcut_set: RefCell::new(String::from("unset")),
            watch: watch::Watch::new(|e| with_app(move |a| a.on_watch(e))),
            auto_queue: RefCell::new(Default::default()),
            auto_busy: Cell::new(false),
            update_on: Arc::new(AtomicBool::new(prefs_auto_update)),
            update_ready: RefCell::new(None),
        })
    }

    pub fn ui(&self) -> AppWindow {
        self.ui.upgrade().expect("window")
    }

    /// The limit to fit: the chat app's while the paste guard's bar is open, else the
    /// user's target.
    pub fn target_bytes(&self) -> u64 {
        match self.guard_app.get() {
            Some(app) => self.chat_limit(app),
            None => self.user_target_bytes(),
        }
    }

    /// A chat app's limit. Discord follows the user's Discord tier if they picked one.
    pub fn chat_limit(&self, app: &str) -> u64 {
        let preset = self.settings.borrow().preset.clone();
        let id = if app == "discord" && preset.starts_with("discord") {
            preset.as_str()
        } else {
            app
        };
        PRESETS
            .iter()
            .find(|p| p.id == id)
            .map_or_else(|| self.user_target_bytes(), |p| p.bytes)
    }

    /// The target picked in the window (or Settings).
    pub fn user_target_bytes(&self) -> u64 {
        let s = self.settings.borrow();
        if s.preset == "custom" {
            s.custom_bytes
        } else {
            PRESETS
                .iter()
                .find(|p| p.id == s.preset)
                .unwrap_or(&PRESETS[0])
                .bytes
        }
    }

    /// Options for `target` with the user's formats, for compressing and for estimates
    /// (the codec changes what quality a size buys).
    pub fn options(&self, target: u64) -> Options {
        let s = self.settings.borrow();
        let mut o = Options::new(target);
        o.video_codec = s.video_codec;
        o.audio_format = s.audio_format;
        o.image_format = s.image_format;
        o.archive = s.archive_format;
        o.gpu = s.gpu;
        o
    }

    pub fn save(&self) {
        let _ = settings::save(&self.settings.borrow());
    }

    /// Push settings and presets into the window.
    pub fn sync_settings(&self) {
        let ui = self.ui();
        let prefs = ui.global::<Prefs>();
        let s = self.settings.borrow();
        let list = targets(s.custom_bytes);
        let items: Vec<PresetItem> = list
            .iter()
            .map(|t| PresetItem {
                name: t.name.as_str().into(),
                note: SharedString::new(),
                size: bytes(t.bytes).into(),
                brand: t.brand.as_str().into(),
            })
            .collect();
        let index = list.iter().position(|t| t.id == s.preset).unwrap_or(0);
        // Update rows in place: replacing the list would rebuild every row, dropping a
        // held button or the cursor in the Custom box.
        let model = ui.get_presets();
        if model.row_count() == items.len() {
            for (i, item) in items.into_iter().enumerate() {
                if model.row_data(i).as_ref() != Some(&item) {
                    model.set_row_data(i, item);
                }
            }
        } else {
            ui.set_presets(ModelRc::new(VecModel::from(items)));
        }
        ui.set_preset_index(index as i32);
        ui.set_target_text(bytes(list[index].bytes).into());

        let discord = s.style == "discord";
        prefs.set_style_index(discord as i32);
        let (options, index): (&[&str], i32) = if discord {
            (
                &["Match", "Light", "Ash", "Dark", "Onyx"],
                match s.discord_theme.as_str() {
                    "light" => 1,
                    "ash" => 2,
                    "dark" => 3,
                    "onyx" => 4,
                    _ => 0,
                },
            )
        } else {
            (
                &["System", "Light", "Dark"],
                match s.theme.as_str() {
                    "light" => 1,
                    "dark" => 2,
                    _ => 0,
                },
            )
        };
        let options: Vec<SharedString> = options.iter().map(|o| (*o).into()).collect();
        // Only when it changes (switching style): a new list rebuilds the whole control.
        if !same_rows(&prefs.get_theme_options(), &options) {
            prefs.set_theme_options(ModelRc::new(VecModel::from(options)));
        }
        prefs.set_theme_index(index);
        prefs.set_output_index(match s.output.as_str() {
            "beside" => 1,
            "custom" => 2,
            "replace" => 3,
            _ => 0,
        });
        prefs.set_output_dir(s.output_dir.as_str().into());
        prefs.set_gpu(s.gpu);
        prefs.set_auto_update(s.auto_update);
        prefs.set_auto_hide(s.auto_hide);
        prefs.set_start_with_windows(s.start_with_windows);
        prefs.set_explorer_menu(s.explorer_menu);
        prefs.set_video_index(match s.video_codec {
            VideoCodec::H264 => 0,
            VideoCodec::H265 => 1,
            VideoCodec::Av1 => 2,
        });
        prefs.set_audio_index(match s.audio_format {
            AudioFormat::Auto => 0,
            AudioFormat::Mp3 => 1,
            AudioFormat::Opus => 2,
        });
        prefs.set_image_index(match s.image_format {
            ImageFormat::Auto => 0,
            ImageFormat::Jpeg => 1,
            ImageFormat::Webp => 2,
        });
        prefs.set_archive_index((s.archive_format == ArchiveFormat::SevenZ) as i32);
        prefs.set_custom_mb((s.custom_bytes / (1024 * 1024)) as i32);
        drop(s);
        self.sync_automatic();
        self.apply_theme();
    }

    pub fn apply_theme(&self) {
        let s = self.settings.borrow();
        let system_dark = sys::system_dark();
        let ui = self.ui();
        let theme = ui.global::<Theme>();
        theme.set_dark(match s.theme.as_str() {
            "light" => false,
            "dark" => true,
            _ => system_dark,
        });
        theme.set_style((s.style == "discord") as i32);
        theme.set_dt(match s.discord_theme.as_str() {
            "light" => 0,
            "ash" => 1,
            "dark" => 2,
            "onyx" => 3,
            _ if system_dark => 2,
            _ => 0,
        });
    }

    /// Show the first-run screen: three steps, one choice each. "Copied files" starts
    /// ticked: it's what makes Kompacter turn up by itself when it's needed, and it only
    /// looks at the size of copied files. The other two keep whatever is already set (off
    /// on a new install).
    fn welcome(&self) {
        let ui = self.ui();
        let s = self.settings.borrow();
        ui.set_welcome_copies(true);
        ui.set_welcome_guard(s.paste_guard);
        ui.set_welcome_clips(s.watch_folders);
        ui.set_welcome_step(0);
        drop(s);
        self.set_view(View::Welcome);
    }

    /// After an update: "What's new" for `version`, once.
    fn whats_new(&self, version: &str, lines: &[&str]) {
        let ui = self.ui();
        ui.set_news_version(version.into());
        let items: Vec<SharedString> = lines.iter().map(|l| (*l).into()).collect();
        ui.set_news_items(ModelRc::new(VecModel::from(items)));
        self.set_view(View::News);
    }

    /// Check GitHub for a newer version now and then, in the background (release builds
    /// on Windows only). A checked download shows the "Update ready" line.
    fn start_update_checks(&self, current: &'static str) {
        let on = self.update_on.clone();
        std::thread::spawn(move || {
            // The installer of the update that just ran (or an old one) isn't needed.
            update::clear_downloads();
            // Let startup (and Start with Windows) settle first.
            std::thread::sleep(Duration::from_secs(90));
            loop {
                if on.load(Ordering::Relaxed) {
                    // The pill shows the download filling a ring (only whole-percent steps
                    // reach the window).
                    let shown = std::sync::atomic::AtomicI32::new(-1);
                    let progress = |version: &str, p: f32| {
                        let pct = (p * 100.0) as i32;
                        if shown.swap(pct, Ordering::Relaxed) != pct {
                            let version = version.to_string();
                            with_app(move |a| {
                                let ui = a.ui();
                                ui.set_update_version(version.into());
                                ui.set_update_progress(pct as f32 / 100.0);
                            });
                        }
                    };
                    match update::check(current, &update::download_dir(), &progress) {
                        Ok(Some(ready)) => {
                            with_app(move |a| {
                                let ui = a.ui();
                                ui.set_update_version(ready.version.as_str().into());
                                ui.set_update_progress(1.0);
                                *a.update_ready.borrow_mut() = Some(ready);
                            });
                            return;
                        }
                        // A failed download: hide the pill until the next try.
                        Err(_) if shown.load(Ordering::Relaxed) >= 0 => {
                            with_app(|a| a.ui().set_update_version(SharedString::new()));
                        }
                        _ => {}
                    }
                }
                std::thread::sleep(Duration::from_secs(24 * 60 * 60));
            }
        });
    }

    /// "Done" on the first-run screen: save the three choices and never ask again.
    fn welcome_done(self: &Rc<Self>) {
        let ui = self.ui();
        {
            let mut s = self.settings.borrow_mut();
            s.welcomed = true;
            s.watch_copies = ui.get_welcome_copies();
            s.paste_guard = ui.get_welcome_guard();
        }
        self.save();
        let clips = ui.get_welcome_clips();
        if clips != self.settings.borrow().watch_folders {
            // Fills in the clip folders this PC has, saves and syncs.
            self.set_watch_folders(clips);
        }
        self.sync_settings();
        self.set_view(View::Idle);
    }

    /// The tray tip was answered: never show it again, and say `note` for a moment.
    fn end_tray_tip(&self, note: &str) {
        self.settings.borrow_mut().tray_tip_seen = true;
        self.save();
        let ui = self.ui();
        ui.set_tray_tip(false);
        if !note.is_empty() {
            ui.set_note(note.into());
            let w = ui.as_weak();
            slint::Timer::single_shot(Duration::from_millis(2600), move || {
                if let Some(ui) = w.upgrade() {
                    ui.set_note(SharedString::new());
                }
            });
        }
    }

    fn file_hover(&self, active: bool) {
        let ui = self.ui();
        ui.set_dragging(active && self.view() != View::Busy);
    }

    pub fn another(&self) {
        *self.current.borrow_mut() = None;
        self.batch.borrow_mut().clear();
        self.guard.set(None);
        self.guard_app.set(None);
        self.ui().set_guard(false);
        self.set_view(View::Idle);
    }

    /// Native picker for one or more files, on a worker thread.
    pub fn pick_files(self: &Rc<Self>) {
        self.modal.set(true);
        let weak = Rc::downgrade(self);
        run(
            || rfd::FileDialog::new().pick_files().unwrap_or_default(),
            move |picked| {
                let Some(app) = weak.upgrade() else { return };
                app.modal.set(false);
                app.show();
                app.open_many(picked);
            },
        );
    }

    /// Native file or folder picker on a worker thread, so the window keeps drawing.
    pub fn pick(self: &Rc<Self>, folder: bool, then: impl FnOnce(&Rc<App>, PathBuf) + 'static) {
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

pub fn kind_name(k: MediaKind) -> &'static str {
    match k {
        MediaKind::Video => "video",
        MediaKind::Audio => "audio",
        MediaKind::Image => "image",
        MediaKind::Folder => "folder",
        MediaKind::Other => "file",
    }
}

/// Run `work` on a worker thread, then `done` with its result on the UI thread.
pub fn run<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + 'static,
) {
    let done = UiOnly(Some(done), std::thread::current().id());
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

thread_local! {
    /// What a second launch does: open the window (set once in main).
    static SHOW: RefCell<Box<dyn Fn()>> = RefCell::new(Box::new(|| {}));
    /// The app, for events from background threads (shortcut, guard, watcher).
    static APP: RefCell<std::rc::Weak<App>> = const { RefCell::new(std::rc::Weak::new()) };
}

/// Run `f` with the app on the UI thread (from any thread).
fn with_app(f: impl FnOnce(&Rc<App>) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(app) = APP.with(|a| a.borrow().upgrade()) {
            f(&app);
        }
    });
}

fn main() -> Result<(), slint::PlatformError> {
    // Files this launch was given ("Kompact" in Explorer, "Open with", a file dropped on
    // the shortcut). They're left where the running Kompacter looks; if that turns out
    // to be this one, it picks them up itself further down.
    sys::leave_handoff(&sys::file_args(std::env::args()));
    if !sys::first_instance() {
        return Ok(());
    }
    let quiet = std::env::args().any(|a| a == "--tray");

    let prefs = settings::load();
    let backend = slint::BackendSelector::new().backend_name("winit".into());
    #[cfg(windows)]
    let backend = backend.renderer_name("skia-software".into());
    #[cfg(not(windows))]
    let backend = backend.renderer_name("femtovg".into());
    backend
        .with_winit_window_attributes_hook(|attrs| {
            #[cfg(windows)]
            {
                use winit::platform::windows::{CornerPreference, WindowAttributesExtWindows};
                attrs
                    .with_skip_taskbar(true)
                    .with_undecorated_shadow(false)
                    .with_corner_preference(CornerPreference::DoNotRound)
            }
            #[cfg(not(windows))]
            attrs
        })
        .select()?;

    let ui = AppWindow::new()?;
    // Skia raster supports the translucent card. Installing a rendering notifier
    // would disable its partial redraws, even if the callback does nothing.
    #[cfg(windows)]
    let glass = true;
    #[cfg(not(windows))]
    let glass = ui.window().set_rendering_notifier(|_, _| {}).is_ok();
    let rich = glass && sys::animations_enabled();
    ui.global::<Fx>().set_glass(glass);
    ui.global::<Fx>().set_rich(rich);
    // Starts folded, so the first open grows in.
    ui.set_opened(false);

    let tray = Tray::new()?;
    let app = App::new(&ui, tray.as_weak(), prefs, rich, glass);
    APP.with(|a| *a.borrow_mut() = Rc::downgrade(&app));
    sys::start_input(|e| with_app(move |a| a.on_input(e)));
    // A tray app is only useful if it's running, so it starts with Windows unless the
    // user says otherwise: switched on once here, and theirs to switch off from then on.
    // (Never from a dev build, which would register its target\debug exe.)
    if !cfg!(debug_assertions) {
        let first = !app.settings.borrow().start_default_done;
        if first || sys::starts_with_windows() {
            // Also keeps the entry pointing at this exe if the app was moved.
            sys::set_start_with_windows(true);
        }
        if first {
            app.settings.borrow_mut().start_default_done = true;
            app.save();
        }
    }
    // Keeps the right-click entry pointing at this exe (not from a dev build either).
    if !cfg!(debug_assertions) && app.settings.borrow().explorer_menu {
        sys::set_explorer_menu(true);
    }
    // The Run entry is the truth (it can be turned off in Task Manager).
    app.settings.borrow_mut().start_with_windows = sys::starts_with_windows();
    // Windows tucks new tray icons into the "^" overflow: offer once to keep it in view.
    ui.set_tray_tip(
        cfg!(windows) && !app.settings.borrow().tray_tip_seen && sys::tray_pinned() != Some(true),
    );
    app.sync_settings();
    ui.set_dash_path(format::dashed_round_rect(308.0, 300.0, 28.0, 6.0, 7.0, 0.0).into());

    // Tidy old outputs. GPU encoders are checked when a video first needs one.
    std::thread::spawn(|| output::clean(None));

    // ── Tray ──
    {
        let a = app.clone();
        tray.on_open(move || a.toggle());
        tray.on_open_folder(|| {
            if let Ok(d) = output::app_dir() {
                sys::open_folder(&d);
            }
        });
        let a = app.clone();
        tray.on_quit(move || a.trim_quit());
    }

    {
        let a = app.clone();
        ui.on_motion_tick(move |track_eyes| a.tick_motion(track_eyes));
    }

    // ── Window events: focus (auto-hide), files dragged in, theme changes ──
    {
        let weak = Rc::downgrade(&app);
        ui.window().on_close_requested(move || {
            if let Some(app) = weak.upgrade() {
                app.close();
            }
            slint::CloseRequestResponse::KeepWindowShown
        });
        let a = app.clone();
        ui.window().on_winit_window_event(move |_, event| {
            match event {
                winit::event::WindowEvent::Focused(false) => a.away(),
                winit::event::WindowEvent::HoveredFile(_) => a.file_hover(true),
                winit::event::WindowEvent::HoveredFileCancelled => a.file_hover(false),
                winit::event::WindowEvent::DroppedFile(p) => {
                    a.file_hover(false);
                    // One event per file: collect them for a moment, then open them all.
                    let first = a.dropped.borrow().is_empty();
                    a.dropped.borrow_mut().push(p.clone());
                    if first {
                        let weak = Rc::downgrade(&a);
                        slint::Timer::single_shot(Duration::from_millis(120), move || {
                            if let Some(a) = weak.upgrade() {
                                let files = std::mem::take(&mut *a.dropped.borrow_mut());
                                a.open_dropped(files);
                            }
                        });
                    }
                }
                winit::event::WindowEvent::ThemeChanged(_) => a.apply_theme(),
                _ => {}
            }
            EventResult::Propagate
        });
    }

    // ── Buttons ──
    {
        let prefs = ui.global::<Prefs>();
        let a = app.clone();
        ui.on_choose_file(move || a.pick_files());
        let a = app.clone();
        ui.on_paste(move || {
            let files = sys::clipboard_files();
            if !files.is_empty() {
                a.open_many(files);
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
        ui.on_compress(move || {
            if a.batch.borrow().is_empty() {
                a.compress(None, Mode::Fit, false)
            } else {
                a.compress_batch()
            }
        });
        let a = app.clone();
        ui.on_split(move || {
            let range = a.last_range.get();
            a.compress(range, Mode::Fit, true)
        });
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
                match a.view() {
                    // Re-check the file against the new limit.
                    View::Big => {
                        let path = a.current.borrow().as_ref().map(|c| c.path.clone());
                        if let Some(p) = path {
                            a.open(p);
                        }
                    }
                    View::Trim => a.trim_update(),
                    _ => {}
                }
            }
        });
        let a = app.clone();
        ui.on_open_settings(move || {
            if a.view() != View::Busy {
                // Windows owns this switch (its own Settings page changes it too).
                let ui = a.ui();
                let prefs = ui.global::<Prefs>();
                prefs.set_tray_pinned(sys::tray_pinned().unwrap_or(false));
                prefs.set_tray_note(SharedString::new());
                a.set_view(View::Settings);
            }
        });
        let a = app.clone();
        prefs.on_set_tray_pinned(move |on| {
            let ui = a.ui();
            let prefs = ui.global::<Prefs>();
            if sys::set_tray_pinned(on) {
                prefs.set_tray_note(SharedString::new());
            } else {
                sys::open_taskbar_settings();
                prefs.set_tray_note("Switch Kompacter on in the list that opened".into());
            }
            prefs.set_tray_pinned(sys::tray_pinned().unwrap_or(false));
        });
        let a = app.clone();
        ui.on_tray_tip_pin(move || {
            let direct = sys::set_tray_pinned(true);
            if !direct {
                sys::open_taskbar_settings();
            }
            a.end_tray_tip(if direct {
                "It stays next to the clock"
            } else {
                "Switch Kompacter on in that list"
            });
        });
        let a = app.clone();
        ui.on_tray_tip_dismiss(move || a.end_tray_tip(""));
        let a = app.clone();
        ui.on_close_settings(move || {
            a.close_settings();
            a.trim_activate();
        });
        let a = app.clone();
        ui.on_card_resize_finished(move || a.sync_window_height());
        let a = app.clone();
        ui.on_open_trim(move || a.open_trim());
        let a = app.clone();
        ui.on_close_trim(move || a.set_view(View::Big));
        let a = app.clone();
        ui.on_trim_changed(move || a.trim_update());
        let a = app.clone();
        ui.on_trim_compress(move || {
            a.trim_stop();
            a.trim_compress()
        });
        let a = app.clone();
        ui.on_trim_play(move || a.trim_play());
        let a = app.clone();
        ui.on_trim_sound_changed(move || a.trim_sound_changed());
        let a = app.clone();
        ui.on_trim_seek(move |pos| a.trim_seek(pos as f64));
        let a = app.clone();
        ui.on_trim_drag_start(move || a.trim_drag_start());
        let a = app.clone();
        ui.on_trim_drag_end(move |playhead| a.trim_drag_end(playhead));
        let a = app.clone();
        ui.on_trim_set_time(move |side, text| a.trim_set_time(side, &text));
        let a = app.clone();
        ui.on_trim_nudge(move |dir, big| a.trim_nudge(dir, big));
        let a = app.clone();
        ui.on_escape(move || match a.ui().get_view() {
            View::Settings => {
                a.close_settings();
                a.trim_activate();
            }
            View::Trim => a.set_view(View::Big),
            View::News => a.set_view(View::Idle),
            _ => a.close(),
        });
        let a = app.clone();
        ui.on_news_done(move || a.set_view(View::Idle));
        // Restart: quit, and the installer runs on the way out (see the end of main).
        let a = app.clone();
        ui.on_restart_to_update(move || a.trim_quit());
        let a = app.clone();
        ui.on_quick_expand(move || a.quick_expand());
        let a = app.clone();
        ui.on_quick_trim(move || a.quick_trim());
        let a = app.clone();
        ui.on_paste_anyway(move || a.paste_anyway());
        let a = app.clone();
        prefs.on_set_auto_update(move |on| {
            a.settings.borrow_mut().auto_update = on;
            a.update_on.store(on, Ordering::Relaxed);
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        prefs.on_set_shortcut(move |i| {
            let id = settings::SHORTCUTS[i.clamp(0, 3) as usize].0;
            a.settings.borrow_mut().shortcut = id.into();
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        prefs.on_set_paste_guard(move |on| {
            a.settings.borrow_mut().paste_guard = on;
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        prefs.on_set_watch_copies(move |on| {
            a.settings.borrow_mut().watch_copies = on;
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        prefs.on_set_watch_folders(move |on| a.set_watch_folders(on));
        let a = app.clone();
        prefs.on_add_clip_folder(move || a.add_clip_folder());
        let a = app.clone();
        prefs.on_remove_clip_folder(move |i| a.remove_clip_folder(i.max(0) as usize));
        let a = app.clone();
        prefs.on_set_style(move |i| {
            a.settings.borrow_mut().style = if i == 1 { "discord" } else { "kompacter" }.into();
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        prefs.on_set_theme(move |i| {
            let mut s = a.settings.borrow_mut();
            if s.style == "discord" {
                s.discord_theme =
                    ["match", "light", "ash", "dark", "onyx"][i.clamp(0, 4) as usize].into();
            } else {
                s.theme = ["system", "light", "dark"][i.clamp(0, 2) as usize].into();
            }
            drop(s);
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        prefs.on_set_output(move |i| {
            let i = i.clamp(0, 3) as usize;
            if i == 2 && a.settings.borrow().output_dir.is_empty() {
                a.pick(true, set_custom_dir);
                return;
            }
            a.settings.borrow_mut().output = ["app", "beside", "custom", "replace"][i].into();
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        prefs.on_choose_folder(move || a.pick(true, set_custom_dir));
        prefs.on_open_outputs(|| {
            if let Ok(d) = output::app_dir() {
                sys::open_folder(&d);
            }
        });
        let a = app.clone();
        prefs.on_set_gpu(move |on| {
            a.settings.borrow_mut().gpu = on;
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        prefs.on_set_auto_hide(move |on| {
            a.settings.borrow_mut().auto_hide = on;
            if on {
                a.away();
            } else {
                a.cancel_auto_hide();
            }
            let saved = settings::save(&a.settings.borrow()).is_ok();
            a.ui().global::<Prefs>().set_auto_hide_error(
                if saved {
                    ""
                } else {
                    "Could not save for next launch"
                }
                .into(),
            );
            a.sync_settings();
        });
        let a = app.clone();
        prefs.on_set_start(move |on| {
            sys::set_start_with_windows(on);
            a.settings.borrow_mut().start_with_windows = sys::starts_with_windows();
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        prefs.on_set_explorer_menu(move |on| {
            sys::set_explorer_menu(on);
            a.settings.borrow_mut().explorer_menu = on;
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        ui.on_welcome_done(move || a.welcome_done());
        let a = app.clone();
        prefs.on_set_video(move |i| {
            a.settings.borrow_mut().video_codec =
                [VideoCodec::H264, VideoCodec::H265, VideoCodec::Av1][i.clamp(0, 2) as usize];
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        prefs.on_set_audio(move |i| {
            a.settings.borrow_mut().audio_format =
                [AudioFormat::Auto, AudioFormat::Mp3, AudioFormat::Opus][i.clamp(0, 2) as usize];
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        prefs.on_set_image(move |i| {
            a.settings.borrow_mut().image_format =
                [ImageFormat::Auto, ImageFormat::Jpeg, ImageFormat::Webp][i.clamp(0, 2) as usize];
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        prefs.on_set_archive(move |i| {
            a.settings.borrow_mut().archive_format = if i == 1 {
                ArchiveFormat::SevenZ
            } else {
                ArchiveFormat::Zip
            };
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        prefs.on_set_custom_mb(move |mb| {
            let mb = (mb as u64).clamp(1, 4096);
            a.settings.borrow_mut().custom_bytes = mb * 1024 * 1024;
            a.save();
            a.sync_settings();
        });
    }

    // A second launch (from the Start menu) opens the window.
    {
        let weak = Rc::downgrade(&app);
        SHOW.with(|f| {
            *f.borrow_mut() = Box::new(move || {
                // Explorer starts one launch per selected file: give them a moment to
                // all arrive, then open them together.
                let weak = weak.clone();
                slint::Timer::single_shot(Duration::from_millis(150), move || {
                    let Some(a) = weak.upgrade() else { return };
                    let files = sys::take_handoff();
                    if !files.is_empty() && a.view() != View::Busy {
                        // The full window, whatever was showing (quick bar included).
                        a.another();
                        a.open_many(files);
                    }
                    a.show();
                });
            })
        });
        sys::listen_for_show(|| {
            let _ = slint::invoke_from_event_loop(|| SHOW.with(|f| (f.borrow())()));
        });
    }

    #[cfg(debug_assertions)]
    if let Ok(v) = std::env::var("KOMPACTER_DEMO") {
        demo(&app, &v);
    }
    // Dev builds only: open this file at startup (for testing on a virtual display).
    #[cfg(debug_assertions)]
    if let Ok(p) = std::env::var("KOMPACTER_OPEN") {
        app.open(PathBuf::from(p));
    }
    // Dev builds only: `KOMPACTER_DROP=<file>@<seconds>` acts out a drop at that time
    // (drag-over state, then the file opens), for recording the README demos.
    #[cfg(debug_assertions)]
    if let Some((p, at)) = std::env::var("KOMPACTER_DROP")
        .ok()
        .and_then(|v| {
            v.rsplit_once('@')
                .map(|(p, t)| (p.to_string(), t.to_string()))
        })
        .and_then(|(p, t)| Some((PathBuf::from(p), t.parse::<f64>().ok()?)))
    {
        let hover = Duration::from_secs_f64((at - 0.7).max(0.0));
        let weak = Rc::downgrade(&app);
        slint::Timer::single_shot(hover, move || {
            if let Some(a) = weak.upgrade() {
                a.ui().set_dragging(true);
            }
        });
        let weak = Rc::downgrade(&app);
        slint::Timer::single_shot(Duration::from_secs_f64(at.max(0.0)), move || {
            if let Some(a) = weak.upgrade() {
                a.ui().set_dragging(false);
                a.open_many(vec![p]);
            }
        });
    }
    // Dev builds only: the quick bar (or the paste guard's) for this file.
    #[cfg(debug_assertions)]
    if let Ok(p) = std::env::var("KOMPACTER_QUICK") {
        let guard = std::env::var("KOMPACTER_GUARD").is_ok().then_some(0);
        if guard.is_some() {
            app.guard_app.set(Some(demo_guard_app()));
        }
        app.quick_open(vec![PathBuf::from(p)], guard);
    }
    // A new version's first start: "What's new", once. A fresh install gets the first-run
    // screen instead, so it just remembers the version.
    if let Some(version) = update::VERSION {
        let (seen, welcomed) = {
            let s = app.settings.borrow();
            (s.seen_version.clone(), s.welcomed)
        };
        if seen != version {
            if welcomed && app.view() == View::Idle && !app.quick.get() {
                if let Some(lines) = news::for_version(version) {
                    app.whats_new(version, lines);
                }
            }
            app.settings.borrow_mut().seen_version = version.into();
            app.save();
        }
        if cfg!(windows) {
            app.start_update_checks(version);
        }
    }
    // First run: ask once what Kompacter may do on its own (see `welcome_done`).
    if !app.settings.borrow().welcomed && app.view() == View::Idle && !app.quick.get() {
        app.welcome();
    }
    // Started with files: open them (and the window, even on a quiet start).
    let given = sys::take_handoff();
    let quiet = quiet && given.is_empty();
    if !given.is_empty() && app.view() != View::Busy {
        app.another();
        app.open_many(given);
    }
    if !quiet && !app.quick.get() {
        app.show();
    }
    slint::run_event_loop_until_quit()?;
    drop(tray);
    // A checked update waits for Kompacter to close; the installer reopens it.
    if app.settings.borrow().auto_update {
        if let Some(ready) = app.update_ready.borrow().as_ref() {
            let _ = update::install(ready);
        }
    }
    Ok(())
}

fn set_custom_dir(app: &Rc<App>, dir: PathBuf) {
    let mut s = app.settings.borrow_mut();
    s.output_dir = dir.display().to_string();
    s.output = "custom".into();
    drop(s);
    app.save();
    app.sync_settings();
}

/// Dev builds only: the chat app a demo paste guard is for,
/// `KOMPACTER_GUARD_APP=slack|whatsapp|telegram|signal` (default Discord).
#[cfg(debug_assertions)]
fn demo_guard_app() -> &'static str {
    match std::env::var("KOMPACTER_GUARD_APP").as_deref() {
        Ok("slack") => "slack",
        Ok("whatsapp") => "whatsapp",
        Ok("telegram") => "telegram",
        Ok("signal") => "signal",
        _ => "discord",
    }
}

/// Dev builds only: `KOMPACTER_DEMO=big|split|trim|busy|done|toobig|parts|error|settings|drag|discord|welcome(-2|-3)|news|update`
/// shows a screen with sample data, for screenshots.
#[cfg(debug_assertions)]
fn demo(app: &Rc<App>, which: &str) {
    let ui = app.ui();
    ui.set_file_name("Valorant clutch 4K.mp4".into());
    ui.set_file_meta("1:42 · 2560×1440 · 128 MB".into());
    ui.set_file_kind("video".into());
    ui.set_meter_fill(0.92);
    ui.set_meter_limit(0.15);
    ui.set_over_text("108 MB too big".into());
    ui.set_sum_before("128 MB".into());
    ui.set_sum_after("≤ 20 MB".into());
    ui.set_compress_label("Compress to 720p".into());
    ui.set_can_trim(true);
    ui.set_progress(0.42);
    ui.set_squeeze(0.42);
    ui.set_busy_left("About 40 s left".into());
    ui.set_done_ok(true);
    ui.set_done_title("Copied".into());
    ui.set_done_sub("19.4 MB · 720p · 6.6× smaller".into());
    ui.set_error_text("ffprobe couldn't read this file".into());
    ui.set_trim_dur(102.0);
    ui.set_trim_a(12.0);
    ui.set_trim_b(40.0);
    ui.set_trim_range("0:12 – 0:40 · 28 s".into());
    ui.set_trim_big("≈ 14.2 MB".into());
    ui.set_trim_status("Fits 20 MB".into());
    ui.set_trim_grade(2);
    ui.set_trim_big("720p".into());
    ui.set_trim_details(ModelRc::new(VecModel::from(vec![
        DetailRow {
            label: "Resolution".into(),
            value: "2560×1440 → 1280×720".into(),
        },
        DetailRow {
            label: "Frame rate".into(),
            value: "60 → 30 fps".into(),
        },
        DetailRow {
            label: "Bitrate".into(),
            value: "1.9 Mbps video · 96 kbps audio".into(),
        },
    ])));
    match which {
        "big" => ui.set_view(View::Big),
        "details" | "folder" => {
            let r = |depth: i32, icon: &str, label: &str, before: &str, after: &str| DiffRow {
                depth,
                icon: icon.into(),
                label: label.into(),
                before: before.into(),
                after: after.into(),
                changed: before != after,
            };
            let rows = if which == "folder" {
                ui.set_file_name("Memes".into());
                ui.set_file_meta("84.1 MB".into());
                ui.set_file_kind("folder".into());
                vec![
                    r(0, "dir", "Memes", "84.1 MB", "19.6 MB"),
                    r(1, "dir", "clips", "61.2 MB", "13.9 MB"),
                    r(2, "file", "funny cat.mp4", "40.8 MB", "9.3 MB"),
                    r(2, "file", "fail.mp4", "20.4 MB", "4.6 MB"),
                    r(1, "dir", "pics", "22.6 MB", "5.4 MB"),
                    r(2, "file", "dog.png", "12.1 MB", "2.8 MB"),
                    r(2, "file", "meme.jpg", "10.5 MB", "2.6 MB"),
                    r(1, "file", "readme.txt", "3 KB", "3 KB"),
                ]
            } else {
                vec![
                    r(0, "", "Resolution", "2560×1440", "1280×720"),
                    r(0, "", "Frame rate", "60 fps", "30 fps"),
                    r(0, "", "Codec", "H.264", "AV1"),
                    r(0, "", "Bitrate", "10 Mbps", "1.6 Mbps"),
                    r(0, "", "Size", "128 MB", "≤ 20 MB"),
                ]
            };
            ui.set_diff(ModelRc::new(VecModel::from(rows)));
            ui.set_diff_open(true);
            ui.set_compress_label("Compress".into());
            ui.set_view(View::Big);
        }
        "split" => {
            ui.set_split_count(3);
            ui.set_view(View::Big);
        }
        "toobig" => {
            ui.set_done_ok(false);
            ui.set_done_split(true);
            ui.set_done_title("Still too big".into());
            ui.set_done_sub("48 MB · too big for one file".into());
            ui.set_view(View::Done);
        }
        "parts" => {
            ui.set_done_title("Copied 3 parts".into());
            ui.set_done_sub("3 files · each under 20 MB".into());
            ui.set_view(View::Done);
        }
        "trim" => {
            ui.set_trim_a_text("0:12.000".into());
            ui.set_trim_b_text("0:40.000".into());
            ui.set_trim_pos(20.0);
            ui.set_trim_range("28.0 s".into());
            ui.set_trim_time("0:12.000".into());
            ui.set_win_h(624.0);
            ui.set_view(View::Trim);
        }
        "busy" => ui.set_view(View::Busy),
        "done" => ui.set_view(View::Done),
        "error" => ui.set_view(View::Error),
        "settings" => {
            ui.set_win_h(600.0);
            ui.set_view(View::Settings);
        }
        "drag" => ui.set_dragging(true),
        "news" => app.whats_new(news::NOTES[0].0, news::NOTES[0].1),
        "update" => ui.set_update_version("0.0.8".into()),
        "update-downloading" => {
            ui.set_update_version("0.0.8".into());
            ui.set_update_progress(0.42);
        }
        "tip" => ui.set_tray_tip(true),
        // `welcome-2` and `welcome-3` open on that step.
        "welcome" | "welcome-1" | "welcome-2" | "welcome-3" => {
            app.welcome();
            let step = which
                .strip_prefix("welcome-")
                .and_then(|n| n.parse::<i32>().ok());
            ui.set_welcome_step(step.map_or(0, |n| n - 1));
        }
        "quick" | "quick-busy" | "quick-done" | "guard" | "quick-empty" => {
            app.quick.set(true);
            ui.set_guard(which == "guard");
            app.guard.set((which == "guard").then_some(1));
            ui.set_sum_after("≤ 20 MB".into());
            if which == "guard" {
                let id = demo_guard_app();
                app.guard_app.set(Some(id));
                ui.set_guard_brand(id.into());
                let limit = bytes(app.chat_limit(id));
                ui.set_sum_after(format!("≤ {limit}").into());
            }
            let real = match which {
                "quick-busy" => View::Busy,
                "quick-done" => View::Done,
                "quick-empty" => View::Quick,
                _ => View::Big,
            };
            if which == "quick-empty" {
                ui.set_quick_state(5);
                app.quick_real.set(View::Idle);
            }
            if which == "quick-done" {
                ui.set_done_sub("19.4 MB · 6.6× smaller".into());
            }
            ui.set_busy_left("About 40 s left".into());
            app.set_view(real);
            // The quick bar opens itself (normally the shortcut or the guard does).
            app.show();
        }
        "trim-gif" => {
            ui.set_trim_a_text("0:12.000".into());
            ui.set_trim_b_text("0:20.000".into());
            ui.set_trim_b(20.0);
            ui.set_trim_pos(15.0);
            ui.set_trim_range("8.0 s".into());
            ui.set_trim_time("0:15.000".into());
            ui.set_trim_mode(2);
            ui.set_trim_big("GIF".into());
            ui.set_trim_grade(0);
            ui.set_trim_status("About 480 px wide · 15 fps".into());
            ui.set_trim_button("Make GIF".into());
            ui.set_win_h(624.0);
            ui.set_view(View::Trim);
        }
        "auto" => {
            let mut s = app.settings.borrow_mut();
            s.paste_guard = true;
            s.watch_folders = true;
            s.clip_folders = vec![
                "C:\\Users\\LS\\Videos\\Medal".into(),
                "C:\\Users\\LS\\Videos\\Captures".into(),
            ];
            drop(s);
            app.sync_settings();
            ui.set_win_h(600.0);
            ui.set_view(View::Settings);
        }
        "discord" => {
            app.settings.borrow_mut().style = "discord".into();
            app.settings.borrow_mut().discord_theme = "ash".into();
            app.sync_settings();
        }
        _ => {}
    }
}

#[cfg(test)]
mod drag_tests {
    use super::*;

    #[test]
    fn hover_cancel_preserves_trim_and_drop_replaces_it() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let app = App::new(&ui, Default::default(), Settings::default(), false, false);
        ui.set_view(View::Trim);
        ui.set_trim_a(0.25);
        ui.set_trim_b(0.75);
        ui.set_trim_pos(0.5);

        for active in [true, false] {
            app.file_hover(active);
            assert_eq!(ui.get_dragging(), active);
            assert_eq!(ui.get_view(), View::Trim);
            assert_eq!(
                (ui.get_trim_a(), ui.get_trim_b(), ui.get_trim_pos()),
                (0.25, 0.75, 0.5)
            );
        }

        let dir = tempfile::tempdir().unwrap();
        app.open_dropped(vec![dir.path().join("missing.mp4")]);
        assert_eq!(ui.get_view(), View::Trim);

        let replacements = vec![dir.path().join("a.mp4"), dir.path().join("b.mp4")];
        for path in &replacements {
            std::fs::write(path, []).unwrap();
        }
        app.open_dropped(replacements.clone());
        assert_eq!(ui.get_view(), View::Big);
        assert_eq!(*app.batch.borrow(), replacements);
    }
}
