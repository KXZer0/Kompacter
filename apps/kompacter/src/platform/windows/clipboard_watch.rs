//! Noticing copies on Windows. A message-only window on its own thread is registered
//! with `AddClipboardFormatListener`, so Windows sends it `WM_CLIPBOARDUPDATE` after
//! every copy and nothing polls. Should that window not come up, the thread falls back
//! to comparing the clipboard's sequence number twice a second, as before.

use std::cell::RefCell;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::DataExchange::{
    AddClipboardFormatListener, RemoveClipboardFormatListener,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    PostThreadMessageW, RegisterClassW, HWND_MESSAGE, MSG, WM_CLIPBOARDUPDATE, WM_QUIT, WNDCLASSW,
};

/// Only used when the listener window can't be created.
const POLL: Duration = Duration::from_millis(500);

type OnCopy = Box<dyn Fn() + Send + Sync>;

thread_local! {
    /// The listener thread's callback; its window procedure runs on that thread.
    static ON_COPY: RefCell<Option<OnCopy>> = const { RefCell::new(None) };
}

enum Stop {
    /// The listener thread's id: posting `WM_QUIT` ends its message loop.
    Listener(u32),
    /// Dropping it ends the polling thread.
    Poll { _stop: mpsc::Sender<()> },
}

/// Watches the clipboard until dropped.
pub struct Watcher {
    stop: Stop,
}

impl Watcher {
    /// `on_copy` runs on the watcher's thread after each change to the clipboard.
    pub fn start(on_copy: impl Fn() + Send + Sync + 'static) -> Option<Self> {
        let (started, result) = mpsc::channel::<Option<u32>>();
        let (stop_poll, stopped) = mpsc::channel::<()>();
        let on_copy: OnCopy = Box::new(on_copy);
        std::thread::Builder::new()
            .name("clipboard-watch".into())
            .spawn(move || match listener_window() {
                Some(hwnd) => {
                    ON_COPY.with(|c| *c.borrow_mut() = Some(on_copy));
                    let _ = started.send(Some(unsafe { GetCurrentThreadId() }));
                    unsafe {
                        let mut msg: MSG = std::mem::zeroed();
                        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                            DispatchMessageW(&msg);
                        }
                        RemoveClipboardFormatListener(hwnd);
                        DestroyWindow(hwnd);
                    }
                }
                None => {
                    let _ = started.send(None);
                    let mut seq = super::clipboard_seq();
                    while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(POLL) {
                        let current = super::clipboard_seq();
                        if current != seq {
                            seq = current;
                            on_copy();
                        }
                    }
                }
            })
            .ok()?;
        let stop = match result.recv().ok()? {
            Some(thread) => Stop::Listener(thread),
            None => Stop::Poll { _stop: stop_poll },
        };
        Some(Self { stop })
    }

    /// Changes each time something new is copied.
    pub fn seq(&self) -> u32 {
        super::clipboard_seq()
    }

    /// The files of the latest copy (empty if it wasn't files).
    pub fn files(&self) -> Vec<PathBuf> {
        super::clipboard_files()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        if let Stop::Listener(thread) = self.stop {
            unsafe {
                PostThreadMessageW(thread, WM_QUIT, 0, 0);
            }
        }
    }
}

/// A hidden message-only window that receives `WM_CLIPBOARDUPDATE`.
fn listener_window() -> Option<HWND> {
    let class = super::wide("KompacterClipboardWatch");
    unsafe {
        let instance = GetModuleHandleW(null());
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(window_proc);
        wc.hInstance = instance;
        wc.lpszClassName = class.as_ptr();
        // Fails harmlessly when a previous watch already registered it.
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            return None;
        }
        if AddClipboardFormatListener(hwnd) == 0 {
            DestroyWindow(hwnd);
            return None;
        }
        Some(hwnd)
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_CLIPBOARDUPDATE {
        ON_COPY.with(|c| {
            if let Some(on_copy) = c.borrow().as_ref() {
                on_copy();
            }
        });
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}
