//! Noticing copied files on macOS. AppKit is only touched on the main thread: a timer
//! there compares the pasteboard's change count (a cheap integer read) and reads the
//! file URLs only when it moved, then tells the watch thread, which reads what the timer
//! left. macOS has no pasteboard-changed notification, so the timer stays; the watch
//! thread itself sleeps until told.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use slint::{Timer, TimerMode};

const POLL: Duration = Duration::from_millis(250);

struct Shared {
    seq: AtomicU32,
    files: Mutex<Vec<PathBuf>>,
    stopped: AtomicBool,
    /// Called on the main thread after each new copy.
    on_copy: Box<dyn Fn() + Send + Sync>,
}

thread_local! {
    /// The main thread's polling timer; a newer watch replaces it.
    static TIMER: RefCell<Option<Timer>> = const { RefCell::new(None) };
}

/// Watches the pasteboard until dropped.
pub struct Watcher {
    shared: Arc<Shared>,
}

impl Watcher {
    /// `None` when the UI event loop is gone. `on_copy` runs (on the main thread) after
    /// each new copy.
    pub fn start(on_copy: impl Fn() + Send + Sync + 'static) -> Option<Self> {
        let shared = Arc::new(Shared {
            seq: AtomicU32::new(0),
            files: Mutex::new(Vec::new()),
            stopped: AtomicBool::new(false),
            on_copy: Box::new(on_copy),
        });
        let polled = shared.clone();
        slint::invoke_from_event_loop(move || poll_on_main_thread(polled)).ok()?;
        Some(Self { shared })
    }

    /// Changes each time something new is copied.
    pub fn seq(&self) -> u32 {
        self.shared.seq.load(Ordering::Acquire)
    }

    /// The files of the latest copy (empty if it wasn't files).
    pub fn files(&self) -> Vec<PathBuf> {
        self.shared
            .files
            .lock()
            .map(|f| f.clone())
            .unwrap_or_default()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Release);
    }
}

fn poll_on_main_thread(shared: Arc<Shared>) {
    if shared.stopped.load(Ordering::Acquire) {
        return;
    }
    // What is on the pasteboard now was copied before the watch began.
    let mut last = super::macos::clipboard_seq();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, POLL, move || {
        if shared.stopped.load(Ordering::Acquire) {
            TIMER.with(|t| {
                if let Some(timer) = t.borrow().as_ref() {
                    timer.stop();
                }
            });
            return;
        }
        let count = super::macos::clipboard_seq();
        if count == last {
            return;
        }
        last = count;
        let files = super::macos::clipboard_files();
        if let Ok(mut latest) = shared.files.lock() {
            *latest = files;
        }
        shared.seq.fetch_add(1, Ordering::Release);
        (shared.on_copy)();
    });
    TIMER.with(|t| *t.borrow_mut() = Some(timer));
}
