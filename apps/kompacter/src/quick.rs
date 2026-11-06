//! The quick bar and everything that opens it without the user opening Kompacter:
//! the global shortcut, the paste guard (Ctrl+V in Discord with a file that's too big),
//! copied files, and new clips in clip folders (those shrink on their own).
//!
//! The quick bar is a small view on top of the normal flow: the flow still moves through
//! Reading → Big → Busy → Done, and `set_view` shows each of those as a state of the bar
//! while `quick` is on. "Open Kompacter" turns it off and shows the real screen.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use slint::winit_030::WinitWindowAccessor;
use slint::ComponentHandle;

use crate::format::{bytes, file_name};
use crate::settings::{shortcut_keys, SHORTCUTS};
use crate::sys::{self, InputEvent};
use crate::watch::{self, WatchEvent};
use crate::{output, run, App, Prefs, View};

impl App {
    /// The screen the flow is on (behind the quick bar, when it's showing).
    pub fn view(&self) -> View {
        if self.quick.get() {
            self.quick_real.get()
        } else {
            self.ui().get_view()
        }
    }

    /// Open the quick bar for these files (nothing copied: says so). `guard` is the
    /// Discord window whose paste was held back.
    pub fn quick_open(self: &Rc<Self>, files: Vec<PathBuf>, guard: Option<isize>) {
        if self.view() == View::Busy {
            // Already compressing: show that instead.
            self.quick_expand();
            self.show();
            return;
        }
        let ui = self.ui();
        self.guard.set(guard);
        ui.set_guard(guard.is_some());
        self.quick.set(true);
        if files.is_empty() {
            *self.current.borrow_mut() = None;
            self.batch.borrow_mut().clear();
            ui.set_file_icon(slint::Image::default());
            self.quick_real.set(View::Idle);
            ui.set_quick_state(5);
            self.set_view(View::Quick);
        } else {
            self.open_many(files);
        }
        self.show();
    }

    /// "Open Kompacter": the full window, on the same screen the bar was showing.
    pub fn quick_expand(self: &Rc<Self>) {
        if !self.quick.get() {
            return;
        }
        self.quick.set(false);
        let real = match self.quick_real.get() {
            View::Quick => View::Idle,
            v => v,
        };
        self.set_view(real);
        if real == View::Big {
            self.show_big();
        }
        self.ui().window().with_winit_window(|w| w.focus_window());
    }

    pub fn quick_trim(self: &Rc<Self>) {
        self.quick.set(false);
        self.open_trim();
    }

    /// Paste guard: paste the original after all.
    pub fn paste_anyway(self: &Rc<Self>) {
        if let Some(w) = self.guard.take() {
            self.paste_into(w, 0);
        }
    }

    /// Fold away, bring `window` (Discord) back to the front and press Ctrl+V there.
    pub fn paste_into(self: &Rc<Self>, window: isize, delay_ms: u64) {
        self.guard.set(None);
        self.ui().set_guard(false);
        let weak = Rc::downgrade(self);
        slint::Timer::single_shot(Duration::from_millis(delay_ms), move || {
            let Some(app) = weak.upgrade() else { return };
            sys::focus_window(window);
            app.close();
            // Only if Discord really is in front: never paste into something else.
            slint::Timer::single_shot(Duration::from_millis(140), move || {
                if sys::is_foreground(window) {
                    sys::send_paste();
                }
            });
        });
    }

    /// A result is ready. Returns true when the paste guard pastes it (the caller then
    /// doesn't need to do anything else).
    pub fn guard_done(self: &Rc<Self>, pasted: bool) -> bool {
        match self.guard.take() {
            Some(w) if pasted => {
                self.paste_into(w, 600);
                true
            }
            _ => {
                self.ui().set_guard(false);
                false
            }
        }
    }

    /// The quick bar shows a result for a moment, then folds away by itself.
    pub fn quick_done(self: &Rc<Self>) {
        if !self.quick.get() {
            return;
        }
        let weak = Rc::downgrade(self);
        slint::Timer::single_shot(Duration::from_millis(2400), move || {
            let Some(app) = weak.upgrade() else { return };
            // Still showing the result: fold away (it's on the clipboard already).
            if app.quick.get() && app.quick_real.get() == View::Done {
                app.close();
            }
        });
    }

    /// The bar is closing: back to the normal window for next time.
    pub fn quick_hidden(&self) {
        self.guard.set(None);
        self.ui().set_guard(false);
        if self.quick.replace(false) {
            let real = match self.quick_real.get() {
                View::Quick => View::Idle,
                v => v,
            };
            self.set_view(real);
        }
    }

    pub fn on_input(self: &Rc<Self>, e: InputEvent) {
        match e {
            InputEvent::Shortcut => {
                let ui = self.ui();
                let open = ui.window().is_visible() && (!self.glass || ui.get_opened());
                if open {
                    self.close();
                } else {
                    self.quick_open(sys::clipboard_files(), None);
                }
            }
            InputEvent::Guard { files, window } => self.quick_open(files, Some(window)),
            InputEvent::ShortcutTaken(taken) => {
                self.ui().global::<Prefs>().set_shortcut_taken(taken);
            }
        }
    }

    pub fn on_watch(self: &Rc<Self>, e: WatchEvent) {
        match e {
            WatchEvent::Copied(files) => {
                // Not while the window is in use (and never over a game).
                if !self.ui().window().is_visible() && !sys::fullscreen_in_front() {
                    self.quick_open(files, None);
                }
            }
            WatchEvent::Clip(path) => {
                self.auto_queue.borrow_mut().push_back(path);
                self.auto_next();
            }
        }
    }

    /// Shrink the next new clip in the background. When it's done it's copied, and the
    /// quick bar says so (unless a game is full screen, or the window is in use).
    fn auto_next(self: &Rc<Self>) {
        if self.auto_busy.get() {
            return;
        }
        let Some(path) = self.auto_queue.borrow_mut().pop_front() else {
            return;
        };
        self.auto_busy.set(true);
        let name = file_name(&path);
        self.set_tray_tip(&format!("Kompacter · shrinking {name}"));
        let prefs = self.settings.borrow().clone();
        let target = self.target().bytes;
        let mut opts = self.options(target);
        opts.out_dir = output::dir(&prefs, &path).ok();
        let engine = self.engine.clone();
        let weak = Rc::downgrade(self);
        run(
            move || {
                let cancel = AtomicBool::new(false);
                let mut out = engine
                    .compress(&path, &opts, &mut |_| {}, &cancel)
                    .map_err(|e| e.to_string())?;
                if !out.untouched && prefs.output == "replace" {
                    out.path = output::replace_original(&out.path, &path)?;
                }
                let copied = out.fits && sys::copy_files(std::slice::from_ref(&out.path)).is_ok();
                output::clean(Some(&out.path));
                Ok::<_, String>((path, out, copied))
            },
            move |r| {
                let Some(app) = weak.upgrade() else { return };
                app.auto_busy.set(false);
                match r {
                    Ok((path, out, copied)) => {
                        let done = format!("{} · {}", file_name(&path), bytes(out.output_bytes));
                        app.set_tray_tip(&format!(
                            "Kompacter · {} {done}",
                            if copied { "copied" } else { "saved" }
                        ));
                        app.auto_tell(&path, &out, copied);
                    }
                    Err(e) => app.set_tray_tip(&format!("Kompacter · couldn't shrink {name}: {e}")),
                }
                app.auto_next();
            },
        );
    }

    /// Show a finished clip in the quick bar, without taking focus.
    fn auto_tell(
        self: &Rc<Self>,
        src: &std::path::Path,
        out: &kompacter_core::Outcome,
        copied: bool,
    ) {
        let ui = self.ui();
        if ui.window().is_visible() || sys::fullscreen_in_front() {
            return;
        }
        ui.set_file_name(file_name(src).into());
        ui.set_file_icon(slint::Image::default());
        ui.set_file_kind("video".into());
        ui.set_done_ok(out.fits);
        ui.set_done_title(if copied { "Clip copied" } else { "Clip saved" }.into());
        let ratio = out.original_bytes as f64 / out.output_bytes.max(1) as f64;
        ui.set_done_sub(
            format!(
                "{} · {ratio:.1}× smaller · {}",
                bytes(out.output_bytes),
                file_name(src)
            )
            .into(),
        );
        *self.result.borrow_mut() = Some(out.path.clone());
        self.quick.set(true);
        self.set_view(View::Done);
        self.show_with(false);
        self.quick_done();
    }

    pub fn set_tray_tip(&self, text: &str) {
        if let Some(t) = self.tray.upgrade() {
            t.set_tip(text.into());
        }
    }

    /// Push the automatic settings into the UI and the background parts.
    pub fn sync_automatic(&self) {
        let ui = self.ui();
        let prefs = ui.global::<Prefs>();
        let s = self.settings.borrow();
        let index = SHORTCUTS
            .iter()
            .position(|(id, _)| *id == s.shortcut)
            .unwrap_or(0);
        prefs.set_shortcut_index(index as i32);
        prefs.set_paste_guard(s.paste_guard);
        prefs.set_watch_copies(s.watch_copies);
        prefs.set_watch_folders(s.watch_folders);
        let shown: Vec<slint::SharedString> = s
            .clip_folders
            .iter()
            .map(|f| short_folder(f).into())
            .collect();
        prefs.set_clip_folders(slint::ModelRc::new(slint::VecModel::from(shown)));

        let limit = self.target().bytes;
        if *self.shortcut_set.borrow() != s.shortcut {
            *self.shortcut_set.borrow_mut() = s.shortcut.clone();
            sys::set_shortcut(shortcut_keys(&s.shortcut));
        }
        sys::set_guard(s.paste_guard, limit);
        self.watch.set(watch::Config {
            copies: s.watch_copies,
            folders_on: s.watch_folders,
            folders: s.clip_folders.iter().map(PathBuf::from).collect(),
            limit,
        });
    }

    /// Clip folders turned on: start with the ones this PC has, or ask for one.
    pub fn set_watch_folders(self: &Rc<Self>, on: bool) {
        let empty = self.settings.borrow().clip_folders.is_empty();
        if on && empty {
            let found: Vec<String> = watch::suggested_folders()
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            if found.is_empty() {
                self.pick(true, |app, dir| {
                    let mut s = app.settings.borrow_mut();
                    s.clip_folders.push(dir.display().to_string());
                    s.watch_folders = true;
                    drop(s);
                    app.save();
                    app.sync_settings();
                });
                return;
            }
            self.settings.borrow_mut().clip_folders = found;
        }
        self.settings.borrow_mut().watch_folders = on;
        self.save();
        self.sync_settings();
    }

    pub fn add_clip_folder(self: &Rc<Self>) {
        self.pick(true, |app, dir| {
            let d = dir.display().to_string();
            let mut s = app.settings.borrow_mut();
            if !s.clip_folders.contains(&d) {
                s.clip_folders.push(d);
            }
            drop(s);
            app.save();
            app.sync_settings();
        });
    }

    pub fn remove_clip_folder(&self, i: usize) {
        let mut s = self.settings.borrow_mut();
        if i < s.clip_folders.len() {
            s.clip_folders.remove(i);
        }
        if s.clip_folders.is_empty() {
            s.watch_folders = false;
        }
        drop(s);
        self.save();
        self.sync_settings();
    }
}

/// "Videos\Medal" instead of the whole path.
fn short_folder(f: &str) -> String {
    let p = std::path::Path::new(f);
    let parts: Vec<String> = p
        .components()
        .rev()
        .take(2)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    match parts.as_slice() {
        [last, parent] if !parent.ends_with(['\\', '/']) => format!("{parent}\\{last}"),
        [last, ..] => last.clone(),
        _ => f.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn short_folder_keeps_the_last_two_parts() {
        assert_eq!(
            super::short_folder("/home/me/Videos/Medal"),
            "Videos\\Medal"
        );
        assert_eq!(super::short_folder("/Medal"), "Medal");
    }
}
