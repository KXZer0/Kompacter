//! Showing and hiding the flyout, its size per screen, and the per-frame touches:
//! eyes that follow the mouse, the press squeezing smoothly, dashes marching on drag.
//!
//! Translucent card: the card grows out of the taskbar and folds back (Slint
//! animates it; Rust only flips `opened`). For an opaque window, Rust slides the
//! whole flyout. The ambient ticker runs only while the window is
//! open and the blob or drag indicator is on screen.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::winit_030::WinitWindowAccessor;
use slint::ComponentHandle;

use crate::app::{format, App};
use crate::platform as sys;

use super::{AppWindow, View};

const FLYOUT_HEIGHT: f32 = 624.0;

#[derive(Default)]
pub struct Motion {
    slide: slint::Timer,
    anchor: Cell<Option<sys::Anchor>>,
    reveal: Cell<f32>,
    /// Watches what has focus after Kompacter lost it (see `away`).
    away: slint::Timer,
    result_hide: slint::Timer,
    /// An automatic close in progress, and whether it requires losing focus.
    auto_closing: Cell<Option<bool>>,
    settings_return: Cell<Option<View>>,
    /// Bumped on every show/close, so a stale close never hides a reopened window.
    generation: Cell<u32>,
    last: Cell<Option<Instant>>,
    dash: Cell<f64>,
    look: Cell<(f32, f32)>,
    ambient_turn: Cell<f32>,
    input_height: Cell<f32>,
    /// Hiding destroyed the native window (macOS, Linux; see `app::start`). The next
    /// one is made once the event loop gets to it, after `show` has returned, and
    /// with Slint's default attributes: Kompacter's own touches wait for its first event.
    remade: Cell<bool>,
    /// That window should take the keyboard when it appears.
    remade_focus: Cell<bool>,
}

/// Content height of the screen showing (the paste guard's quick bar has one more row).
pub fn view_height(ui: &AppWindow) -> f32 {
    match ui.get_view() {
        View::Quick if ui.get_guard() => 168.0,
        View::Trim => 624.0,
        View::Settings => 600.0,
        View::Quick => 136.0,
        _ => 472.0,
    }
}

/// Position the flyout above the taskbar, with room for the slide at its bottom edge.
fn place(ui: &AppWindow, motion: &Motion, glass: bool, resize: bool) {
    // Rust owns the native size. In particular, a hidden flyout may still have
    // the dimensions of Settings even though its next view is shorter.
    let (inset, margin) = if glass { (12.0, 0.0) } else { (0.0, 12.0) };
    let width = 360.0 + 2.0 * inset;
    let height = f64::from(ui.get_win_h()) + 2.0 * inset;
    let Some(anchor) = motion.anchor.get() else {
        if resize {
            ui.window()
                .set_size(slint::LogicalSize::new(width as f32, height as f32));
        }
        return;
    };
    // macOS sizes and moves the window by its own scale factor, which can differ from
    // the screen's backing scale the anchor was measured in: convert the anchor to it.
    #[cfg(target_os = "macos")]
    let anchor = {
        let window = f64::from(ui.window().scale_factor());
        let k = if anchor.scale > 0.0 {
            window / anchor.scale
        } else {
            1.0
        };
        let px = |v: i32| (f64::from(v) * k).round() as i32;
        let (l, t, r, b) = anchor.free;
        sys::Anchor {
            free: (px(l), px(t), px(r), px(b)),
            scale: window,
            at: match anchor.at {
                sys::At::Above(x) => sys::At::Above(px(x)),
                sys::At::RightOf(y) => sys::At::RightOf(px(y)),
                sys::At::LeftOf(y) => sys::At::LeftOf(px(y)),
                at => at,
            },
        }
    };
    let scale = anchor.scale;
    let w = (width * scale).round() as i32;
    let h = (height * scale).round() as i32;
    let m = (margin * scale).round() as i32;
    let offset = if glass {
        0
    } else {
        (f64::from(1.0 - motion.reveal.get()) * 24.0 * scale).round() as i32
    };
    let (left, top, right, bottom) = anchor.free;
    // Centred on the Dock icon, but never past the edges of the free area.
    let centre =
        |at: i32, size: i32, low: i32, high: i32| (at - size / 2).min(high - size - m).max(low + m);
    // Each placement slides in from the taskbar, top bar or Dock it sits against.
    let (x, y) = match anchor.at {
        sys::At::BottomRight => (right - w - m, bottom - h - m + offset),
        sys::At::TopRight => (right - w - m, top + m - offset),
        sys::At::Above(cx) => (centre(cx, w, left, right), bottom - h - m + offset),
        sys::At::RightOf(cy) => (left + m - offset, centre(cy, h, top, bottom)),
        sys::At::LeftOf(cy) => (right - w - m + offset, centre(cy, h, top, bottom)),
    };
    let position = slint::PhysicalPosition::new(x, y);
    if resize {
        if let Some(hwnd) = hwnd(ui) {
            if sys::resize_flyout(hwnd, x, y, w, h) {
                return;
            }
        }
        ui.window()
            .set_size(slint::PhysicalSize::new(w as u32, h as u32));
    }
    if ui.window().position() != position {
        ui.window().set_position(position);
    }
}

#[cfg(windows)]
fn hwnd(ui: &AppWindow) -> Option<isize> {
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    ui.window()
        .with_winit_window(|w| match w.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
            _ => None,
        })
        .flatten()
}
#[cfg(not(windows))]
fn hwnd(_ui: &AppWindow) -> Option<isize> {
    None
}

/// Rounded corners and the system shadow for the panel on macOS.
#[cfg(target_os = "macos")]
fn style_mac_panel(ui: &AppWindow) {
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    ui.window().with_winit_window(|w| {
        if let Ok(handle) = w.window_handle() {
            if let RawWindowHandle::AppKit(h) = handle.as_raw() {
                sys::style_panel(h.ns_view);
            }
        }
    });
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

impl App {
    pub fn set_view(&self, v: View) {
        let ui = self.ui();
        let was = self.view();
        // The quick bar shows the flow's screens as its own states; any other screen
        // (idle, trimmer, settings) turns it off.
        let state = match v {
            View::Reading => Some(0),
            View::Big => Some(1),
            View::Busy => Some(2),
            View::Done => Some(3),
            View::Error => Some(4),
            _ => None,
        };
        if self.quick.get() && v != View::Quick && state.is_none() {
            self.quick.set(false);
        }
        let v = if self.quick.get() {
            if let Some(s) = state {
                ui.set_quick_state(s);
                self.quick_real.set(v);
            }
            View::Quick
        } else {
            v
        };
        if was == View::Trim && v != View::Trim {
            self.trim_halt();
        }
        if v == View::Settings && was != View::Settings {
            self.motion.settings_return.set(Some(was));
        }
        ui.set_view(v);
        // Settings and a compress started from the trimmer go back to it.
        if !matches!(v, View::Trim | View::Settings | View::Busy) {
            self.trim_release();
        }
        if was != self.view() {
            self.motion.result_hide.stop();
        }
        // Reset the press while it's out of sight, so the next run starts open.
        if was == View::Busy && self.view() != View::Busy {
            ui.set_squeeze(0.0);
        }
        self.mood_for_view(was);
        self.sync_window_height();
    }

    pub fn sync_window_height(&self) {
        let ui = self.ui();
        let want = view_height(&ui);
        if self.glass && cfg!(windows) {
            // Resizing the HWND after the card shrinks briefly presents its old
            // surface at the new position. Keep the native surface stationary,
            // and exclude the transparent space above the card from input.
            if ui.get_win_h() != FLYOUT_HEIGHT {
                ui.set_win_h(FLYOUT_HEIGHT);
                place(&ui, &self.motion, self.glass, true);
            }
            if want >= self.motion.input_height.get()
                || ui.get_card_settled()
                || !ui.window().is_visible()
            {
                self.set_flyout_input_height(want);
            }
            return;
        }
        let have = ui.get_win_h();
        if want != have && (want > have || ui.get_card_settled() || !ui.window().is_visible()) {
            ui.set_win_h(want);
            if ui.window().is_visible() {
                place(&ui, &self.motion, self.glass, true);
            }
        }
    }

    fn set_flyout_input_height(&self, height: f32) {
        if let Some(hwnd) = hwnd(&self.ui()) {
            if sys::clip_flyout_input(hwnd, height + 24.0, self.ui().window().scale_factor()) {
                self.motion.input_height.set(height);
            }
        }
    }

    pub fn close_settings(self: &Rc<Self>) {
        let back = self.motion.settings_return.take().unwrap_or_else(|| {
            if self.current.borrow().is_some() {
                View::Big
            } else {
                View::Idle
            }
        });
        if back == View::Big {
            let options = self.options(self.target_bytes());
            let refreshed = {
                let mut current = self.current.borrow_mut();
                current.as_mut().is_some_and(|file| {
                    self.engine
                        .estimate(&file.info, &options)
                        .is_ok_and(|estimate| {
                            file.estimate = estimate;
                            true
                        })
                })
            };
            if refreshed {
                self.show_big();
                return;
            }
        }
        self.set_view(back);
        if back == View::Trim {
            self.trim_update();
        }
    }

    pub fn show(self: &Rc<Self>) {
        self.show_with(true);
    }

    /// Show the window; `focus` false leaves the keyboard where it is (a notice).
    pub fn show_with(self: &Rc<Self>, focus: bool) {
        let ui = self.ui();
        let generation = self.motion.generation.get() + 1;
        self.motion.generation.set(generation);
        self.apply_theme();
        self.motion.slide.stop();
        self.motion.away.stop();
        self.motion.result_hide.stop();
        self.motion.auto_closing.set(None);
        self.auto_hidden.set(None);
        let was_hidden = !ui.window().is_visible();
        if was_hidden {
            self.mood_seen();
        }
        if was_hidden || self.motion.anchor.get().is_none() {
            self.motion.anchor.set(sys::anchor());
        }
        self.sync_window_height();

        let animate = sys::animations_enabled();
        if !self.glass && !animate {
            self.motion.reveal.set(1.0);
        }
        if !self.glass {
            if let Some(h) = hwnd(&ui) {
                sys::style_plain_window(h);
            }
            #[cfg(target_os = "macos")]
            style_mac_panel(&ui);
        }
        place(&ui, &self.motion, self.glass, was_hidden);
        if self.glass && cfg!(windows) {
            self.set_flyout_input_height(view_height(&ui));
        }
        let _ = ui.show();
        self.trim_activate();
        place(&ui, &self.motion, self.glass, was_hidden);
        if self.glass && cfg!(windows) {
            self.set_flyout_input_height(view_height(&ui));
        }
        if focus {
            #[cfg(target_os = "macos")]
            sys::focus_application();
            if ui
                .window()
                .with_winit_window(|w| w.focus_window())
                .is_none()
            {
                self.motion.remade_focus.set(true);
            }
            // Linux compositors may show the panel without granting focus.
            // Their actual focus-loss event still starts auto-hide.
            #[cfg(not(target_os = "linux"))]
            self.away();
        }

        if self.glass {
            if self.rich {
                // Draw the folded card before opening it, unless a close intervened.
                let weak = Rc::downgrade(self);
                slint::Timer::single_shot(Duration::from_millis(16), move || {
                    if let Some(app) = weak.upgrade() {
                        if app.motion.generation.get() == generation {
                            app.ui().set_opened(true);
                        }
                    }
                });
            } else {
                ui.set_opened(true);
            }
        } else {
            ui.set_opened(true);
            if let Some(h) = hwnd(&ui) {
                sys::style_plain_window(h);
            }
            if animate && cfg!(windows) {
                self.slide(true, generation);
            }
        }
    }

    /// Called for every native window event. A window made again after a hide gets what
    /// Slint doesn't carry over (on macOS: always on top, no AppKit shadow or the panel
    /// style) and the focus its show asked for.
    pub fn native_window_event(&self) {
        if !self.motion.remade.get() {
            return;
        }
        let ui = self.ui();
        let made = ui.window().with_winit_window(|_w| {
            // Slint remembers the level it last set and only sets it again on a change,
            // so the new window would be an ordinary one. (On X11 winit asks for the
            // level before the window is mapped, which window managers ignore, so the
            // first window isn't above others either; left as it was.)
            #[cfg(target_os = "macos")]
            {
                use slint::winit_030::winit::platform::macos::WindowExtMacOS;
                _w.set_window_level(slint::winit_030::winit::window::WindowLevel::AlwaysOnTop);
                // Set when the backend was chosen (see `app::select_backend`).
                if self.glass {
                    _w.set_has_shadow(false);
                }
            }
        });
        if made.is_none() {
            return;
        }
        self.motion.remade.set(false);
        #[cfg(target_os = "macos")]
        if !self.glass {
            style_mac_panel(&ui);
        }
        if self.motion.remade_focus.take() && ui.window().is_visible() {
            #[cfg(target_os = "macos")]
            sys::focus_application();
            ui.window().with_winit_window(|w| w.focus_window());
        }
    }

    /// Recheck focus after a short grace period. A drag or picker can temporarily
    /// prevent dismissal without losing the original focus change.
    pub fn away(self: &Rc<Self>) {
        if !self.settings.borrow().auto_hide || self.motion.away.running() {
            return;
        }
        let weak = Rc::downgrade(self);
        let ready = Cell::new(false);
        self.motion.away.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(200),
            move || {
                let Some(app) = weak.upgrade() else { return };
                let ui = app.ui();
                if !app.settings.borrow().auto_hide || app.has_focus() || !ui.window().is_visible()
                {
                    app.motion.away.stop();
                    return;
                }
                // Ignore transient focus changes and let pointer gestures finish.
                let allowed = ui.get_opened() && app.can_auto_hide(true);
                if ready.replace(allowed) && allowed {
                    app.motion.away.stop();
                    app.auto_hidden.set(Some(Instant::now()));
                    app.close_automatically(true);
                }
            },
        );
    }

    fn has_focus(&self) -> bool {
        #[cfg(windows)]
        return sys::app_is_foreground();
        #[cfg(not(windows))]
        self.ui()
            .window()
            .with_winit_window(|w| w.has_focus())
            .unwrap_or(false)
    }

    fn can_auto_hide(&self, require_unfocused: bool) -> bool {
        let ui = self.ui();
        self.settings.borrow().auto_hide
            && ui.window().is_visible()
            && (!require_unfocused || !self.has_focus())
            && !self.modal.get()
            && !ui.get_dragging()
            && self.dropped.borrow().is_empty()
            && self.view() != View::Busy
            && !sys::left_mouse_down()
    }

    pub fn cancel_auto_hide(self: &Rc<Self>) {
        self.motion.away.stop();
        self.motion.result_hide.stop();
        if self.motion.auto_closing.take().is_some() {
            self.show_with(false);
        }
    }

    pub fn auto_hide_result(self: &Rc<Self>) {
        let quick = self.quick.get();
        if !self.settings.borrow().auto_hide || (!quick && self.has_focus()) {
            return;
        }
        self.motion.away.stop();
        let weak = Rc::downgrade(self);
        self.motion.result_hide.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(if quick { 2400 } else { 2500 }),
            move || {
                if let Some(app) = weak.upgrade() {
                    if app.view() != View::Done
                        || app.quick.get() != quick
                        || (!quick && app.has_focus())
                    {
                        app.motion.result_hide.stop();
                    } else if app.ui().get_opened() && app.can_auto_hide(!quick) {
                        app.motion.result_hide.stop();
                        app.close_automatically(!quick);
                    }
                }
            },
        );
    }

    fn close_automatically(self: &Rc<Self>, require_unfocused: bool) {
        if self.ui().get_opened() && self.can_auto_hide(require_unfocused) {
            self.motion.auto_closing.set(Some(require_unfocused));
            self.begin_close();
        }
    }

    /// Fold or slide away, then hide.
    pub fn close(self: &Rc<Self>) {
        self.motion.auto_closing.set(None);
        self.trim_halt();
        self.begin_close();
    }

    fn begin_close(self: &Rc<Self>) {
        let ui = self.ui();
        if !ui.window().is_visible() {
            return;
        }
        let generation = self.motion.generation.get() + 1;
        self.motion.generation.set(generation);
        ui.set_opened(false);
        if self.glass && self.rich {
            // Start evaluating the transition now. An unfocused flyout has no
            // ambient ticker, so keep its exit frames scheduled until the
            // animation actually reaches zero instead of hiding on a deadline.
            let _ = ui.get_flyout_reveal();
            ui.window().request_redraw();
            let weak = Rc::downgrade(self);
            self.motion.slide.start(
                slint::TimerMode::Repeated,
                Duration::from_millis(16),
                move || {
                    let Some(app) = weak.upgrade() else { return };
                    let ui = app.ui();
                    if app.motion.generation.get() != generation || ui.get_opened() {
                        app.motion.slide.stop();
                        return;
                    }
                    let reveal = ui.get_flyout_reveal();
                    ui.window().request_redraw();
                    if reveal == 0.0 {
                        app.motion.slide.stop();
                        let weak = Rc::downgrade(&app);
                        slint::Timer::single_shot(Duration::from_millis(16), move || {
                            if let Some(app) = weak.upgrade() {
                                if app.motion.generation.get() == generation {
                                    app.finish_close();
                                }
                            }
                        });
                    }
                },
            );
        } else if !self.glass && sys::animations_enabled() && hwnd(&ui).is_some() {
            self.slide(false, generation);
        } else {
            self.finish_close();
        }
    }

    fn finish_close(self: &Rc<Self>) {
        if let Some(require_unfocused) = self.motion.auto_closing.take() {
            if !self.can_auto_hide(require_unfocused) {
                self.show_with(false);
                if require_unfocused && !self.has_focus() {
                    self.away();
                }
                return;
            }
        }
        self.hide_now();
    }

    fn hide_now(self: &Rc<Self>) {
        let ui = self.ui();
        let _ = ui.hide();
        // Slint drops the window and its renderer on hide outside Windows (see
        // `app::start`), so the next show makes a new one.
        if !cfg!(windows) {
            self.motion.remade.set(true);
            self.motion.remade_focus.set(false);
        }
        if ui.get_view() == View::Settings {
            self.close_settings();
        }
        self.trim_halt();
        self.trim_drop_later();
        self.quick_hidden();
        self.motion.last.set(None);
        self.motion.slide.stop();
        self.motion.away.stop();
        self.motion.result_hide.stop();
        self.motion.reveal.set(0.0);
        sys::release_memory();
    }

    #[cfg(windows)]
    pub fn toggle(self: &Rc<Self>) {
        let ui = self.ui();
        let open = ui.window().is_visible() && ui.get_opened();
        if open {
            self.close();
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

    /// Move the native window without resizing or rerendering its contents. A reversal
    /// starts at the current position, so repeated tray clicks never jump to an endpoint.
    fn slide(self: &Rc<Self>, opening: bool, generation: u32) {
        let start = Instant::now();
        let from = self.motion.reveal.get();
        let to = if opening { 1.0 } else { 0.0 };
        let duration = if opening { 0.180 } else { 0.160 };
        let weak = Rc::downgrade(self);
        self.motion.slide.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(16),
            move || {
                let Some(app) = weak.upgrade() else { return };
                if app.motion.generation.get() != generation {
                    return;
                }
                let t = (start.elapsed().as_secs_f32() / duration).min(1.0);
                let reveal = from + (to - from) * ease_out(t);
                app.motion.reveal.set(reveal);
                place(&app.ui(), &app.motion, false, false);
                if t == 1.0 {
                    app.motion.slide.stop();
                    if !opening {
                        app.finish_close();
                    }
                }
            },
        );
    }

    /// Update the visible blob's ambient motion and gaze, plus the drag indicator.
    pub fn tick_motion(&self, track_eyes: bool) {
        let ui = self.ui();
        let now = Instant::now();
        let dt = self
            .motion
            .last
            .replace(Some(now))
            .map_or(0.033, |l| now.duration_since(l).as_secs_f32().min(0.066));

        // Eyes follow the mouse, anywhere on screen.
        if track_eyes {
            let turn = (self.motion.ambient_turn.get() + dt / 18.0).fract();
            self.motion.ambient_turn.set(turn);
            ui.set_ambient_turn(turn);
            if let Some((cx, cy)) = sys::cursor() {
                let view = ui.get_view();
                let scale = ui.window().scale_factor();
                let pos = ui.window().position();
                let inset = if self.glass { 12.0 } else { 0.0 };
                let stage_h = if matches!(view, View::Idle | View::Done) {
                    312.0
                } else {
                    124.0
                };
                let bx = pos.x as f32 + (inset + 180.0) * scale;
                // The idle blob sits a bit higher, above the drop hint.
                let lift = if view == View::Idle { 40.0 } else { 0.0 };
                let by = pos.y as f32 + (inset + 50.0 + stage_h / 2.0 - lift) * scale;
                let lx = ((cx as f32 - bx) / (240.0 * scale)).clamp(-1.0, 1.0);
                let ly = ((cy as f32 - by) / (240.0 * scale)).clamp(-1.0, 1.0);
                let (ox, oy) = self.motion.look.get();
                if (lx - ox).abs() > 0.015 || (ly - oy).abs() > 0.015 {
                    self.motion.look.set((lx, ly));
                    ui.set_look_x(lx);
                    ui.set_look_y(ly);
                }
            }
        }
        // Marching dashes while a file hovers over the drop zone.
        if ui.get_dragging() {
            let d = self.motion.dash.get() + dt as f64 * 22.0;
            self.motion.dash.set(d);
            ui.set_dash_path(format::dashed_round_rect(308.0, 300.0, 28.0, 6.0, 7.0, d).into());
        }
    }
}
