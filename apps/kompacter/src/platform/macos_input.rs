//! macOS: the global shortcut and the paste guard.
//!
//! The shortcut is a Carbon hot key (`RegisterEventHotKey`), which needs no permission
//! and is delivered on the main thread through the application's event target. The
//! paste guard is a `CGEventTap` on the login session that can drop Cmd+V before the
//! chat app sees it; macOS only allows that with the Accessibility permission. Without
//! it the guard simply stays off and Settings says how to allow it.
//!
//! The key mapping and the chat app matching below are plain functions, so they're
//! tested on every system.

/// Windows modifier flags, as `settings::shortcut_keys` gives them.
const WIN_ALT: u32 = 1;
const WIN_CTRL: u32 = 2;
const WIN_SHIFT: u32 = 4;
const WIN_LOGO: u32 = 8;

/// Carbon modifier masks (Events.h).
const CMD_KEY: u32 = 1 << 8;
const SHIFT_KEY: u32 = 1 << 9;
const OPTION_KEY: u32 = 1 << 11;
const CONTROL_KEY: u32 = 1 << 12;

/// The macOS virtual key code (ANSI layout) for a Windows virtual key.
fn mac_keycode(vk: u32) -> Option<u32> {
    // kVK_ANSI_A ... kVK_ANSI_Z, in alphabetical order.
    const LETTERS: [u32; 26] = [
        0x00, 0x0B, 0x08, 0x02, 0x0E, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2E, 0x2D, 0x1F,
        0x23, 0x0C, 0x0F, 0x01, 0x11, 0x20, 0x09, 0x0D, 0x07, 0x10, 0x06,
    ];
    // kVK_ANSI_0 ... kVK_ANSI_9.
    const DIGITS: [u32; 10] = [0x1D, 0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1A, 0x1C, 0x19];
    match vk {
        0x41..=0x5A => Some(LETTERS[(vk - 0x41) as usize]),
        0x30..=0x39 => Some(DIGITS[(vk - 0x30) as usize]),
        0x20 => Some(0x31), // kVK_Space
        _ => None,
    }
}

/// Carbon key code and modifiers for Windows modifier flags and a virtual key.
pub fn mac_hotkey(mods: u32, vk: u32) -> Option<(u32, u32)> {
    let code = mac_keycode(vk)?;
    let carbon = [
        (WIN_CTRL, CONTROL_KEY),
        (WIN_ALT, OPTION_KEY),
        (WIN_SHIFT, SHIFT_KEY),
        (WIN_LOGO, CMD_KEY),
    ]
    .into_iter()
    .filter(|(win, _)| mods & win != 0)
    .fold(0, |acc, (_, mac)| acc | mac);
    Some((code, carbon))
}

/// How a Mac shows the shortcut: modifier symbols in Apple's order, then the key
/// ("⌃⌥K", "⌃⇧Space").
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn mac_shortcut_label(mods: u32, vk: u32) -> String {
    let mut label: String = [
        (WIN_CTRL, '\u{2303}'),
        (WIN_ALT, '\u{2325}'),
        (WIN_SHIFT, '\u{21E7}'),
        (WIN_LOGO, '\u{2318}'),
    ]
    .into_iter()
    .filter(|(win, _)| mods & win != 0)
    .map(|(_, symbol)| symbol)
    .collect();
    match vk {
        0x20 => label.push_str("Space"),
        0x30..=0x39 | 0x41..=0x5A => label.push(char::from(vk as u8)),
        _ => label.push('?'),
    }
    label
}

/// The chat app an application is, as its preset id, by its bundle identifier; for a
/// browser, by its front window's title (asked for only then).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn mac_chat_app(
    bundle_id: &str,
    title: impl FnOnce() -> Option<String>,
) -> Option<&'static str> {
    const BROWSERS: [&str; 9] = [
        "com.google.chrome",
        "com.microsoft.edgemac",
        "org.mozilla.firefox",
        "com.brave.browser",
        "com.operasoftware.opera",
        "com.vivaldi.vivaldi",
        "company.thebrowser.browser",
        "app.zen-browser.zen",
        "com.apple.safari",
    ];
    let id = bundle_id.to_ascii_lowercase();
    match id.as_str() {
        // Discord, Discord PTB, Discord Canary.
        i if i.starts_with("com.hnc.discord") => Some("discord"),
        "com.tinyspeck.slackmacgap" => Some("slack"),
        // The native app and the older desktop app.
        "net.whatsapp.whatsapp" | "desktop.whatsapp" => Some("whatsapp"),
        // Telegram for macOS and Telegram Desktop.
        "ru.keepcoder.telegram" | "com.tdesktop.telegram" | "org.telegram.desktop" => {
            Some("telegram")
        }
        "org.whispersystems.signal-desktop" => Some("signal"),
        b if BROWSERS.contains(&b) => super::chat_tab(&title()?),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
pub use imp::*;

#[cfg(target_os = "macos")]
mod imp {
    use std::cell::Cell;
    use std::ffi::c_void;
    use std::path::PathBuf;
    use std::ptr::null_mut;
    use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicU8, Ordering};
    use std::sync::{Mutex, Once, OnceLock};

    use objc2::rc::autoreleasepool;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};
    use objc2_foundation::NSString;

    use super::{mac_chat_app, mac_hotkey};
    use crate::platform::{InputEvent, PasteTarget};

    type OSStatus = i32;
    type Ref = *mut c_void;

    #[repr(C)]
    struct EventTypeSpec {
        event_class: u32,
        event_kind: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct EventHotKeyID {
        signature: u32,
        id: u32,
    }

    type EventHandler = unsafe extern "C" fn(Ref, Ref, *mut c_void) -> OSStatus;
    type TapCallback = unsafe extern "C" fn(Ref, u32, Ref, *mut c_void) -> Ref;

    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        fn GetApplicationEventTarget() -> Ref;
        fn InstallEventHandler(
            target: Ref,
            handler: EventHandler,
            count: usize,
            types: *const EventTypeSpec,
            user_data: *mut c_void,
            out: *mut Ref,
        ) -> OSStatus;
        fn RegisterEventHotKey(
            code: u32,
            modifiers: u32,
            id: EventHotKeyID,
            target: Ref,
            options: u32,
            out: *mut Ref,
        ) -> OSStatus;
        fn UnregisterEventHotKey(hot_key: Ref) -> OSStatus;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventTapCreate(
            tap: u32,
            place: u32,
            options: u32,
            events: u64,
            callback: TapCallback,
            user_info: *mut c_void,
        ) -> Ref;
        fn CGEventTapEnable(tap: Ref, enable: bool);
        fn CGEventGetFlags(event: Ref) -> u64;
        fn CGEventGetIntegerValueField(event: Ref, field: u32) -> i64;
        fn CGEventSetIntegerValueField(event: Ref, field: u32, value: i64);
        fn CGEventCreateKeyboardEvent(source: Ref, key: u16, down: bool) -> Ref;
        fn CGEventSetFlags(event: Ref, flags: u64);
        fn CGEventPost(tap: u32, event: Ref);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFRunLoopCommonModes: *const c_void;
        static kCFBooleanTrue: *const c_void;
        static kCFTypeDictionaryKeyCallBacks: u8;
        static kCFTypeDictionaryValueCallBacks: u8;
        fn CFMachPortCreateRunLoopSource(alloc: Ref, port: Ref, order: isize) -> Ref;
        fn CFRunLoopGetCurrent() -> Ref;
        fn CFRunLoopAddSource(run_loop: Ref, source: Ref, mode: *const c_void);
        fn CFRunLoopRun();
        fn CFDictionaryCreate(
            alloc: Ref,
            keys: *const *const c_void,
            values: *const *const c_void,
            count: isize,
            key_callbacks: *const u8,
            value_callbacks: *const u8,
        ) -> Ref;
        fn CFGetTypeID(cf: *const c_void) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        static kAXTrustedCheckOptionPrompt: *const c_void;
        fn AXIsProcessTrusted() -> u8;
        fn AXIsProcessTrustedWithOptions(options: Ref) -> u8;
        fn AXUIElementCreateApplication(pid: i32) -> Ref;
        fn AXUIElementSetMessagingTimeout(element: Ref, seconds: f32) -> i32;
        fn AXUIElementCopyAttributeValue(element: Ref, attribute: Ref, value: *mut Ref) -> i32;
    }

    const K_EVENT_CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
    const K_EVENT_HOT_KEY_PRESSED: u32 = 5;
    const SIGNATURE: u32 = u32::from_be_bytes(*b"KMPT");

    const SESSION_TAP: u32 = 1; // kCGSessionEventTap
    const HID_TAP: u32 = 0; // kCGHIDEventTap
    const HEAD_INSERT: u32 = 0; // kCGHeadInsertEventTap
    const TAP_DEFAULT: u32 = 0; // kCGEventTapOptionDefault: may drop events
    const KEY_DOWN: u32 = 10; // kCGEventKeyDown
    const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
    const TAP_DISABLED_BY_USER: u32 = 0xFFFF_FFFF;
    const FIELD_AUTOREPEAT: u32 = 8; // kCGKeyboardEventAutorepeat
    const FIELD_KEYCODE: u32 = 9; // kCGKeyboardEventKeycode
    const FIELD_USER_DATA: u32 = 42; // kCGEventSourceUserData
    const FLAG_CONTROL: u64 = 0x0004_0000;
    const FLAG_OPTION: u64 = 0x0008_0000;
    const FLAG_COMMAND: u64 = 0x0010_0000;
    const KEY_V: u16 = 0x09;
    /// Marks the Cmd+V Kompacter presses itself, so the guard lets it by.
    const OURS: i64 = 0x4B4D_5054;

    type Callback = Box<dyn Fn(InputEvent) + Send + Sync>;
    static ON_INPUT: OnceLock<Callback> = OnceLock::new();
    static GUARD_ON: AtomicBool = AtomicBool::new(false);
    static GUARD_LIMIT: AtomicU64 = AtomicU64::new(u64::MAX);
    static TAP: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
    static TAP_THREAD: Once = Once::new();
    /// The permission note last reported: 0 none yet, 1 needed, 2 not needed.
    static NOTE: AtomicU8 = AtomicU8::new(0);
    /// `set_guard` has been called before (the first call is the app starting).
    static GUARD_SET: AtomicBool = AtomicBool::new(false);
    /// Clipboard change count and limit → the too-big files on it (empty: none).
    static GUARD_SEEN: Mutex<Option<(u32, u64, Vec<PathBuf>)>> = Mutex::new(None);

    thread_local! {
        /// The registered hot key (main thread only).
        static HOT_KEY: Cell<Ref> = const { Cell::new(null_mut()) };
    }

    fn emit(e: InputEvent) {
        if let Some(f) = ON_INPUT.get() {
            f(e);
        }
    }

    /// Install the hot key handler. Call on the main thread; `on_event` runs there too
    /// (for the shortcut) or on the paste guard's thread; keep it quick.
    pub fn start_input(on_event: impl Fn(InputEvent) + Send + Sync + 'static) {
        if MainThreadMarker::new().is_none() || ON_INPUT.set(Box::new(on_event)).is_err() {
            return;
        }
        let spec = EventTypeSpec {
            event_class: K_EVENT_CLASS_KEYBOARD,
            event_kind: K_EVENT_HOT_KEY_PRESSED,
        };
        let mut handler = null_mut();
        let status = unsafe {
            InstallEventHandler(
                GetApplicationEventTarget(),
                hot_key_pressed,
                1,
                &spec,
                null_mut(),
                &mut handler,
            )
        };
        if status != 0 {
            eprintln!("Kompacter: couldn't listen for the shortcut ({status})");
        }
    }

    unsafe extern "C" fn hot_key_pressed(_: Ref, _: Ref, _: *mut c_void) -> OSStatus {
        // Kompacter registers a single hot key, so every press is the shortcut.
        emit(InputEvent::Shortcut);
        0
    }

    /// Register the global shortcut: (Windows modifiers, virtual key), or None for no
    /// shortcut. The answer comes back as `InputEvent::ShortcutTaken`. Main thread only.
    pub fn set_shortcut(key: Option<(u32, u32)>) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        let old = HOT_KEY.with(|k| k.replace(null_mut()));
        if !old.is_null() {
            unsafe { UnregisterEventHotKey(old) };
        }
        let Some((code, modifiers)) = key.and_then(|(mods, vk)| mac_hotkey(mods, vk)) else {
            emit(InputEvent::ShortcutTaken(false));
            return;
        };
        let mut hot_key = null_mut();
        let id = EventHotKeyID {
            signature: SIGNATURE,
            id: 1,
        };
        let status = unsafe {
            RegisterEventHotKey(
                code,
                modifiers,
                id,
                GetApplicationEventTarget(),
                0,
                &mut hot_key,
            )
        };
        if status == 0 {
            HOT_KEY.with(|k| k.set(hot_key));
        }
        emit(InputEvent::ShortcutTaken(status != 0));
    }

    fn trusted() -> bool {
        unsafe { AXIsProcessTrusted() != 0 }
    }

    /// Ask macOS to show its "allow Accessibility" prompt (once per launch of the app,
    /// macOS decides; it never blocks).
    fn ask_for_trust() {
        unsafe {
            let keys = [kAXTrustedCheckOptionPrompt];
            let values = [kCFBooleanTrue];
            let options = CFDictionaryCreate(
                null_mut(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            if !options.is_null() {
                AXIsProcessTrustedWithOptions(options);
                CFRelease(options);
            }
        }
    }

    fn note(needed: bool) {
        let v = if needed { 1 } else { 2 };
        if NOTE.swap(v, Ordering::SeqCst) != v {
            emit(InputEvent::GuardNeedsPermission(needed));
        }
    }

    /// Turn the paste guard on or off; `discord_limit` is the Discord limit in bytes.
    /// Other chat apps use their own preset's limit.
    pub fn set_guard(on: bool, discord_limit: u64) {
        GUARD_LIMIT.store(discord_limit, Ordering::SeqCst);
        let was = GUARD_ON.swap(on, Ordering::SeqCst);
        let starting = !GUARD_SET.swap(true, Ordering::SeqCst);
        let tap = TAP.load(Ordering::SeqCst);
        if !on {
            note(false);
            if !tap.is_null() {
                unsafe { CGEventTapEnable(tap, false) };
            }
            return;
        }
        if trusted() {
            note(false);
        } else {
            // Switched on just now (not the app starting with it on): ask.
            if !was && !starting {
                ask_for_trust();
            }
            note(true);
        }
        if tap.is_null() {
            TAP_THREAD.call_once(|| {
                std::thread::spawn(run_tap);
            });
        } else {
            unsafe { CGEventTapEnable(tap, true) };
        }
    }

    /// The paste guard's thread: waits for the permission, then runs the event tap.
    fn run_tap() {
        loop {
            if GUARD_ON.load(Ordering::SeqCst) && trusted() {
                let tap = unsafe {
                    CGEventTapCreate(
                        SESSION_TAP,
                        HEAD_INSERT,
                        TAP_DEFAULT,
                        1 << KEY_DOWN,
                        on_key,
                        null_mut(),
                    )
                };
                if !tap.is_null() {
                    TAP.store(tap, Ordering::SeqCst);
                    note(false);
                    unsafe {
                        let source = CFMachPortCreateRunLoopSource(null_mut(), tap, 0);
                        CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
                        CGEventTapEnable(tap, GUARD_ON.load(Ordering::SeqCst));
                        CFRunLoopRun();
                    }
                    return;
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }

    unsafe extern "C" fn on_key(_: Ref, kind: u32, event: Ref, _: *mut c_void) -> Ref {
        if kind == TAP_DISABLED_BY_TIMEOUT || kind == TAP_DISABLED_BY_USER {
            let tap = TAP.load(Ordering::SeqCst);
            if GUARD_ON.load(Ordering::SeqCst) && !tap.is_null() {
                unsafe { CGEventTapEnable(tap, true) };
            }
            return event;
        }
        if kind != KEY_DOWN || event.is_null() || !GUARD_ON.load(Ordering::SeqCst) {
            return event;
        }
        unsafe {
            let flags = CGEventGetFlags(event);
            if CGEventGetIntegerValueField(event, FIELD_KEYCODE) != i64::from(KEY_V)
                || flags & FLAG_COMMAND == 0
                || flags & (FLAG_CONTROL | FLAG_OPTION) != 0
                || CGEventGetIntegerValueField(event, FIELD_USER_DATA) == OURS
            {
                return event;
            }
            let repeat = CGEventGetIntegerValueField(event, FIELD_AUTOREPEAT) != 0;
            match autoreleasepool(|_| held_paste()) {
                Some((files, pid, app)) => {
                    // A held key repeats: hold those back too, but open the bar once.
                    if !repeat {
                        emit(InputEvent::Guard {
                            files,
                            target: PasteTarget::App(pid),
                            app,
                        });
                    }
                    null_mut() // The chat app never sees this paste.
                }
                None => event,
            }
        }
    }

    /// Cmd+V right now would paste files that are too big into a chat app: the files,
    /// the app's process id and its preset id.
    fn held_paste() -> Option<(Vec<PathBuf>, i32, &'static str)> {
        let front = NSWorkspace::sharedWorkspace().frontmostApplication()?;
        let pid = front.processIdentifier();
        if pid <= 0 || pid as u32 == std::process::id() {
            return None;
        }
        let bundle = front.bundleIdentifier()?.to_string();
        let app = mac_chat_app(&bundle, || window_title(pid))?;
        let limit = match app {
            "discord" => GUARD_LIMIT.load(Ordering::SeqCst),
            id => kompacter_core::size::preset(id)?.bytes,
        };
        let files = too_big_on_clipboard(limit);
        (!files.is_empty()).then_some((files, pid, app))
    }

    /// The files on the clipboard if any of them is over `limit` (else empty).
    fn too_big_on_clipboard(limit: u64) -> Vec<PathBuf> {
        let seq = crate::platform::clipboard_seq();
        let mut seen = GUARD_SEEN.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((s, l, files)) = seen.as_ref() {
            if *s == seq && *l == limit {
                return files.clone();
            }
        }
        let files = crate::platform::clipboard_files();
        let over = files
            .iter()
            .any(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() > limit));
        let files = if over { files } else { Vec::new() };
        *seen = Some((seq, limit, files.clone()));
        files
    }

    /// The title of an app's focused window (a browser's tab title), through the
    /// Accessibility API the paste guard is already allowed to use.
    fn window_title(pid: i32) -> Option<String> {
        let focused = NSString::from_str("AXFocusedWindow");
        let title_key = NSString::from_str("AXTitle");
        unsafe {
            let app = AXUIElementCreateApplication(pid);
            if app.is_null() {
                return None;
            }
            // Never keep a key press waiting on a busy app for long.
            AXUIElementSetMessagingTimeout(app, 0.25);
            let mut window = null_mut();
            let found = AXUIElementCopyAttributeValue(
                app,
                objc2::rc::Retained::as_ptr(&focused) as Ref,
                &mut window,
            ) == 0;
            CFRelease(app);
            if !found || window.is_null() {
                return None;
            }
            let mut title = null_mut();
            let found = AXUIElementCopyAttributeValue(
                window,
                objc2::rc::Retained::as_ptr(&title_key) as Ref,
                &mut title,
            ) == 0;
            CFRelease(window);
            if !found || title.is_null() {
                return None;
            }
            let text = (CFGetTypeID(title) == CFStringGetTypeID())
                .then(|| (*(title as *const NSString)).to_string());
            CFRelease(title);
            text
        }
    }

    /// The process id of the app in front.
    pub fn front_app() -> Option<i32> {
        let front = NSWorkspace::sharedWorkspace().frontmostApplication()?;
        Some(front.processIdentifier()).filter(|pid| *pid > 0)
    }

    /// Bring an app back to the front (Kompacter is in front, so macOS allows it).
    pub fn activate_app(pid: i32) {
        if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
            app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
        }
    }

    /// Press Cmd+V for the app in front (marked as Kompacter's, so the guard lets it by).
    pub fn send_paste() {
        unsafe {
            for down in [true, false] {
                let event = CGEventCreateKeyboardEvent(null_mut(), KEY_V, down);
                if event.is_null() {
                    return;
                }
                CGEventSetFlags(event, FLAG_COMMAND);
                CGEventSetIntegerValueField(event, FIELD_USER_DATA, OURS);
                CGEventPost(HID_TAP, event);
                CFRelease(event);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::settings::{shortcut_keys, SHORTCUTS};

    #[test]
    fn settings_shortcuts_become_carbon_hot_keys() {
        let keys: Vec<Option<(u32, u32)>> = SHORTCUTS
            .iter()
            .map(|(id, _)| shortcut_keys(id).and_then(|(m, vk)| mac_hotkey(m, vk)))
            .collect();
        assert_eq!(
            keys,
            [
                None,
                Some((0x28, CONTROL_KEY | OPTION_KEY)),
                Some((0x28, CONTROL_KEY | SHIFT_KEY)),
                Some((0x31, CONTROL_KEY | SHIFT_KEY)),
            ]
        );
        // The paste guard's V, and the Windows key as Command.
        assert_eq!(mac_hotkey(WIN_LOGO, 0x56), Some((0x09, CMD_KEY)));
        assert_eq!(mac_hotkey(0, 0x30), Some((0x1D, 0)));
        assert_eq!(mac_hotkey(WIN_CTRL, 0x70), None);
    }

    #[test]
    fn shortcuts_read_like_a_mac_menu() {
        let labels: Vec<String> = SHORTCUTS
            .iter()
            .filter_map(|(id, _)| shortcut_keys(id))
            .map(|(m, vk)| mac_shortcut_label(m, vk))
            .collect();
        assert_eq!(labels, ["⌃⌥K", "⌃⇧K", "⌃⇧Space"]);
        assert_eq!(mac_shortcut_label(WIN_LOGO | WIN_SHIFT, 0x56), "⇧⌘V");
    }

    #[test]
    fn finds_chat_apps_by_bundle_or_browser_title() {
        let no_title = || -> Option<String> { panic!("only browsers need a title") };
        assert_eq!(mac_chat_app("com.hnc.Discord", no_title), Some("discord"));
        assert_eq!(
            mac_chat_app("com.hnc.DiscordPTB", no_title),
            Some("discord")
        );
        assert_eq!(
            mac_chat_app("com.hnc.DiscordCanary", no_title),
            Some("discord")
        );
        assert_eq!(
            mac_chat_app("com.tinyspeck.slackmacgap", no_title),
            Some("slack")
        );
        assert_eq!(
            mac_chat_app("net.whatsapp.WhatsApp", no_title),
            Some("whatsapp")
        );
        assert_eq!(
            mac_chat_app("ru.keepcoder.Telegram", no_title),
            Some("telegram")
        );
        assert_eq!(
            mac_chat_app("com.tdesktop.Telegram", no_title),
            Some("telegram")
        );
        assert_eq!(
            mac_chat_app("org.whispersystems.signal-desktop", no_title),
            Some("signal")
        );
        let tab = |t: &str| {
            let t = t.to_string();
            move || Some(t)
        };
        assert_eq!(
            mac_chat_app("com.google.Chrome", tab("#general | Server - Discord")),
            Some("discord")
        );
        assert_eq!(
            mac_chat_app("com.apple.Safari", tab("WhatsApp")),
            Some("whatsapp")
        );
        assert_eq!(mac_chat_app("com.apple.Safari", || None), None);
        assert_eq!(mac_chat_app("com.apple.Safari", tab("Apple")), None);
        // Not a browser: its window title doesn't count.
        assert_eq!(mac_chat_app("com.apple.TextEdit", no_title), None);
        assert_eq!(mac_chat_app("com.microsoft.Word", no_title), None);
    }
}
