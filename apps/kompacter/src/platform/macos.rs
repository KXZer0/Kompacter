//! Native file clipboard and Launch Services events for the winit application.

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, ProtocolObject, Sel};
use objc2::{ffi, sel, ClassType, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSApplicationDelegateReply, NSPasteboard};
use objc2_foundation::{NSArray, NSFileManager, NSObjectProtocol, NSString, NSURL};
use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

fn local_path(url: &NSURL) -> Option<PathBuf> {
    if !url.isFileURL() {
        return None;
    }
    if let Some(host) = url.host() {
        let host = host.to_string();
        if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
            return None;
        }
    }
    // Darwin's PATH_MAX includes the terminating NUL. The checked API also
    // handles file URLs with an empty or otherwise unrepresentable path.
    let mut buffer = [0_u8; 1024];
    let bytes = NonNull::from(&mut buffer).cast();
    // SAFETY: The buffer is writable for the supplied length.
    if !unsafe { url.getFileSystemRepresentation_maxLength(bytes, buffer.len()) } {
        return None;
    }
    CStr::from_bytes_until_nul(&buffer)
        .ok()
        .and_then(filesystem_path)
}

fn filesystem_path(bytes: &CStr) -> Option<PathBuf> {
    let path = PathBuf::from(std::ffi::OsString::from_vec(bytes.to_bytes().to_vec()));
    path.is_absolute().then_some(path)
}

pub fn clipboard_files() -> Vec<PathBuf> {
    autoreleasepool(|_| {
        let board = NSPasteboard::generalPasteboard();
        let classes = NSArray::from_slice(&[NSURL::class()]);
        // SAFETY: NSURL conforms to NSPasteboardReading and no options are supplied.
        unsafe { board.readObjectsForClasses_options(&classes, None) }
            .into_iter()
            .flat_map(|urls| {
                urls.to_vec().into_iter().filter_map(|url| {
                    url.downcast::<NSURL>()
                        .ok()
                        .and_then(|url| local_path(&url))
                })
            })
            .collect()
    })
}

pub fn copy_files(paths: &[PathBuf]) -> Result<(), String> {
    if paths.is_empty() {
        return Err("there are no files to copy".into());
    }
    autoreleasepool(|_| {
        let urls = paths
            .iter()
            .map(|path| file_url(path).map(ProtocolObject::from_retained))
            .collect::<Result<Vec<_>, String>>()?;
        let board = NSPasteboard::generalPasteboard();
        board.clearContents();
        if board.writeObjects(&NSArray::from_retained_slice(&urls)) {
            Ok(())
        } else {
            Err("couldn't put the files on the clipboard".into())
        }
    })
}

fn file_url(path: &Path) -> Result<Retained<NSURL>, String> {
    let absolute = std::path::absolute(path).map_err(|e| e.to_string())?;
    let bytes = CString::new(absolute.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    // SAFETY: The initializer takes a valid, terminated path string.
    Ok(unsafe {
        NSURL::fileURLWithFileSystemRepresentation_isDirectory_relativeToURL(
            NonNull::from(bytes.as_c_str()).cast(),
            absolute.is_dir(),
            None,
        )
    })
}

/// Move a file to the Trash (Finder's "Put Back" works), never delete it outright.
pub fn recycle(path: &Path) -> Result<(), String> {
    autoreleasepool(|_| {
        let url = file_url(path)?;
        NSFileManager::defaultManager()
            .trashItemAtURL_resultingItemURL_error(&url, None)
            .map_err(|e| e.localizedDescription().to_string())
    })
}

pub fn clipboard_seq() -> u32 {
    autoreleasepool(|_| NSPasteboard::generalPasteboard().changeCount() as u32)
}

pub fn show_in_folder(file: &Path) {
    let _ = std::process::Command::new("/usr/bin/open")
        .arg("-R")
        .arg(file)
        .spawn();
}

type Activation = Box<dyn Fn(Vec<PathBuf>)>;

thread_local! {
    static ON_ACTIVATION: RefCell<Option<Activation>> = const { RefCell::new(None) };
}

fn emit_activation(files: Vec<PathBuf>) {
    ON_ACTIVATION.with(|callback| {
        if let Some(callback) = callback.borrow().as_ref() {
            callback(files);
        }
    });
}

extern "C-unwind" fn reopen(_: &AnyObject, _: Sel, _: &NSApplication, _: Bool) -> Bool {
    emit_activation(Vec::new());
    Bool::NO // Kompacter handles reopening its own window.
}

extern "C-unwind" fn open_urls(_: &AnyObject, _: Sel, _: &NSApplication, urls: &NSArray<NSURL>) {
    let files: Vec<_> = urls
        .to_vec()
        .iter()
        .filter_map(|url| local_path(url))
        .collect();
    if !files.is_empty() {
        emit_activation(files);
    }
}

extern "C-unwind" fn open_files(
    _: &AnyObject,
    _: Sel,
    app: &NSApplication,
    names: &NSArray<NSString>,
) {
    let files: Vec<_> = names
        .to_vec()
        .iter()
        .filter_map(|name| {
            // SAFETY: Foundation owns this terminated path string for the
            // lifetime of the NSString and the enclosing AppKit autorelease pool.
            let path = unsafe { CStr::from_ptr(name.fileSystemRepresentation().as_ptr()) };
            filesystem_path(path)
        })
        .collect();
    let reply = if files.is_empty() {
        NSApplicationDelegateReply::Failure
    } else {
        emit_activation(files);
        NSApplicationDelegateReply::Success
    };
    app.replyToOpenOrPrint(reply);
}

/// Extend the winit delegate before AppKit handles its first Launch Services event.
/// Winit still owns the delegate and handles its lifecycle and event loop methods.
/// Call from the main thread once Slint has created the winit event loop.
pub fn install_activation(callback: impl Fn(Vec<PathBuf>) + 'static) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(delegate) = app.delegate() else {
        return false;
    };
    let object: &AnyObject = (*delegate).as_ref();
    let class = std::ptr::from_ref::<AnyClass>(object.class()).cast_mut();
    let reopen_types = if cfg!(target_arch = "aarch64") {
        c"B@:@B"
    } else {
        c"c@:@c"
    };
    // SAFETY: These are the documented delegate signatures. class_addMethod
    // leaves existing delegate methods in place when a selector is present.
    let installed = unsafe {
        [
            ffi::class_addMethod(
                class,
                sel!(applicationShouldHandleReopen:hasVisibleWindows:),
                std::mem::transmute::<*const (), Imp>(reopen as *const ()),
                reopen_types.as_ptr(),
            ),
            ffi::class_addMethod(
                class,
                sel!(application:openURLs:),
                std::mem::transmute::<*const (), Imp>(open_urls as *const ()),
                c"v@:@@".as_ptr(),
            ),
            ffi::class_addMethod(
                class,
                sel!(application:openFiles:),
                std::mem::transmute::<*const (), Imp>(open_files as *const ()),
                c"v@:@@".as_ptr(),
            ),
        ]
    };
    ON_ACTIVATION.with(|slot| *slot.borrow_mut() = Some(Box::new(callback)));
    // AppKit caches delegate capabilities when the delegate is assigned.
    app.setDelegate(None);
    app.setDelegate(Some(&delegate));
    installed.iter().all(|result| result.as_bool())
}

/// The top right of the free area (below the menu bar, beside the Dock) of the screen
/// under the mouse, in winit's physical pixels: the panel hangs below the menu-bar icon.
pub fn anchor() -> Option<super::Anchor> {
    use objc2_app_kit::{NSEvent, NSScreen};
    let mtm = MainThreadMarker::new()?;
    let screens = NSScreen::screens(mtm);
    // Window coordinates count down from the top of the screen with the menu bar.
    let main = screens.firstObject()?;
    let mouse = NSEvent::mouseLocation();
    let screen = screens
        .iter()
        .find(|s| {
            let f = s.frame();
            (f.origin.x..f.origin.x + f.size.width).contains(&mouse.x)
                && (f.origin.y..f.origin.y + f.size.height).contains(&mouse.y)
        })
        .unwrap_or_else(|| main.clone());
    let free = screen.visibleFrame();
    let scale = screen.backingScaleFactor();
    if free.size.width <= 0.0 || free.size.height <= 0.0 || scale <= 0.0 {
        return None;
    }
    let top = main.frame().size.height - (free.origin.y + free.size.height);
    let right = free.origin.x + free.size.width;
    Some(super::Anchor {
        right: (right * scale).round() as i32,
        edge: (top * scale).round() as i32,
        scale,
        from_top: true,
    })
}

/// Give the frameless panel the system's rounded corners and shadow: a titled window
/// whose content fills it, with the title bar and its buttons hidden.
pub fn style_panel(view: NonNull<std::ffi::c_void>) {
    use objc2_app_kit::{NSView, NSWindowButton, NSWindowStyleMask, NSWindowTitleVisibility};
    // SAFETY: winit hands out the panel's live NSView, used here on the main thread.
    let view: &NSView = unsafe { view.cast().as_ref() };
    let Some(window) = view.window() else { return };
    let mask = NSWindowStyleMask::Titled | NSWindowStyleMask::FullSizeContentView;
    if !window.styleMask().contains(mask) {
        window.setStyleMask(window.styleMask() | mask);
    }
    window.setTitlebarAppearsTransparent(true);
    window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    for button in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ] {
        if let Some(button) = window.standardWindowButton(button) {
            button.setHidden(true);
        }
    }
    window.setHasShadow(true);
    window.invalidateShadow();
}

pub fn focus_application() {
    if let Some(mtm) = MainThreadMarker::new() {
        let app = NSApplication::sharedApplication(mtm);
        if app.respondsToSelector(sel!(activate)) {
            app.activate();
        } else {
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_local_encoded_file_urls() {
        autoreleasepool(|_| {
            for input in [
                "file:///tmp/Finder%20file%20%231.mov",
                "file://localhost/tmp/Finder%20file%20%231.mov",
            ] {
                let url = NSURL::URLWithString(&NSString::from_str(input)).unwrap();
                assert_eq!(
                    local_path(&url),
                    Some(PathBuf::from("/tmp/Finder file #1.mov"))
                );
            }
            for input in [
                "https://example.com/file.mov",
                "file://remote.example.com/tmp/file.mov",
            ] {
                let url = NSURL::URLWithString(&NSString::from_str(input)).unwrap();
                assert_eq!(local_path(&url), None);
            }
        });
    }
}
