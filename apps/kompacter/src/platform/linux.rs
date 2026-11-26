//! Linux desktop activation, native file clipboard and file manager integration.

use std::ffi::OsString;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

mod activation;
pub use activation::activate_window;

pub const APP_ID: &str = "io.github.KXZer0.Kompacter";
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_MESSAGE: u64 = 8 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
pub enum Action {
    Open,
    Choose,
    Quit,
    Quiet,
}

#[derive(Deserialize, Serialize)]
pub struct Activation {
    pub action: Action,
    paths: Vec<Vec<u8>>,
    pub token: Option<String>,
}

impl Activation {
    fn from_args() -> Self {
        let mut action = Action::Open;
        let mut paths = Vec::new();
        let mut literal = false;
        for arg in std::env::args_os().skip(1) {
            if !literal {
                match arg.to_str() {
                    Some("--") => {
                        literal = true;
                        continue;
                    }
                    Some("--tray") => {
                        action = Action::Quiet;
                        continue;
                    }
                    Some("--choose-files") => {
                        action = Action::Choose;
                        continue;
                    }
                    Some("--quit") => {
                        action = Action::Quit;
                        continue;
                    }
                    Some(arg) if arg.starts_with("--") => continue,
                    _ => {}
                }
            }
            if let Ok(path) = std::path::absolute(&arg) {
                if path.exists() {
                    paths.push(path.into_os_string().into_vec());
                }
            }
        }
        if !paths.is_empty() && matches!(action, Action::Quiet) {
            action = Action::Open;
        }
        Self {
            action,
            paths,
            token: std::env::var("XDG_ACTIVATION_TOKEN")
                .or_else(|_| std::env::var("DESKTOP_STARTUP_ID"))
                .ok(),
        }
    }

    pub fn take_files(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.paths)
            .into_iter()
            .map(|bytes| PathBuf::from(OsString::from_vec(bytes)))
            .collect()
    }
}

pub struct Primary {
    listener: UnixListener,
    initial: Activation,
}

impl Primary {
    pub fn listen(self, on_activation: impl Fn(Activation) + Send + 'static) -> Activation {
        let Self { listener, initial } = self;
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                // An abstract socket is automatically released at exit, but is
                // visible to other users. Only accept messages from our own UID.
                if !same_user(&stream) || stream.set_read_timeout(Some(IO_TIMEOUT)).is_err() {
                    continue;
                }
                let mut body = Vec::new();
                let read = (&stream).take(MAX_MESSAGE + 1).read_to_end(&mut body);
                if read.is_ok() && body.len() as u64 <= MAX_MESSAGE {
                    if let Ok(activation) = serde_json::from_slice(&body) {
                        on_activation(activation);
                        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
                        let _ = stream.write_all(b"\x01");
                    }
                }
            }
        });
        initial
    }
}

fn same_user(stream: &UnixStream) -> bool {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&cred) as libc::socklen_t;
    // SO_PEERCRED reads the credentials associated with the connected socket.
    unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        ) == 0
            && cred.uid == libc::geteuid()
    }
}

fn socket_address() -> io::Result<SocketAddr> {
    // Keep each graphical login independent, including multiple sessions
    // sharing /run/user/<uid>. Use the Wayland socket in a Wayland session.
    let mut session = DefaultHasher::new();
    let display = std::env::var_os("WAYLAND_DISPLAY").or_else(|| std::env::var_os("DISPLAY"));
    display.hash(&mut session);
    std::env::var_os("XDG_RUNTIME_DIR").hash(&mut session);
    let name = format!(
        "{APP_ID}.{}.{:016x}",
        unsafe { libc::geteuid() },
        session.finish()
    );
    SocketAddr::from_abstract_name(name.as_bytes())
}

/// Elect a resident before initializing the GUI, or deliver this launch to it.
pub fn launch() -> io::Result<Option<Primary>> {
    let initial = Activation::from_args();
    let address = socket_address()?;
    let deadline = std::time::Instant::now() + IO_TIMEOUT;
    loop {
        match UnixListener::bind_addr(&address) {
            Ok(_) if matches!(initial.action, Action::Quit) => return Ok(None),
            Ok(listener) => return Ok(Some(Primary { listener, initial })),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {}
            Err(error) => return Err(error),
        }

        match UnixStream::connect_addr(&address) {
            Ok(mut stream) => {
                if !same_user(&stream) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "Kompacter socket owner differs",
                    ));
                }
                stream.set_write_timeout(Some(IO_TIMEOUT))?;
                stream.set_read_timeout(Some(Duration::from_secs(10)))?;
                let message = serde_json::to_vec(&initial)?;
                if message.len() as u64 > MAX_MESSAGE {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Too many files for one activation",
                    ));
                }
                stream.write_all(&message)?;
                stream.shutdown(Shutdown::Write)?;
                return stream.read_exact(&mut [0]).map(|()| None);
            }
            // The previous resident may have exited between bind and connect.
            Err(error)
                if error.kind() == io::ErrorKind::ConnectionRefused
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error),
        }
    }
}

const FILE_LIST: &str = "text/uri-list";
const GNOME_FILES: &str = "x-special/gnome-copied-files";

// X11 serves clipboard requests on a background connection. Reuse its
// context across jobs so hiding the panel leaves a single selection owner.
static CLIPBOARD: std::sync::Mutex<Option<clipboard_rs::ClipboardContext>> =
    std::sync::Mutex::new(None);

fn clipboard<T>(
    f: impl FnOnce(&clipboard_rs::ClipboardContext) -> clipboard_rs::common::Result<T>,
) -> Result<T, String> {
    let mut context = CLIPBOARD.lock().map_err(|e| e.to_string())?;
    if context.is_none() {
        *context = Some(clipboard_rs::ClipboardContext::new().map_err(|e| e.to_string())?);
    }
    f(context.as_ref().expect("clipboard initialized")).map_err(|e| e.to_string())
}

pub fn clipboard_files() -> Vec<PathBuf> {
    use clipboard_rs::Clipboard;

    clipboard(|clipboard| {
        let formats = clipboard.available_formats()?;
        for mime in [FILE_LIST, GNOME_FILES] {
            if !formats.iter().any(|format| format == mime) {
                continue;
            }
            if let Ok(bytes) = clipboard.get_buffer(mime) {
                let files = paths_from_uris(&bytes);
                if !files.is_empty() {
                    return Ok(files);
                }
            }
        }
        Ok(Vec::new())
    })
    .unwrap_or_default()
}

fn paths_from_uris(bytes: &[u8]) -> Vec<PathBuf> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Vec::new();
    };
    text.trim_end_matches('\0')
        .lines()
        .filter_map(|line| url::Url::parse(line).ok()?.to_file_path().ok())
        .collect()
}

pub fn copy_files(paths: &[PathBuf]) -> Result<(), String> {
    use clipboard_rs::{Clipboard, ClipboardContent};

    if cfg!(debug_assertions) && std::env::var_os("KOMPACTER_DEMO_COPY").is_some() {
        return Ok(());
    }
    let uris = paths
        .iter()
        .map(|path| {
            url::Url::from_file_path(path)
                .map(|uri| uri.to_string())
                .map_err(|()| format!("Invalid clipboard path: {}", path.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if uris.is_empty() {
        return Err("No files to copy".into());
    }
    clipboard(|clipboard| {
        clipboard.set(vec![
            ClipboardContent::Other(FILE_LIST.into(), uris.join("\r\n").into_bytes()),
            ClipboardContent::Other(
                GNOME_FILES.into(),
                format!("copy\n{}", uris.join("\n")).into_bytes(),
            ),
            ClipboardContent::Other("application/x-kde-cutselection".into(), b"0".to_vec()),
        ])
    })
}

pub fn show_in_folder(file: &Path) {
    let file = file.to_owned();
    std::thread::spawn(move || {
        if reveal(&file).is_err() {
            super::open_folder(file.parent().unwrap_or(Path::new(".")));
        }
    });
}

fn reveal(file: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let uri = url::Url::from_file_path(std::path::absolute(file)?)
        .map_err(|()| io::Error::new(io::ErrorKind::InvalidInput, "Invalid file path"))?;
    let session = zbus::blocking::Connection::session()?;
    let manager = zbus::blocking::Proxy::new(
        &session,
        "org.freedesktop.FileManager1",
        "/org/freedesktop/FileManager1",
        "org.freedesktop.FileManager1",
    )?;
    manager.call::<_, _, ()>("ShowItems", &([uri.as_str()], ""))?;
    Ok(())
}

/// Track host availability off the UI thread. Slint's SNI service retries
/// registrations once it is running, but needs a host for its initial start.
pub fn watch_tray_host(on_available: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || loop {
        if tray_host_available() {
            on_available();
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    });
}

fn tray_host_available() -> bool {
    let Ok(session) = zbus::blocking::Connection::session() else {
        return false;
    };
    zbus::blocking::proxy::Builder::<zbus::blocking::Proxy<'_>>::new(&session)
        .destination("org.kde.StatusNotifierWatcher")
        .and_then(|builder| builder.path("/StatusNotifierWatcher"))
        .and_then(|builder| builder.interface("org.kde.StatusNotifierWatcher"))
        .and_then(|builder| {
            builder
                .cache_properties(zbus::proxy::CacheProperties::No)
                .build()
        })
        .and_then(|watcher| watcher.get_property::<bool>("IsStatusNotifierHostRegistered"))
        .unwrap_or(false)
}
