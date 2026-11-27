//! Noticing copied files on Wayland. An ordinary Wayland window only sees the clipboard
//! while it has focus, so this uses a data-control device, the protocol clipboard
//! managers use: `ext-data-control-v1` where the compositor has it, else the older
//! `wlr-data-control` (KDE, Sway and other wlroots desktops). GNOME offers neither, and
//! X11 sessions are not covered: there the feature reports itself unavailable.
//!
//! The watcher runs on its own thread and its own connection. It reads a selection only
//! when it offers a file list, and only while someone holds a [`Watcher`].

use std::io::Read;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use wayland_client::backend::WaylandError;
use wayland_client::globals::{registry_queue_init, GlobalList, GlobalListContents};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{event_created_child, Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1 as ext_device, ext_data_control_manager_v1 as ext_manager,
    ext_data_control_offer_v1 as ext_offer,
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1 as wlr_device, zwlr_data_control_manager_v1 as wlr_manager,
    zwlr_data_control_offer_v1 as wlr_offer,
};

/// File lists, in the order they are tried.
const FILE_TYPES: [&str; 2] = [super::FILE_LIST, super::GNOME_FILES];
/// How long the app that copied may take to hand over its file list.
const READ_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_LIST: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Protocol {
    Ext,
    Wlr,
}

/// Which data-control manager to use, or why copies can't be noticed. `globals` is
/// `None` when there is no Wayland session to ask, else whether the compositor offers
/// the ext and the wlr manager.
fn decide(globals: Option<(bool, bool)>, desktop: &str) -> Result<Protocol, &'static str> {
    match globals {
        None => Err("Needs a Wayland session"),
        Some((true, _)) => Ok(Protocol::Ext),
        Some((false, true)) => Ok(Protocol::Wlr),
        Some((false, false)) if desktop.split(':').any(|d| d.eq_ignore_ascii_case("gnome")) => {
            Err("GNOME doesn't let apps see what you copy")
        }
        Some((false, false)) => Err("This desktop doesn't let apps see what you copy"),
    }
}

/// Whether copies can be noticed in this session; `Err` says why not, in a few words.
pub fn support() -> Result<(), &'static str> {
    static SUPPORT: OnceLock<Result<(), &'static str>> = OnceLock::new();
    *SUPPORT.get_or_init(|| {
        let globals = std::env::var_os("WAYLAND_DISPLAY")
            .is_some()
            .then(|| {
                let (_, globals, _) = connect()?;
                Some(offered(&globals))
            })
            .flatten();
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        decide(globals, &desktop).map(|_| ())
    })
}

fn connect() -> Option<(Connection, GlobalList, EventQueue<State>)> {
    let conn = Connection::connect_to_env().ok()?;
    let (globals, queue) = registry_queue_init::<State>(&conn).ok()?;
    Some((conn, globals, queue))
}

fn offered(globals: &GlobalList) -> (bool, bool) {
    let has = |name: &str| {
        globals
            .contents()
            .with_list(|l| l.iter().any(|g| g.interface == name))
    };
    (
        has(ext_manager::ExtDataControlManagerV1::interface().name),
        has(wlr_manager::ZwlrDataControlManagerV1::interface().name),
    )
}

struct Shared {
    seq: AtomicU32,
    files: Mutex<Vec<PathBuf>>,
}

/// Watches the clipboard until dropped.
pub struct Watcher {
    shared: Arc<Shared>,
    /// Dropping it wakes the thread, which then disconnects.
    _stop: UnixStream,
}

impl Watcher {
    /// `None` where the session has no data-control manager or no seat.
    pub fn start() -> Option<Self> {
        let (conn, globals, mut queue) = connect()?;
        let qh = queue.handle();
        let protocol = decide(Some(offered(&globals)), "").ok()?;
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ()).ok()?;
        let device = match protocol {
            Protocol::Ext => {
                let manager: ext_manager::ExtDataControlManagerV1 =
                    globals.bind(&qh, 1..=1, ()).ok()?;
                Device::Ext(manager.get_data_device(&seat, &qh, ()), manager)
            }
            Protocol::Wlr => {
                // Version 1: the clipboard only, not the primary selection.
                let manager: wlr_manager::ZwlrDataControlManagerV1 =
                    globals.bind(&qh, 1..=1, ()).ok()?;
                Device::Wlr(manager.get_data_device(&seat, &qh, ()), manager)
            }
        };
        let mut state = State::default();
        // The compositor answers with what is on the clipboard now. That was copied
        // before the watch began, so it doesn't count as a copy.
        queue.roundtrip(&mut state).ok()?;
        state.changed = false;
        if state.finished {
            return None;
        }
        let (stop, wake) = UnixStream::pair().ok()?;
        let shared = Arc::new(Shared {
            seq: AtomicU32::new(0),
            files: Mutex::new(Vec::new()),
        });
        let watching = shared.clone();
        std::thread::Builder::new()
            .name("clipboard-watch".into())
            .spawn(move || {
                run(&conn, &mut queue, &mut state, &wake, &watching);
                drop(state);
                device.destroy();
                let _ = conn.flush();
            })
            .ok()?;
        Some(Self {
            shared,
            _stop: stop,
        })
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

fn run(
    conn: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
    wake: &UnixStream,
    shared: &Shared,
) {
    loop {
        if queue.dispatch_pending(state).is_err() || state.finished {
            return;
        }
        if std::mem::take(&mut state.changed) {
            let files = state
                .selection
                .as_ref()
                .map(|offer| read_files(conn, offer))
                .unwrap_or_default();
            if let Ok(mut latest) = shared.files.lock() {
                *latest = files;
            }
            shared.seq.fetch_add(1, Ordering::Release);
        }
        if conn.flush().is_err() {
            return;
        }
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let mut fds = [
            libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: both descriptors stay open for the call.
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        if fds[1].revents != 0 {
            return;
        }
        if fds[0].revents & libc::POLLIN != 0 {
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return,
            }
        } else if fds[0].revents != 0 {
            return;
        }
    }
}

/// Ask the app that copied for its file list, if it offers one.
fn read_files(conn: &Connection, offer: &Offer) -> Vec<PathBuf> {
    let types = offer.types();
    for kind in FILE_TYPES {
        if !types.iter().any(|t| t == kind) {
            continue;
        }
        let Ok((reader, writer)) = std::io::pipe() else {
            return Vec::new();
        };
        offer.receive(kind, writer.as_fd());
        if conn.flush().is_err() {
            return Vec::new();
        }
        // Only the other app keeps a write end, so the read ends when it is done.
        drop(writer);
        let files = super::paths_from_uris(&read_until_closed(reader, READ_TIMEOUT));
        if !files.is_empty() {
            return files;
        }
    }
    Vec::new()
}

fn read_until_closed(mut reader: std::io::PipeReader, timeout: Duration) -> Vec<u8> {
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let mut fd = libc::pollfd {
            fd: reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: the descriptor stays open for the call.
        let ready = unsafe { libc::poll(&mut fd, 1, left.as_millis() as libc::c_int) };
        if ready == 0 {
            return Vec::new();
        }
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Vec::new();
        }
        match reader.read(&mut chunk) {
            Ok(0) => return bytes,
            Ok(n) if bytes.len() + n <= MAX_LIST => bytes.extend_from_slice(&chunk[..n]),
            Ok(_) => return Vec::new(),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Vec::new(),
        }
    }
}

/// The mime types an offer announced.
type Types = Mutex<Vec<String>>;

enum Device {
    Ext(
        ext_device::ExtDataControlDeviceV1,
        ext_manager::ExtDataControlManagerV1,
    ),
    Wlr(
        wlr_device::ZwlrDataControlDeviceV1,
        wlr_manager::ZwlrDataControlManagerV1,
    ),
}

impl Device {
    fn destroy(&self) {
        match self {
            Self::Ext(device, manager) => {
                device.destroy();
                manager.destroy();
            }
            Self::Wlr(device, manager) => {
                device.destroy();
                manager.destroy();
            }
        }
    }
}

/// What is on the clipboard. Destroyed once something else is copied.
enum Offer {
    Ext(ext_offer::ExtDataControlOfferV1),
    Wlr(wlr_offer::ZwlrDataControlOfferV1),
}

impl Offer {
    fn types(&self) -> Vec<String> {
        let types = match self {
            Self::Ext(offer) => offer.data::<Types>(),
            Self::Wlr(offer) => offer.data::<Types>(),
        };
        types
            .and_then(|t| t.lock().ok().map(|t| t.clone()))
            .unwrap_or_default()
    }

    fn receive(&self, kind: &str, fd: std::os::fd::BorrowedFd<'_>) {
        match self {
            Self::Ext(offer) => offer.receive(kind.into(), fd),
            Self::Wlr(offer) => offer.receive(kind.into(), fd),
        }
    }
}

impl Drop for Offer {
    fn drop(&mut self) {
        match self {
            Self::Ext(offer) => offer.destroy(),
            Self::Wlr(offer) => offer.destroy(),
        }
    }
}

#[derive(Default)]
struct State {
    selection: Option<Offer>,
    changed: bool,
    finished: bool,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// The ext and wlr protocols are the same apart from their names.
macro_rules! data_control {
    ($variant:ident, $manager:ident::$Manager:ident, $device:ident::$Device:ident, $offer:ident::$Offer:ident) => {
        impl Dispatch<$manager::$Manager, ()> for State {
            fn event(
                _: &mut Self,
                _: &$manager::$Manager,
                _: $manager::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }

        impl Dispatch<$device::$Device, ()> for State {
            fn event(
                state: &mut Self,
                _: &$device::$Device,
                event: $device::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                match event {
                    $device::Event::Selection { id } => {
                        state.selection = id.map(Offer::$variant);
                        state.changed = true;
                    }
                    $device::Event::PrimarySelection { id: Some(offer) } => offer.destroy(),
                    $device::Event::Finished => state.finished = true,
                    _ => {}
                }
            }

            event_created_child!(State, $device::$Device, [
                $device::EVT_DATA_OFFER_OPCODE => ($offer::$Offer, Types::default()),
            ]);
        }

        impl Dispatch<$offer::$Offer, Types> for State {
            fn event(
                _: &mut Self,
                _: &$offer::$Offer,
                event: $offer::Event,
                types: &Types,
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                if let $offer::Event::Offer { mime_type } = event {
                    if let Ok(mut types) = types.lock() {
                        types.push(mime_type);
                    }
                }
            }
        }
    };
}

data_control!(
    Ext,
    ext_manager::ExtDataControlManagerV1,
    ext_device::ExtDataControlDeviceV1,
    ext_offer::ExtDataControlOfferV1
);
data_control!(
    Wlr,
    wlr_manager::ZwlrDataControlManagerV1,
    wlr_device::ZwlrDataControlDeviceV1,
    wlr_offer::ZwlrDataControlOfferV1
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_a_manager_or_says_why_not() {
        assert_eq!(decide(Some((true, true)), "KDE"), Ok(Protocol::Ext));
        assert_eq!(decide(Some((false, true)), "sway"), Ok(Protocol::Wlr));
        assert_eq!(decide(Some((true, false)), "niri"), Ok(Protocol::Ext));
        assert_eq!(
            decide(Some((false, false)), "ubuntu:GNOME"),
            Err("GNOME doesn't let apps see what you copy")
        );
        assert_eq!(
            decide(Some((false, false)), "weston"),
            Err("This desktop doesn't let apps see what you copy")
        );
        // X11 sessions, or no display at all.
        assert_eq!(decide(None, "GNOME"), Err("Needs a Wayland session"));
    }

    #[test]
    fn a_file_list_is_read_until_the_app_closes_it() {
        let (reader, mut writer) = std::io::pipe().unwrap();
        let sender = std::thread::spawn(move || {
            use std::io::Write;
            writer.write_all(b"file:///tmp/a%20b.mp4\r\n").unwrap();
        });
        let bytes = read_until_closed(reader, READ_TIMEOUT);
        sender.join().unwrap();
        assert_eq!(
            super::super::paths_from_uris(&bytes),
            [PathBuf::from("/tmp/a b.mp4")]
        );
    }

    #[test]
    fn an_app_that_never_answers_is_given_up_on() {
        let (reader, _writer) = std::io::pipe().unwrap();
        assert!(read_until_closed(reader, Duration::from_millis(50)).is_empty());
    }

    /// Needs a compositor with data-control and `wl-copy`, e.g.
    /// `WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 sway` with `WAYLAND_DISPLAY` set.
    #[test]
    #[ignore]
    fn sees_files_copied_in_another_app() {
        // wl-copy stays in the background to serve the copy; don't let it hold our output.
        use std::process::Stdio;
        let watcher = Watcher::start().expect("a compositor with data-control");
        let before = watcher.seq();
        let status = std::process::Command::new("wl-copy")
            .args([
                "--type",
                "text/uri-list",
                "file:///tmp/kompacter%20watch.mp4",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("wl-copy");
        assert!(status.success());
        let deadline = Instant::now() + Duration::from_secs(5);
        while watcher.seq() == before && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_ne!(watcher.seq(), before);
        assert_eq!(watcher.files(), [PathBuf::from("/tmp/kompacter watch.mp4")]);

        // Text is a copy too, but not of files.
        let before = watcher.seq();
        let status = std::process::Command::new("wl-copy")
            .arg("hello")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("wl-copy");
        assert!(status.success());
        let deadline = Instant::now() + Duration::from_secs(5);
        while watcher.seq() == before && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_ne!(watcher.seq(), before);
        assert!(watcher.files().is_empty());
    }
}
