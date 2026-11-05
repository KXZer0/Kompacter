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
mod output;
mod settings;
mod sys;
mod trim;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kompacter_core::size::PRESETS;
use kompacter_core::{
    ArchiveFormat, AudioFormat, Engine, Estimate, ImageFormat, MediaInfo, MediaKind, Options,
    VideoCodec,
};
use slint::winit_030::{winit, EventResult, WinitWindowAccessor};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

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
            let brand = if p.id.starts_with("discord") {
                "discord"
            } else {
                "mail"
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

/// The file currently in the window.
pub struct Current {
    pub path: PathBuf,
    pub info: MediaInfo,
    pub estimate: Estimate,
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
    pub started: Cell<Option<Instant>>,
    /// A file dialog is open: don't auto-hide while the user picks.
    pub modal: Cell<bool>,
    /// When the window last hid itself on focus loss (a tray click causes that too).
    pub auto_hidden: Cell<Option<Instant>>,
    /// All animations (off in Low-memory mode or when Windows animations are off).
    pub rich: bool,
    /// Floating see-through card (GPU drawing); otherwise a plain window.
    pub glass: bool,
    pub motion: motion::Motion,
    pub trim: trim::Trim,
}

impl App {
    pub fn ui(&self) -> AppWindow {
        self.ui.upgrade().expect("window")
    }

    pub fn target(&self) -> Target {
        let s = self.settings.borrow();
        let list = targets(s.custom_bytes);
        let i = list.iter().position(|t| t.id == s.preset).unwrap_or(0);
        list.into_iter().nth(i).expect("presets")
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
        settings::save(&self.settings.borrow());
    }

    /// Push settings and presets into the window.
    pub fn sync_settings(&self) {
        let ui = self.ui();
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
        ui.set_presets(ModelRc::new(VecModel::from(items)));
        ui.set_preset_index(index as i32);
        ui.set_target_text(bytes(list[index].bytes).into());

        let discord = s.style == "discord";
        ui.set_style_index(discord as i32);
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
        ui.set_theme_options(ModelRc::new(VecModel::from(options)));
        ui.set_theme_index(index);
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
        ui.set_low_memory(s.low_memory);
        ui.set_video_index(match s.video_codec {
            VideoCodec::H264 => 0,
            VideoCodec::H265 => 1,
            VideoCodec::Av1 => 2,
        });
        ui.set_audio_index(match s.audio_format {
            AudioFormat::Auto => 0,
            AudioFormat::Mp3 => 1,
            AudioFormat::Opus => 2,
        });
        ui.set_image_index(match s.image_format {
            ImageFormat::Auto => 0,
            ImageFormat::Jpeg => 1,
            ImageFormat::Webp => 2,
        });
        ui.set_archive_index((s.archive_format == ArchiveFormat::SevenZ) as i32);
        ui.set_custom_mb((s.custom_bytes / (1024 * 1024)) as i32);
        drop(s);
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

    pub fn another(&self) {
        *self.current.borrow_mut() = None;
        self.set_view(View::Idle);
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

    /// Restart Kompacter (the drawing mode can only be chosen at start).
    fn restart(&self) {
        if let Ok(exe) = std::env::current_exe() {
            let pid = std::process::id().to_string();
            if std::process::Command::new(exe)
                .args(["--after", &pid])
                .spawn()
                .is_ok()
            {
                let _ = slint::quit_event_loop();
            }
        }
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
}

fn main() -> Result<(), slint::PlatformError> {
    // After a restart, wait for the old copy to exit first.
    let args: Vec<String> = std::env::args().collect();
    if let Some(pid) = args
        .iter()
        .position(|a| a == "--after")
        .and_then(|i| args.get(i + 1))
        .and_then(|p| p.parse().ok())
    {
        sys::wait_for_exit(pid);
    }
    if !sys::first_instance() {
        return Ok(());
    }
    let quiet = args.iter().any(|a| a == "--tray");

    let prefs = settings::load();
    let low_memory = prefs.low_memory;

    let mut backend = slint::BackendSelector::new().backend_name("winit".into());
    if low_memory {
        // CPU drawing: the least memory.
        backend = backend.renderer_name("software".into());
    }
    // Otherwise the GPU first, with CPU drawing as Slint's automatic fallback.
    backend
        .with_winit_window_attributes_hook(move |attrs| {
            #[cfg(windows)]
            {
                use winit::platform::windows::{CornerPreference, WindowAttributesExtWindows};
                let attrs = attrs.with_skip_taskbar(true);
                if low_memory {
                    attrs
                        .with_undecorated_shadow(true)
                        .with_corner_preference(CornerPreference::Round)
                } else {
                    // The floating card draws its own corners and shadow.
                    attrs
                        .with_undecorated_shadow(false)
                        .with_corner_preference(CornerPreference::DoNotRound)
                }
            }
            #[cfg(not(windows))]
            attrs
        })
        .select()?;

    let ui = AppWindow::new()?;
    // Only GPU drawing supports a rendering hook, so this tells which one we got.
    let gpu = !low_memory && ui.window().set_rendering_notifier(|_, _| {}).is_ok();
    let rich = !low_memory && sys::animations_enabled();
    ui.global::<Fx>().set_glass(gpu);
    ui.global::<Fx>().set_rich(rich);
    // Starts folded, so the first open grows in.
    ui.set_opened(!gpu);

    let tray = Tray::new()?;
    let app = Rc::new(App {
        ui: ui.as_weak(),
        engine: Arc::new(Engine::new(sys::ffmpeg())),
        cancel: Arc::new(AtomicBool::new(false)),
        settings: RefCell::new(prefs),
        current: RefCell::new(None),
        result: RefCell::new(None),
        last_range: Cell::new(None),
        started: Cell::new(None),
        modal: Cell::new(false),
        auto_hidden: Cell::new(None),
        rich,
        glass: gpu,
        motion: motion::Motion::default(),
        trim: trim::Trim::default(),
    });
    // The Run entry is the truth (it can be turned off in Task Manager).
    app.settings.borrow_mut().start_with_windows = sys::starts_with_windows();
    app.sync_settings();
    ui.set_dash_path(format::dashed_round_rect(308.0, 300.0, 28.0, 6.0, 7.0, 0.0).into());

    // Find GPU encoders in the background (so the first compress doesn't wait), then
    // tidy old outputs.
    {
        let engine = app.engine.clone();
        let w = ui.as_weak();
        std::thread::spawn(move || {
            let none = engine.hw_encoders().is_empty();
            // Which newer codecs this PC's GPU can encode (else they run on the CPU, slower).
            let hevc = !engine.hw_encoders_for(VideoCodec::H265).is_empty();
            let av1 = !engine.hw_encoders_for(VideoCodec::Av1).is_empty();
            let w2 = w.clone();
            let _ = w2.upgrade_in_event_loop(move |ui| {
                ui.set_hevc_gpu(hevc);
                ui.set_av1_gpu(av1);
            });
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
                        a.close();
                    }
                }
                winit::event::WindowEvent::HoveredFile(_) => {
                    if ui.get_view() != View::Busy && !ui.get_dragging() {
                        if !matches!(ui.get_view(), View::Idle | View::Reading) {
                            a.another();
                        }
                        ui.set_dragging(true);
                        a.wake();
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
        ui.on_compress(move || a.compress(None, false, false));
        let a = app.clone();
        ui.on_split(move || {
            let range = a.last_range.get();
            a.compress(range, false, true)
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
                match a.ui().get_view() {
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
            if a.ui().get_view() != View::Busy {
                a.set_view(View::Settings);
            }
        });
        let a = app.clone();
        ui.on_close_settings(move || a.close_settings());
        let a = app.clone();
        ui.on_open_trim(move || a.open_trim());
        let a = app.clone();
        ui.on_close_trim(move || a.set_view(View::Big));
        let a = app.clone();
        ui.on_trim_changed(move || a.trim_update());
        let a = app.clone();
        ui.on_trim_compress(move || a.trim_compress());
        let a = app.clone();
        ui.on_escape(move || match a.ui().get_view() {
            View::Settings => a.close_settings(),
            View::Trim => a.set_view(View::Big),
            _ => a.close(),
        });
        let a = app.clone();
        ui.on_set_style(move |i| {
            a.settings.borrow_mut().style = if i == 1 { "discord" } else { "kompacter" }.into();
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        ui.on_set_theme(move |i| {
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
        ui.on_set_output(move |i| {
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
        ui.on_choose_folder(move || a.pick(true, set_custom_dir));
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
        ui.on_set_low_memory(move |on| {
            a.settings.borrow_mut().low_memory = on;
            a.save();
            a.sync_settings();
            // Let the switch finish moving, then come back in the new mode.
            let weak = Rc::downgrade(&a);
            slint::Timer::single_shot(Duration::from_millis(350), move || {
                if let Some(a) = weak.upgrade() {
                    a.restart();
                }
            });
        });
        let a = app.clone();
        ui.on_set_video(move |i| {
            a.settings.borrow_mut().video_codec =
                [VideoCodec::H264, VideoCodec::H265, VideoCodec::Av1][i.clamp(0, 2) as usize];
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        ui.on_set_audio(move |i| {
            a.settings.borrow_mut().audio_format =
                [AudioFormat::Auto, AudioFormat::Mp3, AudioFormat::Opus][i.clamp(0, 2) as usize];
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        ui.on_set_image(move |i| {
            a.settings.borrow_mut().image_format =
                [ImageFormat::Auto, ImageFormat::Jpeg, ImageFormat::Webp][i.clamp(0, 2) as usize];
            a.save();
            a.sync_settings();
        });
        let a = app.clone();
        ui.on_set_archive(move |i| {
            a.settings.borrow_mut().archive_format = if i == 1 {
                ArchiveFormat::SevenZ
            } else {
                ArchiveFormat::Zip
            };
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

    // A second launch (from the Start menu) opens the window.
    {
        let weak = Rc::downgrade(&app);
        SHOW.with(|f| {
            *f.borrow_mut() = Box::new(move || {
                if let Some(a) = weak.upgrade() {
                    a.show();
                }
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
    if !quiet {
        app.show();
    }
    slint::run_event_loop_until_quit()?;
    drop(tray);
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

/// Dev builds only: `KOMPACTER_DEMO=big|split|trim|busy|done|toobig|parts|error|settings|drag|discord`
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
    ui.set_compress_label("Compress to 720p".into());
    ui.set_can_trim(true);
    ui.set_progress(0.42);
    ui.set_squeeze(0.42);
    ui.set_intro(1.0);
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
        "discord" => {
            app.settings.borrow_mut().style = "discord".into();
            app.settings.borrow_mut().discord_theme = "ash".into();
            app.sync_settings();
        }
        _ => {}
    }
}
