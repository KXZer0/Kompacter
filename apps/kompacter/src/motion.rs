//! Showing and hiding the flyout, its size per screen, and the per-frame touches:
//! eyes that follow the mouse, the press squeezing smoothly, dashes marching on drag.
//!
//! Floating card (GPU drawing): the card grows out of the taskbar and folds back (Slint
//! animates it; Rust only flips `opened`). Plain window (Low-memory mode, or no GPU): Rust
//! slides and fades the window itself. The per-frame ticker runs only while the window is
//! visible and something is moving.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::winit_030::WinitWindowAccessor;
use slint::ComponentHandle;

use crate::{format, sys, App, AppWindow, View};

#[derive(Default)]
pub struct Motion {
    ticker: slint::Timer,
    slide: slint::Timer,
    /// Bumped on every show/close, so a stale close never hides a reopened window.
    generation: Cell<u32>,
    last: Cell<Option<Instant>>,
    squeeze: Cell<f32>,
    intro: Cell<f32>,
    dash: Cell<f64>,
    look: Cell<(f32, f32)>,
}

/// Content height of each screen.
pub fn view_height(v: View) -> f32 {
    match v {
        View::Trim => 624.0,
        View::Settings => 600.0,
        _ => 472.0,
    }
}

/// Bottom-right, above the taskbar. Returns the window's top-left in physical pixels.
fn place(ui: &AppWindow, glass: bool) -> Option<(i32, i32, f64)> {
    let (right, bottom, scale) = sys::corner()?;
    // The floating card has its own 12 px see-through margin (room for its shadow).
    let (inset, margin) = if glass { (12.0, 0.0) } else { (0.0, 12.0) };
    let w = (360.0 + 2.0 * inset) * scale;
    let h = (ui.get_win_h() as f64 + 2.0 * inset) * scale;
    let x = (right as f64 - w - margin * scale) as i32;
    let y = (bottom as f64 - h - margin * scale) as i32;
    ui.window().set_position(slint::PhysicalPosition::new(x, y));
    Some((x, y, scale))
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

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

impl App {
    pub fn set_view(&self, v: View) {
        let ui = self.ui();
        let was = ui.get_view();
        ui.set_view(v);
        if v == View::Busy && was != View::Busy {
            self.motion.squeeze.set(0.0);
            self.motion.intro.set(0.0);
            ui.set_squeeze(0.0);
            ui.set_intro(0.0);
        }
        let want = view_height(v);
        let have = ui.get_win_h();
        if want > have || !self.glass {
            ui.set_win_h(want);
            if ui.window().is_visible() {
                place(&ui, self.glass);
            }
        } else if want < have {
            // Let the card fold in first, then shrink the window to match.
            let w = ui.as_weak();
            let glass = self.glass;
            slint::Timer::single_shot(Duration::from_millis(400), move || {
                if let Some(ui) = w.upgrade() {
                    if view_height(ui.get_view()) == want && ui.get_win_h() != want {
                        ui.set_win_h(want);
                        place(&ui, glass);
                    }
                }
            });
        }
    }

    pub fn close_settings(&self) {
        let back = if self.current.borrow().is_some() {
            View::Big
        } else {
            View::Idle
        };
        self.set_view(back);
    }

    pub fn show(self: &Rc<Self>) {
        let ui = self.ui();
        let generation = self.motion.generation.get() + 1;
        self.motion.generation.set(generation);
        self.apply_theme();
        self.motion.slide.stop();

        let pos = place(&ui, self.glass);
        let _ = ui.show();
        // Again once the window exists (first show), then focus it.
        let pos = place(&ui, self.glass).or(pos);
        ui.window().with_winit_window(|w| w.focus_window());

        if self.glass {
            // Grow in (next frame, so the folded state is drawn first).
            let w = ui.as_weak();
            slint::Timer::single_shot(Duration::from_millis(16), move || {
                if let Some(ui) = w.upgrade() {
                    ui.set_opened(true);
                }
            });
        } else if let (Some(h), Some((x, y, scale))) = (hwnd(&ui), pos) {
            sys::style_plain_window(h);
            self.slide(h, x, y, scale, true, generation);
        }
        self.wake();
    }

    /// Fold or slide away, then hide.
    pub fn close(self: &Rc<Self>) {
        let ui = self.ui();
        if !ui.window().is_visible() {
            return;
        }
        let generation = self.motion.generation.get() + 1;
        self.motion.generation.set(generation);
        if self.glass {
            ui.set_opened(false);
            let weak = Rc::downgrade(self);
            slint::Timer::single_shot(Duration::from_millis(230), move || {
                if let Some(app) = weak.upgrade() {
                    if app.motion.generation.get() == generation {
                        app.hide_now();
                    }
                }
            });
        } else {
            let pos = ui.window().position();
            match hwnd(&ui) {
                Some(h) => self.slide(
                    h,
                    pos.x,
                    pos.y,
                    ui.window().scale_factor() as f64,
                    false,
                    generation,
                ),
                None => self.hide_now(),
            }
        }
    }

    fn hide_now(&self) {
        let ui = self.ui();
        if ui.get_view() == View::Settings {
            self.close_settings();
        }
        self.motion.ticker.stop();
        let _ = ui.hide();
    }

    pub fn toggle(self: &Rc<Self>) {
        let ui = self.ui();
        let open = ui.window().is_visible() && (!self.glass || ui.get_opened());
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

    /// Plain window: slide up and fade in (or down and out), then hide when closing.
    fn slide(
        self: &Rc<Self>,
        h: isize,
        x: i32,
        y: i32,
        scale: f64,
        opening: bool,
        generation: u32,
    ) {
        let start = Instant::now();
        let dur = if opening { 220.0 } else { 150.0 };
        let dist = (if opening { 18.0 } else { 12.0 }) * scale;
        let weak = Rc::downgrade(self);
        let step = move || -> bool {
            let Some(app) = weak.upgrade() else {
                return true;
            };
            if app.motion.generation.get() != generation {
                return true;
            }
            let t = (start.elapsed().as_secs_f64() * 1000.0 / dur).min(1.0) as f32;
            let k = if opening { ease_out(t) } else { t * t };
            let (off, alpha) = if opening {
                ((1.0 - k) as f64 * dist, k)
            } else {
                (k as f64 * dist, 1.0 - k)
            };
            sys::set_alpha(h, (alpha * 255.0) as u8);
            app.ui()
                .window()
                .set_position(slint::PhysicalPosition::new(x, y + off as i32));
            if t >= 1.0 {
                if !opening {
                    app.hide_now();
                    sys::set_alpha(h, 255);
                    app.ui()
                        .window()
                        .set_position(slint::PhysicalPosition::new(x, y));
                }
                return true;
            }
            false
        };
        if step() {
            return;
        }
        let step = std::cell::RefCell::new(step);
        let weak = Rc::downgrade(self);
        self.motion.slide.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(8),
            move || {
                if (step.borrow_mut())() {
                    if let Some(app) = weak.upgrade() {
                        app.motion.slide.stop();
                    }
                }
            },
        );
    }

    /// Start the per-frame ticker (if something needs it).
    pub fn wake(self: &Rc<Self>) {
        if self.motion.ticker.running() {
            return;
        }
        self.motion.last.set(None);
        let weak = Rc::downgrade(self);
        self.motion.ticker.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(16),
            move || {
                if let Some(app) = weak.upgrade() {
                    if !app.tick() {
                        app.motion.ticker.stop();
                    }
                }
            },
        );
    }

    /// One frame. Returns false when nothing needs frames any more.
    fn tick(&self) -> bool {
        let ui = self.ui();
        if !ui.window().is_visible() {
            return false;
        }
        let now = Instant::now();
        let dt = self
            .motion
            .last
            .replace(Some(now))
            .map_or(0.016, |l| now.duration_since(l).as_secs_f32().min(0.1));
        let view = ui.get_view();
        let mut busy = false;

        // The press follows real progress smoothly instead of jumping in steps.
        if view == View::Busy {
            busy = true;
            let target = ui.get_progress().clamp(0.0, 1.0);
            let cur = self.motion.squeeze.get();
            let next = cur + (target - cur) * (1.0 - (-dt / 0.22).exp());
            self.motion.squeeze.set(next);
            ui.set_squeeze(next);
            let i = (self.motion.intro.get() + dt / 0.55).min(1.0);
            self.motion.intro.set(i);
            ui.set_intro(ease_out(i));
        }

        if self.rich {
            // Eyes follow the mouse, anywhere on screen.
            let stage = matches!(
                view,
                View::Idle | View::Done | View::Reading | View::Big | View::Error
            );
            if let (true, Some((cx, cy))) = (stage, sys::cursor()) {
                let scale = ui.window().scale_factor();
                let pos = ui.window().position();
                let inset = if self.glass { 12.0 } else { 0.0 };
                let stage_h = if matches!(view, View::Idle | View::Done) {
                    312.0
                } else {
                    124.0
                };
                let bx = pos.x as f32 + (inset + 180.0) * scale;
                let by = pos.y as f32 + (inset + 50.0 + stage_h / 2.0) * scale;
                let lx = ((cx as f32 - bx) / (240.0 * scale)).clamp(-1.0, 1.0);
                let ly = ((cy as f32 - by) / (240.0 * scale)).clamp(-1.0, 1.0);
                let (ox, oy) = self.motion.look.get();
                if (lx - ox).abs() > 0.015 || (ly - oy).abs() > 0.015 {
                    self.motion.look.set((lx, ly));
                    ui.set_look_x(lx);
                    ui.set_look_y(ly);
                }
            }
            // Marching dashes while a file hovers over the drop zone.
            if ui.get_dragging() {
                let d = self.motion.dash.get() + dt as f64 * 22.0;
                self.motion.dash.set(d);
                ui.set_dash_path(format::dashed_round_rect(308.0, 300.0, 28.0, 6.0, 7.0, d).into());
            }
        }
        busy || self.rich
    }
}
