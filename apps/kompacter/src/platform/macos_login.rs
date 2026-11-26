//! Open at login on macOS. On macOS 13 and later Kompacter registers itself as a login
//! item (`SMAppService.mainAppService`), listed under System Settings → General → Login
//! Items. Older systems get a per-user LaunchAgent that starts the app with `--tray`.
//! Only the app bundle registers; a bare `cargo run` binary never does.

use std::path::{Path, PathBuf};

use objc2::runtime::AnyClass;
use objc2_service_management::{SMAppService, SMAppServiceStatus};

use super::StartEntry;

const LABEL: &str = "io.github.KXZer0.Kompacter";

/// This program, when it is the executable of an `.app` bundle.
fn bundled_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let is_bundle = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app";
    is_bundle.then_some(exe)
}

pub fn supported() -> bool {
    bundled_exe().is_some()
}

/// The login item service, on systems that have it (macOS 13+).
fn service() -> Option<objc2::rc::Retained<SMAppService>> {
    AnyClass::get(c"SMAppService")?;
    // SAFETY: The class exists (checked above); this is a plain class getter.
    Some(unsafe { SMAppService::mainAppService() })
}

fn agent_file() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(format!("Library/LaunchAgents/{LABEL}.plist")))
}

pub fn set(on: bool) {
    let Some(exe) = bundled_exe() else { return };
    if let Some(service) = service() {
        // SAFETY: SMAppService methods with no arguments, on a live instance.
        let status = unsafe { service.status() };
        let result = if on && status != SMAppServiceStatus::Enabled {
            unsafe { service.registerAndReturnError() }
        } else if !on && status != SMAppServiceStatus::NotRegistered {
            unsafe { service.unregisterAndReturnError() }
        } else {
            Ok(())
        };
        if let Err(error) = result {
            eprintln!("Kompacter login item: {}", error.localizedDescription());
        }
        // Switched off in System Settings earlier: only the user can switch it back
        // on there, so take them to it.
        if on && unsafe { service.status() } == SMAppServiceStatus::RequiresApproval {
            unsafe { SMAppService::openSystemSettingsLoginItems() };
        }
        // A LaunchAgent left from before a macOS upgrade would start a second copy.
        if let Some(file) = agent_file() {
            let _ = std::fs::remove_file(file);
        }
        return;
    }
    let Some(file) = agent_file() else { return };
    let result = if on {
        let text = agent_plist(&exe);
        if std::fs::read_to_string(&file).is_ok_and(|old| old == text) {
            return;
        }
        file.parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&file, text))
    } else {
        match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    };
    if let Err(error) = result {
        eprintln!("Kompacter login item: {error}");
    }
}

pub fn state() -> StartEntry {
    let Some(exe) = bundled_exe() else {
        return StartEntry::Absent;
    };
    if let Some(service) = service() {
        // SAFETY: A getter on a live instance.
        return match unsafe { service.status() } {
            SMAppServiceStatus::Enabled => StartEntry::Here,
            // Registered, then switched off by the user in System Settings.
            SMAppServiceStatus::RequiresApproval => StartEntry::Disabled,
            _ => StartEntry::Absent,
        };
    }
    match agent_file().map(std::fs::read_to_string) {
        Some(Ok(text)) if text == agent_plist(&exe) => StartEntry::Here,
        Some(Ok(_)) => StartEntry::Elsewhere,
        _ => StartEntry::Absent,
    }
}

/// A LaunchAgent that starts `exe --tray` at login.
fn agent_plist(exe: &Path) -> String {
    let exe = xml_escape(&exe.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>--tray</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#
    )
}

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}
