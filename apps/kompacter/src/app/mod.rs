//! Tray application state, startup, and event routing.

mod flow;
pub(crate) mod format;
#[cfg(target_os = "linux")]
mod linux_desktop;
mod news;
mod output;
mod quick;
pub(crate) mod settings;
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
    ArchiveFormat, AudioFormat, CompressionPreference, Engine, Estimate, ImageFormat, MediaInfo,
    MediaKind, Mode, Options, VideoCodec,
};
use slint::winit_030::{winit, EventResult, WinitWindowAccessor};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::frontend::{motion, AppWindow, Fx, Prefs, PresetItem, Theme, Tray, View};
use crate::platform as sys;

use self::format::bytes;
use self::settings::Settings;

const COMPRESSION_PREFERENCES: [CompressionPreference; 3] = [
    CompressionPreference::Compatibility,
    CompressionPreference::Balanced,
    CompressionPreference::Efficiency,
];
const VIDEO_OVERRIDES: [Option<VideoCodec>; 4] = [
    None,
    Some(VideoCodec::H264),
    Some(VideoCodec::H265),
    Some(VideoCodec::Av1),
];
const AUDIO_OVERRIDES: [Option<AudioFormat>; 4] = [
    None,
    Some(AudioFormat::Auto),
    Some(AudioFormat::Mp3),
    Some(AudioFormat::Opus),
];
const IMAGE_OVERRIDES: [Option<ImageFormat>; 4] = [
    None,
    Some(ImageFormat::Auto),
    Some(ImageFormat::Jpeg),
    Some(ImageFormat::Webp),
];
const ARCHIVE_OVERRIDES: [Option<ArchiveFormat>; 3] =
    [None, Some(ArchiveFormat::Zip), Some(ArchiveFormat::SevenZ)];

fn audio_label(format: AudioFormat) -> &'static str {
    match format {
        AudioFormat::Auto => "Best fit",
        AudioFormat::Mp3 => "MP3",
        AudioFormat::Opus => "Opus",
    }
}

fn image_label(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Auto => "Best fit",
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::Webp => "WebP",
    }
}

fn archive_label(format: ArchiveFormat) -> &'static str {
    match format {
        ArchiveFormat::Zip => "Zip",
        ArchiveFormat::SevenZ => "7z",
    }
}

fn choice_index<T: PartialEq>(choices: &[T], value: T) -> i32 {
    choices
        .iter()
        .position(|choice| *choice == value)
        .unwrap_or(0) as i32
}

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
    /// Shell input received while a probe or compression owns the panel.
    pending_files: RefCell<Vec<PathBuf>>,
    pub started: Cell<Option<Instant>>,
    /// A file dialog is open: don't auto-hide while the user picks.
    pub modal: Cell<bool>,
    clipboard_pending: Cell<bool>,
    /// When the window last hid itself on focus loss (a tray click causes that too).
    pub auto_hidden: Cell<Option<Instant>>,
    /// All animations (off in Low-memory mode or when Windows animations are off).
    pub rich: bool,
    /// Floating see-through card; otherwise a plain window.
    pub glass: bool,
    /// The window is drawn on the GPU this run (Windows; chosen at start).
    pub gpu_drawing: bool,
    pub motion: motion::Motion,
    pub trim: trim::Trim,
    pub tray: RefCell<Option<Tray>>,
    /// The quick bar is showing (see quick.rs); `quick_real` is the flow's screen.
    pub quick: Cell<bool>,
    pub quick_real: Cell<View>,
    /// Paste guard: the chat app whose paste was held back.
    pub guard: Cell<Option<sys::PasteTarget>>,
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
    /// Starts a fresh copy (which waits for this one to close) and quits, so a setting
    /// that is read at start, like GPU drawing, takes effect.
    pub fn restart(self: &Rc<Self>) {
        #[cfg(windows)]
        {
            let Ok(exe) = std::env::current_exe() else {
                return;
            };
            if std::process::Command::new(exe)
                .arg(RESTARTED_ARG)
                .spawn()
                .is_ok()
            {
                self.trim_quit();
            }
        }
    }

    fn new(
        ui: &AppWindow,
        tray: Option<Tray>,
        prefs: Settings,
        rich: bool,
        glass: bool,
        gpu_drawing: bool,
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
            pending_files: RefCell::new(Vec::new()),
            started: Cell::new(None),
            modal: Cell::new(false),
            clipboard_pending: Cell::new(false),
            auto_hidden: Cell::new(None),
            rich,
            glass,
            gpu_drawing,
            motion: motion::Motion::default(),
            trim: trim::Trim::default(),
            tray: RefCell::new(tray),
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
        self.settings.borrow().options(target)
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
            "replace" if sys::CAPABILITIES.trash => 3,
            _ => 0,
        });
        prefs.set_output_dir(s.output_dir.as_str().into());
        prefs.set_windows_integration(cfg!(windows));
        prefs.set_replace_available(sys::CAPABILITIES.trash);
        prefs.set_trash_name(sys::TRASH_NAME.into());
        prefs.set_gpu(s.gpu);
        prefs.set_gpu_drawing_available(cfg!(windows));
        prefs.set_gpu_drawing(s.gpu_drawing());
        prefs.set_gpu_drawing_follows(s.gpu_drawing.is_none());
        prefs.set_gpu_drawing_running(self.gpu_drawing);
        prefs.set_auto_update(s.auto_update);
        prefs.set_auto_hide(s.auto_hide);
        prefs.set_start_available(sys::start_with_system_supported());
        prefs.set_start_label(sys::START_WITH_SYSTEM_NAME.into());
        prefs.set_start_with_windows(s.start_with_windows);
        prefs.set_explorer_menu(s.explorer_menu);
        prefs.set_compression_preference(choice_index(
            &COMPRESSION_PREFERENCES,
            s.compression_preference,
        ));
        prefs.set_video_index(choice_index(&VIDEO_OVERRIDES, s.video_codec));
        prefs.set_audio_index(choice_index(&AUDIO_OVERRIDES, s.audio_format));
        prefs.set_image_index(choice_index(&IMAGE_OVERRIDES, s.image_format));
        prefs.set_archive_index(choice_index(&ARCHIVE_OVERRIDES, s.archive_format));
        let auto = s.compression_preference.options(0);
        prefs.set_video_auto(auto.video_codec.label().into());
        prefs.set_audio_auto(audio_label(auto.audio_format).into());
        prefs.set_image_auto(image_label(auto.image_format).into());
        prefs.set_archive_auto(archive_label(auto.archive).into());
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

    /// Show the available automatic features and the compression preference.
    fn welcome(&self) {
        let ui = self.ui();
        let s = self.settings.borrow();
        let features: Vec<i32> = [
            (
                sys::CAPABILITIES.watch_copies && sys::watch_copies_support().is_ok(),
                0,
            ),
            (sys::CAPABILITIES.paste_guard, 1),
            (sys::CAPABILITIES.watch_folders, 2),
        ]
        .into_iter()
        .filter_map(|(available, feature)| available.then_some(feature))
        .collect();
        ui.set_welcome_features(ModelRc::new(VecModel::from(features)));
        ui.set_welcome_copies(true);
        ui.set_welcome_guard(s.paste_guard);
        ui.set_welcome_clips(s.watch_folders);
        ui.set_welcome_preference(choice_index(
            &COMPRESSION_PREFERENCES,
            s.compression_preference,
        ));
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

    /// Check GitHub for a newer version now and then, in the background. A checked
    /// download (or, where Kompacter can't update itself, a newer release) shows the
    /// "Update ready" line.
    fn start_update_checks(&self, current: &'static str) {
        let on = self.update_on.clone();
        std::thread::spawn(move || {
            // The installer of the update that just ran (or an old one) isn't needed.
            update::clear_downloads();
            // Let startup (and starting with the system) settle first.
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

    /// "Done" on the first-run screen: save the available choices and never ask again.
    fn welcome_done(self: &Rc<Self>) {
        let ui = self.ui();
        {
            let mut s = self.settings.borrow_mut();
            s.welcomed = true;
            s.compression_preference =
                COMPRESSION_PREFERENCES[ui.get_welcome_preference() as usize];
            if sys::CAPABILITIES.watch_copies && sys::watch_copies_support().is_ok() {
                s.watch_copies = ui.get_welcome_copies();
            }
            if sys::CAPABILITIES.paste_guard {
                s.paste_guard = ui.get_welcome_guard();
            }
        }
        self.save();
        let clips = ui.get_welcome_clips();
        if sys::CAPABILITIES.watch_folders && clips != self.settings.borrow().watch_folders {
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

    /// Raise the current panel, optionally replacing its selection with files from the shell.
    fn activate(self: &Rc<Self>, files: Vec<PathBuf>) {
        if !files.is_empty() {
            if self.started.get().is_some() || matches!(self.view(), View::Busy | View::Reading) {
                let mut pending = self.pending_files.borrow_mut();
                pending.extend(files);
                self.ui().set_queued_files(pending.len() as i32);
            } else {
                let mut pending = self.take_pending_files();
                pending.extend(files);
                self.replace_selection(pending);
            }
        }
        self.show();
    }

    fn take_pending_files(&self) -> Vec<PathBuf> {
        self.ui().set_queued_files(0);
        std::mem::take(&mut *self.pending_files.borrow_mut())
    }

    fn next_selection(self: &Rc<Self>) {
        let files = self.take_pending_files();
        self.another();
        self.open_many(files);
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

    fn paste_clipboard(self: &Rc<Self>) {
        if self.clipboard_pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        run(sys::clipboard_files, move |files| {
            let Some(app) = weak.upgrade() else { return };
            app.clipboard_pending.set(false);
            if !files.is_empty() {
                app.open_many(files);
                return;
            }
            let ui = app.ui();
            let note = SharedString::from("Copy a file first");
            ui.set_note(note.clone());
            let weak = ui.as_weak();
            slint::Timer::single_shot(Duration::from_millis(1800), move || {
                if let Some(ui) = weak.upgrade().filter(|ui| ui.get_note() == note) {
                    ui.set_note(SharedString::new());
                }
            });
        });
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
    #[cfg(windows)]
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

/// Passed to the copy started by "Restart" in Settings, which waits for this one to close.
#[cfg(windows)]
const RESTARTED_ARG: &str = "--restarted";

/// Picks Slint's winit backend and renderer. `gpu`: Skia on the GPU (Windows only).
fn select_backend(gpu: bool) -> Result<(), slint::PlatformError> {
    let backend = slint::BackendSelector::new().backend_name("winit".into());
    #[cfg(windows)]
    let backend = backend.renderer_name(if gpu { "skia" } else { "skia-software" }.into());
    #[cfg(not(windows))]
    let backend = backend.renderer_name("femtovg".into());
    backend
        .with_winit_window_attributes_hook(move |attrs| {
            #[cfg(windows)]
            {
                let _ = gpu;
                use winit::platform::windows::{CornerPreference, WindowAttributesExtWindows};
                attrs
                    .with_skip_taskbar(true)
                    .with_undecorated_shadow(false)
                    .with_corner_preference(CornerPreference::DoNotRound)
            }
            // The rounded card and its shadow are drawn by the UI on a clear window,
            // as on Windows and Linux, so AppKit's square window shadow stays off.
            #[cfg(target_os = "macos")]
            {
                let _ = gpu;
                use winit::platform::macos::WindowAttributesExtMacOS;
                attrs.with_transparent(true).with_has_shadow(false)
            }
            #[cfg(not(any(windows, target_os = "macos")))]
            {
                let _ = gpu;
                attrs
            }
        })
        .select()
}

pub(crate) fn start() -> Result<(), slint::PlatformError> {
    #[cfg(target_os = "linux")]
    let Some(linux_launch) = sys::linux::launch()
        .map_err(|error| slint::PlatformError::Other(format!("Kompacter activation: {error}")))?
    else {
        return Ok(());
    };
    // Files this launch was given ("Kompact" in Explorer, "Open with", a file dropped on
    // the shortcut). They're left where the running Kompacter looks; if that turns out
    // to be this one, it picks them up itself further down.
    #[cfg(windows)]
    {
        sys::leave_handoff(&sys::file_args(std::env::args()));
        // After "Restart" in Settings the old copy is still closing: wait for it.
        let restarted = std::env::args().any(|a| a == RESTARTED_ARG);
        if !(if restarted {
            sys::wait_first_instance()
        } else {
            sys::first_instance()
        }) {
            return Ok(());
        }
    }
    #[cfg(not(target_os = "linux"))]
    let quiet = std::env::args().any(|a| a == "--tray");
    // Only one Kompacter on macOS: a copy the login agent started while another runs
    // (or any other second copy) hands over to it and leaves.
    #[cfg(target_os = "macos")]
    if let Some(other) = sys::another_instance() {
        if !quiet {
            sys::activate_instance(&other);
        }
        return Ok(());
    }

    let prefs = settings::load();
    // Windows draws on the CPU (Skia raster: least memory, redraws only what changed)
    // unless GPU drawing is on (Skia on Direct3D 12: less CPU while animating, more
    // memory). Slint presents D3D12 through DirectComposition, so the see-through card
    // looks the same either way. If the GPU renderer can't start, the CPU one takes
    // over. macOS and Linux already draw on the GPU (FemtoVG).
    #[cfg(windows)]
    let gpu_drawing = prefs.gpu_drawing()
        && select_backend(true)
            .inspect_err(|e| eprintln!("GPU drawing unavailable: {e}"))
            .is_ok();
    #[cfg(windows)]
    if !gpu_drawing {
        select_backend(false)?;
    }
    #[cfg(not(windows))]
    let gpu_drawing = false;
    #[cfg(not(windows))]
    select_backend(false)?;

    #[cfg(target_os = "linux")]
    slint::set_xdg_app_id(sys::linux::APP_ID)?;
    let ui = AppWindow::new()?;
    // Skia (raster or Direct3D 12 through DirectComposition) draws the translucent card.
    // Installing a rendering notifier would disable the raster renderer's partial
    // redraws, even if the callback does nothing.
    #[cfg(windows)]
    let glass = true;
    // FemtoVG draws the translucent card on macOS and Linux alike.
    #[cfg(not(windows))]
    let glass = ui.window().set_rendering_notifier(|_, _| {}).is_ok();
    let rich = glass && sys::animations_enabled();
    ui.global::<Fx>().set_glass(glass);
    ui.global::<Fx>().set_rich(rich);
    // Starts folded, so the first open grows in.
    ui.set_opened(false);

    // The tray icon is Kompacter's home on Windows. On macOS the Dock icon is (click it
    // to open the panel above it, drop files on it), so there's no menu-bar icon too.
    // Linux adds its tray once a tray host appears (see `linux_desktop`).
    #[cfg(windows)]
    let tray = Some(Tray::new()?);
    #[cfg(not(windows))]
    let tray = None;
    let app = App::new(&ui, tray, prefs, rich, glass, gpu_drawing);
    APP.with(|a| *a.borrow_mut() = Rc::downgrade(&app));
    #[cfg(target_os = "macos")]
    if !sys::install_activation(|files| with_app(move |app| app.activate(files))) {
        eprintln!("Kompacter could not register all macOS activation events");
    }
    sys::start_input(|e| with_app(move |a| a.on_input(e)));
    // A tray app is only useful if it's running, so it starts with the system unless the
    // user says otherwise: switched on once here, and theirs to switch off from then on.
    // (Never from a dev build, which would register its target\debug exe.)
    if sys::start_with_system_supported() && !cfg!(debug_assertions) {
        let first = !app.settings.borrow().start_default_done;
        let entry = sys::start_entry();
        // An entry switched off in the system's own settings stays off.
        if (first && entry != sys::StartEntry::Disabled)
            || matches!(entry, sys::StartEntry::Here | sys::StartEntry::Elsewhere)
        {
            // Also keeps the entry pointing at this copy if the app was moved.
            sys::set_start_with_system(true);
        }
        if first {
            app.settings.borrow_mut().start_default_done = true;
            app.save();
        }
    }
    // Keeps the right-click entry pointing at this exe (not from a dev build either).
    if cfg!(windows) && !cfg!(debug_assertions) && app.settings.borrow().explorer_menu {
        sys::set_explorer_menu(true);
    }
    // The system's entry is the truth (it can be turned off in Task Manager, System
    // Settings or the desktop's startup apps).
    app.settings.borrow_mut().start_with_windows = sys::start_entry() == sys::StartEntry::Here;
    // Windows tucks new tray icons into the "^" overflow: offer once to keep it in view.
    ui.set_tray_tip(
        cfg!(windows) && !app.settings.borrow().tray_tip_seen && sys::tray_pinned() != Some(true),
    );
    app.sync_settings();
    ui.set_dash_path(format::dashed_round_rect(308.0, 300.0, 28.0, 6.0, 7.0, 0.0).into());

    // Tidy old outputs. GPU encoders are checked when a video first needs one.
    std::thread::spawn(|| output::clean(None));

    // ── Tray ──
    #[cfg(not(target_os = "linux"))]
    if let Some(tray) = app.tray.borrow().as_ref() {
        let weak = Rc::downgrade(&app);
        tray.on_open(move || {
            if let Some(app) = weak.upgrade() {
                #[cfg(target_os = "macos")]
                app.activate(Vec::new());
                #[cfg(windows)]
                app.toggle();
            }
        });
        tray.on_open_folder(|| {
            if let Ok(d) = output::app_dir() {
                sys::open_folder(&d);
            }
        });
        let weak = Rc::downgrade(&app);
        tray.on_quit(move || {
            if let Some(app) = weak.upgrade() {
                app.trim_quit();
            }
        });
    }
    #[cfg(target_os = "linux")]
    linux_desktop::attach(&app);

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
                                a.replace_selection(files);
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
        ui.on_paste(move || a.paste_clipboard());
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
        ui.on_another(move || a.next_selection());
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
        ui.on_restart_to_update(move || {
            let page = a
                .update_ready
                .borrow()
                .as_ref()
                .filter(|ready| !ready.installs_on_quit())
                .cloned();
            match page {
                // Kompacter can't replace itself here: open the release page and keep running.
                Some(ready) => {
                    let _ = update::install(&ready);
                }
                None => a.trim_quit(),
            }
        });
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
        prefs.on_set_gpu_drawing(move |on| {
            let mut s = a.settings.borrow_mut();
            // Back to "follow the preference" when the choice matches what it would pick.
            let follows = s.compression_preference == CompressionPreference::Efficiency;
            s.gpu_drawing = (on != follows).then_some(on);
            drop(s);
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        prefs.on_restart(move || a.restart());
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
            sys::set_start_with_system(on);
            a.settings.borrow_mut().start_with_windows =
                sys::start_entry() == sys::StartEntry::Here;
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
        prefs.on_set_compression_preference(move |i| {
            a.settings.borrow_mut().compression_preference =
                COMPRESSION_PREFERENCES[i.clamp(0, 2) as usize];
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        prefs.on_set_video(move |i| {
            a.settings.borrow_mut().video_codec =
                VIDEO_OVERRIDES.get(i as usize).copied().flatten();
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        prefs.on_set_audio(move |i| {
            a.settings.borrow_mut().audio_format =
                AUDIO_OVERRIDES.get(i as usize).copied().flatten();
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        prefs.on_set_image(move |i| {
            a.settings.borrow_mut().image_format =
                IMAGE_OVERRIDES.get(i as usize).copied().flatten();
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        prefs.on_set_archive(move |i| {
            a.settings.borrow_mut().archive_format =
                ARCHIVE_OVERRIDES.get(i as usize).copied().flatten();
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        prefs.on_reset_formats(move || {
            {
                let mut s = a.settings.borrow_mut();
                s.video_codec = None;
                s.audio_format = None;
                s.image_format = None;
                s.archive_format = None;
            }
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
    #[cfg(windows)]
    {
        let weak = Rc::downgrade(&app);
        SHOW.with(|f| {
            *f.borrow_mut() = Box::new(move || {
                // Explorer starts one launch per selected file: give them a moment to
                // all arrive, then open them together.
                let weak = weak.clone();
                slint::Timer::single_shot(Duration::from_millis(150), move || {
                    let Some(a) = weak.upgrade() else { return };
                    a.activate(sys::take_handoff());
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
        let guard = std::env::var("KOMPACTER_GUARD")
            .is_ok()
            .then_some(sys::PasteTarget::Window(0));
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
        app.start_update_checks(version);
    }
    // First run: ask once what Kompacter may do on its own (see `welcome_done`).
    if !app.settings.borrow().welcomed && app.view() == View::Idle && !app.quick.get() {
        app.welcome();
    }
    // Started with files: open them (and the window, even on a quiet start).
    #[cfg(not(target_os = "linux"))]
    {
        #[cfg(windows)]
        let given = sys::take_handoff();
        #[cfg(not(windows))]
        let given = sys::file_args(std::env::args());
        if !given.is_empty() || (!quiet && !app.quick.get()) {
            app.activate(given);
        }
    }
    #[cfg(target_os = "linux")]
    linux_desktop::start(&app, linux_launch);
    slint::run_event_loop_until_quit()?;
    app.tray.borrow_mut().take();
    // A checked update waits for Kompacter to close; the installer reopens it.
    if app.settings.borrow().auto_update {
        if let Some(ready) = app
            .update_ready
            .borrow()
            .as_ref()
            .filter(|ready| ready.installs_on_quit())
        {
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

/// Dev builds only: `KOMPACTER_DEMO=big|split|trim|busy|done|toobig|parts|error|settings|drag|discord|welcome(-2|-3|-4|-preference)|news|update`
/// shows a screen with sample data, for screenshots.
#[cfg(debug_assertions)]
fn demo(app: &Rc<App>, which: &str) {
    use crate::frontend::{DetailRow, DiffRow};

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
        // Numbered demos open the corresponding available step.
        "welcome" | "welcome-1" | "welcome-2" | "welcome-3" | "welcome-4"
        | "welcome-preference" => {
            app.welcome();
            let step = which
                .strip_prefix("welcome-")
                .and_then(|n| n.parse::<i32>().ok());
            let final_step = ui.get_welcome_features().row_count() as i32;
            ui.set_welcome_step(if which == "welcome-preference" {
                final_step
            } else {
                step.map_or(0, |n| n - 1).clamp(0, final_step)
            });
        }
        "quick" | "quick-busy" | "quick-done" | "guard" | "quick-empty" => {
            app.quick.set(true);
            ui.set_guard(which == "guard");
            app.guard
                .set((which == "guard").then_some(sys::PasteTarget::Window(1)));
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
mod tests {
    use super::*;

    #[test]
    fn welcome_preference_works_with_the_keyboard() {
        use slint::platform::{Key, WindowEvent};

        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        ui.set_welcome_features(ModelRc::new(VecModel::from(vec![2])));
        ui.set_view(View::Welcome);
        ui.set_opened(true);
        ui.show().unwrap();
        ui.window().dispatch_event(WindowEvent::Resized {
            size: slint::LogicalSize::new(360.0, 472.0),
        });
        ui.window()
            .dispatch_event(WindowEvent::WindowActiveChanged(true));
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));

        let weak = ui.as_weak();
        ui.on_welcome_done(move || weak.unwrap().set_view(View::Idle));
        let press = |key: Key| {
            ui.window()
                .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
            ui.window()
                .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
        };

        assert_eq!(ui.get_welcome_preference(), 1);
        press(Key::Return);
        assert_eq!(ui.get_welcome_step(), 1);
        press(Key::Home);
        assert_eq!(ui.get_welcome_preference(), 0);
        press(Key::End);
        assert_eq!(ui.get_welcome_preference(), 2);
        press(Key::LeftArrow);
        assert_eq!(ui.get_welcome_preference(), 1);
        press(Key::Return);
        assert_eq!(ui.get_view(), View::Idle);
    }

    #[test]
    fn hover_cancel_preserves_trim_and_drop_replaces_it() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let app = App::new(
            &ui,
            Default::default(),
            Settings::default(),
            false,
            false,
            false,
        );
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
        app.replace_selection(vec![dir.path().join("missing.mp4")]);
        assert_eq!(ui.get_view(), View::Trim);

        let replacements = vec![dir.path().join("a.mp4"), dir.path().join("b.mp4")];
        for path in &replacements {
            std::fs::write(path, []).unwrap();
        }
        app.replace_selection(replacements.clone());
        assert_eq!(ui.get_view(), View::Big);
        assert_eq!(*app.batch.borrow(), replacements);
    }
}
