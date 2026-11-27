//! The tray icon (Windows, Linux) and the Dock icon (macOS) show what Kompacter is doing:
//! the press squashing the blob while a file shrinks (automatic clip jobs too), a check
//! for a few seconds after it worked, a dizzy blob with a red X after it failed (until the
//! panel opens), and the idle blob otherwise. They follow the style (Kompacter or Discord).
//!
//! The art is drawn by tools/tray_icons.py into frontend/tray/. The only timers run while
//! a job does (frames) or for a few seconds after (back to idle): nothing ticks when idle.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use slint::{ComponentHandle, Image, Timer, TimerMode};
#[cfg(not(target_os = "macos"))]
use slint::{Rgba8Pixel, SharedPixelBuffer};

use super::{App, APP};
use crate::frontend::View;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mood {
    Idle,
    Busy,
    Done,
    Error,
}

/// The compressing loop: 16 frames at 10 fps.
const FRAMES: usize = 16;
const FRAME_TIME: Duration = Duration::from_millis(100);
/// Shown when animations are off: the press closed, the blob squished.
const STILL_FRAME: usize = 6;

#[derive(Default)]
pub struct TrayArt {
    mood: Cell<Option<Mood>>,
    discord: Cell<bool>,
    frame: Cell<usize>,
    /// The compressing frames for the tray's tint and size; empty unless busy.
    frames: RefCell<Vec<Image>>,
    anim: Timer,
}

impl App {
    /// The flow changed screens: compressing, finished or failed show in the icon.
    pub(crate) fn mood_for_view(&self, was: View) {
        let now = self.view();
        match now {
            View::Busy => self.set_mood(Mood::Busy),
            _ if now == was => {}
            View::Done if self.ui().get_done_ok() => self.set_mood(Mood::Done),
            View::Done | View::Error => self.set_mood(Mood::Error),
            // Cancelled (or left the busy screen some other way).
            _ if was == View::Busy => self.settle_mood(),
            _ => {}
        }
    }

    pub(super) fn set_mood(&self, mood: Mood) {
        let art = &self.tray_art;
        // Done and error are news for someone not looking: with the panel open it shows
        // the result itself, so the icon only ever says "compressing" or nothing. Hidden,
        // they stay until the panel is opened (`mood_seen`).
        if matches!(mood, Mood::Done | Mood::Error) && self.ui().window().is_visible() {
            return self.settle_mood();
        }
        if art.mood.get() == Some(mood) {
            return;
        }
        art.mood.set(Some(mood));
        art.frame.set(STILL_FRAME);
        art.anim.stop();
        art.frames.borrow_mut().clear();
        if mood == Mood::Busy && self.rich {
            art.frame.set(0);
            art.anim.start(TimerMode::Repeated, FRAME_TIME, || {
                if let Some(app) = APP.with(|a| a.borrow().upgrade()) {
                    let art = &app.tray_art;
                    art.frame.set((art.frame.get() + 1) % FRAMES);
                    app.show_mood();
                }
            });
        }
        self.show_mood();
    }

    /// Back to compressing if a job still runs, else idle.
    pub(super) fn settle_mood(&self) {
        let busy = self.view() == View::Busy || self.auto_busy.get();
        self.set_mood(if busy { Mood::Busy } else { Mood::Idle });
    }

    /// The panel opened from hidden: it shows the result, so the icon can stop saying it.
    pub(crate) fn mood_seen(&self) {
        if matches!(self.tray_art.mood.get(), Some(Mood::Done | Mood::Error)) {
            self.settle_mood();
        }
    }

    /// The style changed (or a tray appeared): redraw in the right tint.
    pub(super) fn tint_mood(&self, discord: bool) {
        let art = &self.tray_art;
        if art.discord.replace(discord) != discord {
            art.frames.borrow_mut().clear();
            self.show_mood();
        }
    }

    pub(super) fn show_mood(&self) {
        let art = &self.tray_art;
        let mood = art.mood.get().unwrap_or(Mood::Idle);
        let tint = art.discord.get() as usize;
        #[cfg(not(target_os = "macos"))]
        if let Some(tray) = self.tray.borrow().as_ref() {
            let size = tray_size();
            let image = if mood == Mood::Busy {
                let mut frames = art.frames.borrow_mut();
                if frames.is_empty() {
                    *frames = strip(BUSY[tint][size], FRAMES);
                }
                frames.get(art.frame.get()).cloned()
            } else {
                // The strip holds idle, done, error.
                let index = match mood {
                    Mood::Done => 1,
                    Mood::Error => 2,
                    _ => 0,
                };
                strip(MOODS[tint][size], 3).into_iter().nth(index)
            };
            if let Some(image) = image {
                tray.set_art(image);
            }
        }
        #[cfg(target_os = "macos")]
        crate::platform::set_dock_icon(match mood {
            // The bundle's own icon is the idle Kompacter blob.
            Mood::Idle if tint == 0 => None,
            Mood::Idle => Some(DOCK_IDLE_DISCORD),
            Mood::Done => Some(DOCK_DONE[tint]),
            Mood::Error => Some(DOCK_ERROR[tint]),
            Mood::Busy => Some(DOCK_BUSY[tint][art.frame.get()]),
        });
    }
}

/// Cuts an embedded strip of `count` square icons into images.
#[cfg(not(target_os = "macos"))]
fn strip(png: &[u8], count: usize) -> Vec<Image> {
    let Some(all) = Image::load_from_data(png, Some("png"))
        .ok()
        .and_then(|i| i.to_rgba8())
    else {
        return Vec::new();
    };
    let n = all.height() as usize;
    let width = all.width() as usize;
    let pixels = all.as_slice();
    (0..count.min(width / n.max(1)))
        .map(|i| {
            let mut one = SharedPixelBuffer::<Rgba8Pixel>::new(n as u32, n as u32);
            let out = one.make_mut_slice();
            for y in 0..n {
                out[y * n..(y + 1) * n].copy_from_slice(&pixels[y * width + i * n..][..n]);
            }
            Image::from_rgba8(one)
        })
        .collect()
}

/// Icon sizes drawn for the tray.
#[cfg(not(target_os = "macos"))]
const SIZES: [u32; 5] = [16, 20, 24, 32, 48];

/// Which drawn size to hand the tray (an index into `SIZES`). Slint takes one image and
/// the shell scales it, so Windows gets its small-icon size for the system DPI (16 px at
/// 100 %, 24 at 150 %, 32 at 200 %); Linux tray hosts scale it themselves (22-24 px
/// usually), so they get 32.
#[cfg(not(target_os = "macos"))]
fn tray_size() -> usize {
    #[cfg(windows)]
    let want = crate::platform::tray_icon_px();
    #[cfg(not(windows))]
    let want = 32;
    SIZES
        .iter()
        .position(|&s| s >= want)
        .unwrap_or(SIZES.len() - 1)
}

macro_rules! art {
    ($($file:literal),* $(,)?) => {
        [$(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/frontend/tray/", $file)).as_slice()),*]
    };
}

// [tint][size]: tint 0 = Kompacter, 1 = Discord.
#[cfg(not(target_os = "macos"))]
static MOODS: [[&[u8]; 5]; 2] = [
    art!(
        "kompacter-moods-16.png",
        "kompacter-moods-20.png",
        "kompacter-moods-24.png",
        "kompacter-moods-32.png",
        "kompacter-moods-48.png",
    ),
    art!(
        "discord-moods-16.png",
        "discord-moods-20.png",
        "discord-moods-24.png",
        "discord-moods-32.png",
        "discord-moods-48.png",
    ),
];
#[cfg(not(target_os = "macos"))]
static BUSY: [[&[u8]; 5]; 2] = [
    art!(
        "kompacter-busy-16.png",
        "kompacter-busy-20.png",
        "kompacter-busy-24.png",
        "kompacter-busy-32.png",
        "kompacter-busy-48.png",
    ),
    art!(
        "discord-busy-16.png",
        "discord-busy-20.png",
        "discord-busy-24.png",
        "discord-busy-32.png",
        "discord-busy-48.png",
    ),
];

#[cfg(target_os = "macos")]
static DOCK_IDLE_DISCORD: &[u8] = art!("discord-dock-idle.png")[0];
#[cfg(target_os = "macos")]
static DOCK_DONE: [&[u8]; 2] = art!("kompacter-dock-done.png", "discord-dock-done.png");
#[cfg(target_os = "macos")]
static DOCK_ERROR: [&[u8]; 2] = art!("kompacter-dock-error.png", "discord-dock-error.png");
#[cfg(target_os = "macos")]
static DOCK_BUSY: [[&[u8]; FRAMES]; 2] = [
    art!(
        "kompacter-dock-busy-00.png",
        "kompacter-dock-busy-01.png",
        "kompacter-dock-busy-02.png",
        "kompacter-dock-busy-03.png",
        "kompacter-dock-busy-04.png",
        "kompacter-dock-busy-05.png",
        "kompacter-dock-busy-06.png",
        "kompacter-dock-busy-07.png",
        "kompacter-dock-busy-08.png",
        "kompacter-dock-busy-09.png",
        "kompacter-dock-busy-10.png",
        "kompacter-dock-busy-11.png",
        "kompacter-dock-busy-12.png",
        "kompacter-dock-busy-13.png",
        "kompacter-dock-busy-14.png",
        "kompacter-dock-busy-15.png",
    ),
    art!(
        "discord-dock-busy-00.png",
        "discord-dock-busy-01.png",
        "discord-dock-busy-02.png",
        "discord-dock-busy-03.png",
        "discord-dock-busy-04.png",
        "discord-dock-busy-05.png",
        "discord-dock-busy-06.png",
        "discord-dock-busy-07.png",
        "discord-dock-busy-08.png",
        "discord-dock-busy-09.png",
        "discord-dock-busy-10.png",
        "discord-dock-busy-11.png",
        "discord-dock-busy-12.png",
        "discord-dock-busy-13.png",
        "discord-dock-busy-14.png",
        "discord-dock-busy-15.png",
    ),
];

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn strips_hold_every_icon() {
        for tint in 0..2 {
            for (size, &n) in SIZES.iter().enumerate() {
                let moods = strip(MOODS[tint][size], 3);
                let busy = strip(BUSY[tint][size], FRAMES);
                assert_eq!((moods.len(), busy.len()), (3, FRAMES));
                for image in moods.iter().chain(&busy) {
                    assert_eq!((image.size().width, image.size().height), (n, n));
                }
            }
        }
    }
}
