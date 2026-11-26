//! Start with the system on Linux: an XDG autostart entry
//! (`~/.config/autostart/io.github.KXZer0.Kompacter.desktop`) that runs Kompacter with
//! `--tray`, so it starts quietly in the tray.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

const FILE_NAME: &str = "io.github.KXZer0.Kompacter.desktop";
/// Where the .deb installs the program; `/usr/bin/kompacter` links to it.
const DEB_PROGRAM: &str = "/usr/lib/kompacter/Kompacter";

use super::StartEntry as State;

/// Switch starting with the system on or off for this copy of Kompacter.
pub fn set(on: bool) {
    let Some(dir) = dir() else { return };
    let result = if on {
        match exec_line() {
            Some(exec) => enable(&dir, &exec),
            None => Err(std::io::Error::other("no usable path to start Kompacter")),
        }
    } else {
        disable(&dir)
    };
    if let Err(error) = result {
        eprintln!("Kompacter autostart: {error}");
    }
}

/// The autostart entry as it is now.
pub fn state() -> State {
    // Without a usable path of our own, any entry is someone else's.
    dir().map_or(State::Absent, |dir| {
        state_in(&dir, &exec_line().unwrap_or_default())
    })
}

/// `$XDG_CONFIG_HOME/autostart`, or `~/.config/autostart`.
fn dir() -> Option<PathBuf> {
    dir_from(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

fn dir_from(xdg_config_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    // The spec says a relative XDG_CONFIG_HOME is invalid and to be ignored.
    xdg_config_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|h| h.join(".config"))
        })
        .map(|config| config.join("autostart"))
}

/// The `Exec=` value that starts this copy quietly.
fn exec_line() -> Option<String> {
    let program = program(
        std::env::var_os("APPIMAGE"),
        std::env::current_exe().ok().as_deref(),
    )?;
    exec_for(&program)
}

/// What to start: the AppImage file itself (the running program lives in a temporary
/// mount), `kompacter` on the PATH for the .deb, or this program's own path.
fn program(appimage: Option<OsString>, exe: Option<&Path>) -> Option<PathBuf> {
    if let Some(image) = appimage.filter(|a| !a.is_empty()) {
        return std::path::absolute(image).ok();
    }
    let exe = exe?;
    if exe == Path::new(DEB_PROGRAM) {
        return Some(PathBuf::from("kompacter"));
    }
    std::path::absolute(exe).ok()
}

/// `program --tray`, quoted per the Desktop Entry spec. `None` for a path a desktop
/// file can't hold (not UTF-8, or with control characters).
fn exec_for(program: &Path) -> Option<String> {
    let program = program.to_str()?;
    if program.is_empty() || program.chars().any(char::is_control) {
        return None;
    }
    const RESERVED: &[char] = &[
        ' ', '\t', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#', '(', ')', '`',
    ];
    let mut arg = String::new();
    if program.contains(RESERVED) {
        // Inside quotes `"`, `` ` ``, `$` and `\` take a backslash; then the whole
        // value is a desktop-file string, where every backslash is doubled again.
        arg.push('"');
        for c in program.chars() {
            match c {
                '"' | '`' | '$' => arg.push_str("\\\\"),
                '\\' => arg.push_str("\\\\\\"),
                _ => {}
            }
            if c == '%' {
                arg.push('%');
            }
            arg.push(c);
        }
        arg.push('"');
    } else {
        arg = program.replace('%', "%%");
    }
    Some(format!("{arg} --tray"))
}

fn entry(exec: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Kompacter\n\
         Comment=Shrink files to fit an upload limit\n\
         Exec={exec}\n\
         Icon=io.github.KXZer0.Kompacter\n\
         Terminal=false\n\
         NoDisplay=true\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

fn enable(dir: &Path, exec: &str) -> std::io::Result<()> {
    let file = dir.join(FILE_NAME);
    let text = entry(exec);
    if std::fs::read_to_string(&file).is_ok_and(|old| old == text) {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    // Written beside it and renamed, so a session starting now never reads half a file.
    let partial = dir.join(format!(".{FILE_NAME}.{}", std::process::id()));
    std::fs::write(&partial, text)?;
    std::fs::rename(&partial, &file).inspect_err(|_| {
        let _ = std::fs::remove_file(&partial);
    })
}

fn disable(dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(dir.join(FILE_NAME)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn state_in(dir: &Path, exec: &str) -> State {
    let Ok(text) = std::fs::read_to_string(dir.join(FILE_NAME)) else {
        return State::Absent;
    };
    let mut in_entry = false;
    let (mut found, mut off) = (None, false);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        let Some((key, value)) = line.split_once('=').filter(|_| in_entry) else {
            continue;
        };
        match (key.trim(), value.trim()) {
            ("Exec", value) => found = Some(value.to_owned()),
            ("Hidden", "true") | ("X-GNOME-Autostart-enabled", "false") => off = true,
            _ => {}
        }
    }
    if off {
        State::Disabled
    } else if found.as_deref() == Some(exec) && !exec.is_empty() {
        State::Here
    } else {
        State::Elsewhere
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autostart_folder_follows_xdg_config_home() {
        let dir = |xdg: Option<&str>, home: Option<&str>| {
            dir_from(xdg.map(OsString::from), home.map(OsString::from))
        };
        assert_eq!(
            dir(Some("/cfg"), Some("/home/a")),
            Some(PathBuf::from("/cfg/autostart"))
        );
        assert_eq!(
            dir(None, Some("/home/a")),
            Some(PathBuf::from("/home/a/.config/autostart"))
        );
        // A relative XDG_CONFIG_HOME is ignored, as the spec says.
        assert_eq!(
            dir(Some("cfg"), Some("/home/a")),
            Some(PathBuf::from("/home/a/.config/autostart"))
        );
        assert_eq!(dir(None, None), None);
    }

    #[test]
    fn starts_the_appimage_the_deb_or_this_program() {
        let exe = Path::new("/tmp/.mount_KompaXYZ/usr/bin/Kompacter");
        assert_eq!(
            program(Some("/home/a/Apps/Kompacter.AppImage".into()), Some(exe)),
            Some(PathBuf::from("/home/a/Apps/Kompacter.AppImage"))
        );
        assert_eq!(
            program(None, Some(Path::new(DEB_PROGRAM))),
            Some(PathBuf::from("kompacter"))
        );
        assert_eq!(
            program(
                Some("".into()),
                Some(Path::new("/opt/Kompacter-1.0/Kompacter"))
            ),
            Some(PathBuf::from("/opt/Kompacter-1.0/Kompacter"))
        );
        assert_eq!(program(None, None), None);
    }

    #[test]
    fn exec_is_quoted_per_the_desktop_entry_spec() {
        let exec = |p: &str| exec_for(Path::new(p));
        assert_eq!(exec("kompacter").as_deref(), Some("kompacter --tray"));
        assert_eq!(
            exec("/opt/kompacter/Kompacter").as_deref(),
            Some("/opt/kompacter/Kompacter --tray")
        );
        assert_eq!(
            exec("/home/a/My Apps/Kompacter.AppImage").as_deref(),
            Some("\"/home/a/My Apps/Kompacter.AppImage\" --tray")
        );
        // `"`, `$`, `` ` `` and `\` are escaped inside quotes, then backslashes doubled.
        assert_eq!(
            exec(r#"/a b/"x"$y`z\w"#).as_deref(),
            Some(r#""/a b/\\"x\\"\\$y\\`z\\\\w" --tray"#)
        );
        // `%` starts a field code, so it is doubled, quoted or not.
        assert_eq!(exec("/a/100%").as_deref(), Some("/a/100%% --tray"));
        assert_eq!(exec("/a b/100%").as_deref(), Some("\"/a b/100%%\" --tray"));
        assert_eq!(exec("/a/line\nbreak"), None);
        assert_eq!(exec(""), None);
    }

    #[test]
    fn entry_has_the_autostart_keys() {
        let text = entry("kompacter --tray");
        assert!(text.starts_with("[Desktop Entry]\n"));
        for line in [
            "Type=Application",
            "Exec=kompacter --tray",
            "NoDisplay=true",
            "X-GNOME-Autostart-enabled=true",
        ] {
            assert!(text.lines().any(|l| l == line), "missing {line}");
        }
    }

    #[test]
    fn enable_detect_and_disable() {
        let config = tempfile::tempdir().unwrap();
        let dir = config.path().join("autostart");
        let here = "\"/home/a/My Apps/Kompacter.AppImage\" --tray";
        assert_eq!(state_in(&dir, here), State::Absent);
        // Disabling with nothing there is fine.
        disable(&dir).unwrap();

        enable(&dir, here).unwrap();
        assert_eq!(state_in(&dir, here), State::Here);
        assert_eq!(
            std::fs::read_to_string(dir.join(FILE_NAME)).unwrap(),
            entry(here)
        );
        // The app moved: the entry is somewhere else's until it is pointed here again.
        let moved = "/opt/Kompacter/Kompacter --tray";
        assert_eq!(state_in(&dir, moved), State::Elsewhere);
        enable(&dir, moved).unwrap();
        assert_eq!(state_in(&dir, moved), State::Here);
        assert_eq!(state_in(&dir, here), State::Elsewhere);
        // No temporary files are left behind.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);

        disable(&dir).unwrap();
        assert_eq!(state_in(&dir, moved), State::Absent);
        assert!(!dir.join(FILE_NAME).exists());
    }

    #[test]
    fn an_entry_switched_off_by_the_desktop_counts_as_off() {
        let config = tempfile::tempdir().unwrap();
        let dir = config.path();
        let exec = "kompacter --tray";
        for off in ["Hidden=true", "X-GNOME-Autostart-enabled=false"] {
            let text = entry(exec).replace("X-GNOME-Autostart-enabled=true", off);
            std::fs::write(dir.join(FILE_NAME), text).unwrap();
            assert_eq!(state_in(dir, exec), State::Disabled);
        }
        // Keys in other groups don't count.
        let text = format!(
            "{}[Desktop Action X]\nExec=other\nHidden=true\n",
            entry(exec)
        );
        std::fs::write(dir.join(FILE_NAME), text).unwrap();
        assert_eq!(state_in(dir, exec), State::Here);
    }
}
