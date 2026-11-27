//! Automatic: notices big files without being asked. The worker sleeps until a
//! watch is enabled, then only wakes when the sources the user chose report a change:
//! the clipboard watcher after a copy, the file system (inotify, ReadDirectoryChangesW,
//! FSEvents) when something changes in a clip folder.

use std::cell::RefCell;
use std::collections::{hash_map::Entry, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime};

use notify::{EventKind, Watcher as _};

use crate::platform as sys;

#[cfg(windows)]
use sys::clipboard_watch::Watcher as Copies;
/// Where new copies come from. Each tells the worker when something was copied.
/// Windows: a clipboard format listener. macOS: a main-thread timer watches the
/// pasteboard. Linux: a Wayland data-control watcher.
#[cfg(target_os = "linux")]
use sys::linux::clipboard_watch::Watcher as Copies;
#[cfg(target_os = "macos")]
use sys::macos_watch::Watcher as Copies;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
struct Copies;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
impl Copies {
    fn start(_on_copy: impl Fn() + Send + Sync + 'static) -> Option<Self> {
        None
    }
    fn seq(&self) -> u32 {
        0
    }
    fn files(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}

pub enum WatchEvent {
    /// Files were copied and at least one is over the limit.
    Copied(Vec<PathBuf>),
    /// A new clip over the limit finished saving in a clip folder.
    Clip(PathBuf),
}

#[derive(Default)]
pub struct Config {
    pub copies: bool,
    pub folders_on: bool,
    pub folders: Vec<PathBuf>,
    pub limit: u64,
}

/// What wakes the worker.
enum Msg {
    Config(Config),
    /// The clipboard watcher saw a copy.
    Copied,
    /// Something changed in a clip folder.
    Files(notify::Result<notify::Event>),
    Stop,
}

pub struct Watch {
    updates: RefCell<Option<Sender<Msg>>>,
    on_event: fn(WatchEvent),
}

const VIDEO: [&str; 6] = ["mp4", "mkv", "mov", "webm", "avi", "m4v"];
/// A copy is read this long after it is reported, so a burst counts once.
const COPY_SETTLE: Duration = Duration::from_millis(100);
/// How often a changed clip is looked at until its size holds still.
const CLIP_CHECK: Duration = Duration::from_secs(3);
/// Folders file-change events can't cover (the watch failed, or a network drive) are
/// walked this often instead.
const RESCAN: Duration = Duration::from_secs(60);
/// How deep to look under each clip folder (ShadowPlay makes one folder per game).
const DEPTH: usize = 3;

/// Kompacter's own results end in ".kompacter.<ext>"; never pick those up again.
fn ours(p: &Path) -> bool {
    p.file_stem()
        .is_some_and(|s| s.to_string_lossy().ends_with(".kompacter"))
}

fn is_clip(p: &Path) -> bool {
    p.extension()
        .is_some_and(|ext| VIDEO.iter().any(|kind| ext.eq_ignore_ascii_case(kind)))
        && !ours(p)
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, u64, SystemTime)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let Ok(kind) = e.file_type() else { continue };
        let path = e.path();
        if kind.is_dir() {
            if depth > 0 {
                walk(&path, depth - 1, out);
            }
        } else if is_clip(&path) {
            if let Ok(meta) = e.metadata() {
                if let Ok(modified) = meta.modified() {
                    out.push((path, meta.len(), modified));
                }
            }
        }
    }
}

fn stat(path: &Path) -> Option<(u64, SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    meta.is_file()
        .then(|| meta.modified().ok().map(|m| (meta.len(), m)))
        .flatten()
}

/// Clips seen since the watch began. A clip counts once its size and time hold still
/// over two looks `CLIP_CHECK` apart; only `pending` ones are looked at.
#[derive(Default)]
struct Clips {
    files: HashMap<PathBuf, Clip>,
    pending: HashSet<PathBuf>,
}

struct Clip {
    size: u64,
    modified: SystemTime,
    stable: u8,
}

impl Clips {
    /// Something happened to `path`: look at it on the next check.
    fn touched(&mut self, path: PathBuf) {
        self.pending.insert(path);
    }

    /// A walk found a clip: look at it only when it is new or changed.
    fn found(&mut self, path: PathBuf, size: u64, modified: SystemTime, since: SystemTime) {
        if modified < since {
            return;
        }
        let known = self
            .files
            .get(&path)
            .is_some_and(|c| c.size == size && c.modified == modified);
        if !known {
            self.pending.insert(path);
        }
    }

    fn check(
        &mut self,
        stat: impl Fn(&Path) -> Option<(u64, SystemTime)>,
        since: SystemTime,
        limit: u64,
        on_clip: impl Fn(PathBuf),
    ) {
        for path in std::mem::take(&mut self.pending) {
            let Some((size, modified)) = stat(&path).filter(|&(_, m)| m >= since) else {
                self.files.remove(&path);
                continue;
            };
            let mut entry = match self.files.entry(path) {
                Entry::Occupied(entry) => entry,
                Entry::Vacant(entry) => {
                    self.pending.insert(entry.key().clone());
                    entry.insert(Clip {
                        size,
                        modified,
                        stable: 0,
                    });
                    continue;
                }
            };
            let state = entry.get_mut();
            if size != state.size || modified != state.modified {
                state.size = size;
                state.modified = modified;
                state.stable = 0;
                self.pending.insert(entry.key().clone());
            } else if size > 0 && state.stable < 2 {
                state.stable += 1;
                if state.stable == 2 {
                    if size > limit {
                        on_clip(entry.key().clone());
                    }
                } else {
                    self.pending.insert(entry.key().clone());
                }
            }
            // Else it holds still (already counted, or empty): wait for the next change.
        }
    }
}

/// How many levels `path` is below the closest of `roots` (0 for a root itself).
fn depth_below(roots: &[PathBuf], path: &Path) -> Option<usize> {
    roots
        .iter()
        .filter_map(|root| path.strip_prefix(root).ok())
        .map(|rest| rest.components().count())
        .min()
}

/// Whether file-change events may miss what other computers write here.
#[cfg(target_os = "linux")]
fn network_drive(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    const REMOTE: [i64; 4] = [
        0x6969,                 // NFS
        0x517B,                 // SMB
        0xFF53_4D42_u32 as i64, // CIFS
        0xFE53_4D42_u32 as i64, // SMB2
    ];
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut fs: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: a valid, terminated path and a buffer of the right type.
    if unsafe { libc::statfs(c_path.as_ptr(), &mut fs) } != 0 {
        return false;
    }
    #[allow(clippy::unnecessary_cast)]
    let kind = fs.f_type as i64;
    REMOTE.contains(&kind) || REMOTE.contains(&(kind & 0xFFFF_FFFF))
}

#[cfg(windows)]
fn network_drive(path: &Path) -> bool {
    sys::network_drive(path)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn network_drive(_path: &Path) -> bool {
    false
}

/// The clip folders being watched.
struct Folders {
    roots: Vec<PathBuf>,
    watcher: Option<notify::RecommendedWatcher>,
    /// Roots that are walked every `RESCAN` as well: their watch failed, or they are on
    /// a network drive.
    rescanned: Vec<PathBuf>,
    clips: Clips,
    since: SystemTime,
    check_at: Option<Instant>,
    rescan_at: Option<Instant>,
}

impl Folders {
    fn new(roots: &[PathBuf], events: &Sender<Msg>) -> Self {
        let mut folders = Self {
            roots: Vec::new(),
            watcher: None,
            rescanned: Vec::new(),
            clips: Clips::default(),
            since: SystemTime::now(),
            check_at: None,
            rescan_at: None,
        };
        folders.set_roots(roots, events);
        folders
    }

    fn set_roots(&mut self, roots: &[PathBuf], events: &Sender<Msg>) {
        let roots: Vec<PathBuf> = roots
            .iter()
            .map(|r| std::path::absolute(r).unwrap_or_else(|_| r.clone()))
            .collect();
        if roots == self.roots {
            return;
        }
        // The old watch stops before the new one starts.
        self.watcher = None;
        self.rescanned.clear();
        let events = events.clone();
        let config = notify::Config::default()
            .with_event_kinds(notify::EventKindMask::CORE)
            .with_fsevent_latency(Duration::from_secs(1));
        self.watcher = notify::RecommendedWatcher::new(
            move |event| {
                let _ = events.send(Msg::Files(event));
            },
            config,
        )
        .ok();
        for root in &roots {
            let watched = self
                .watcher
                .as_mut()
                .is_some_and(|w| watch_tree(w, root, DEPTH));
            if !watched || network_drive(root) {
                self.rescanned.push(root.clone());
            }
        }
        self.roots = roots;
        self.rescan_at = (!self.rescanned.is_empty()).then(|| Instant::now() + RESCAN);
    }

    fn next(&self) -> Option<Instant> {
        [self.check_at, self.rescan_at].into_iter().flatten().min()
    }

    fn event(&mut self, event: notify::Result<notify::Event>) {
        let event = match event {
            Ok(event) if !event.need_rescan() => event,
            // Events may have been lost: walk everything once.
            _ => {
                let roots = self.roots.clone();
                self.rescan(&roots);
                return;
            }
        };
        // Only a new or renamed folder needs walking; changes inside one report themselves.
        let new_dirs = matches!(
            event.kind,
            EventKind::Create(_)
                | EventKind::Modify(notify::event::ModifyKind::Name(_))
                | EventKind::Any
                | EventKind::Other
        );
        for path in event.paths {
            let Some(depth) = depth_below(&self.roots, &path) else {
                continue;
            };
            if new_dirs && depth <= DEPTH && path.is_dir() {
                #[cfg(target_os = "linux")]
                if let Some(w) = self.watcher.as_mut() {
                    watch_tree(w, &path, DEPTH - depth);
                }
                let mut found = Vec::new();
                walk(&path, DEPTH - depth, &mut found);
                for (path, size, modified) in found {
                    self.clips.found(path, size, modified, self.since);
                }
            } else if (1..=DEPTH + 1).contains(&depth) && is_clip(&path) {
                self.clips.touched(path);
            }
        }
    }

    fn rescan(&mut self, roots: &[PathBuf]) {
        let mut found = Vec::new();
        for root in roots {
            walk(root, DEPTH, &mut found);
        }
        for (path, size, modified) in found {
            self.clips.found(path, size, modified, self.since);
        }
    }

    /// Runs what is due, then plans the next check.
    fn tick(&mut self, now: Instant, limit: u64, on_clip: impl Fn(PathBuf)) {
        if self.rescan_at.is_some_and(|at| now >= at) {
            let roots = self.rescanned.clone();
            self.rescan(&roots);
            self.rescan_at = Some(now + RESCAN);
        }
        if self.check_at.is_some_and(|at| now >= at) {
            let roots = &self.roots;
            let in_reach = |p: &Path| depth_below(roots, p).is_some_and(|d| d <= DEPTH + 1);
            self.clips.check(
                |p| in_reach(p).then(|| stat(p)).flatten(),
                self.since,
                limit,
                on_clip,
            );
            self.check_at = None;
        }
        if self.check_at.is_none() && !self.clips.pending.is_empty() {
            self.check_at = Some(now + CLIP_CHECK);
        }
    }
}

/// Watches `root` and the folders up to `depth` below it. Linux (inotify) watches each
/// folder on its own so the depth limit also bounds the number of watches; elsewhere
/// one recursive watch is cheap and events from deeper down are ignored.
fn watch_tree(watcher: &mut notify::RecommendedWatcher, root: &Path, depth: usize) -> bool {
    #[cfg(target_os = "linux")]
    {
        if watcher
            .watch(root, notify::RecursiveMode::NonRecursive)
            .is_err()
        {
            return false;
        }
        if depth > 0 {
            if let Ok(entries) = std::fs::read_dir(root) {
                for e in entries.flatten() {
                    if e.file_type().is_ok_and(|t| t.is_dir()) {
                        // A subfolder that can't be watched is still covered by its parent
                        // for new folders; its own changes are missed, like a full limit.
                        watch_tree(watcher, &e.path(), depth - 1);
                    }
                }
            }
        }
        true
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = depth;
        watcher
            .watch(root, notify::RecursiveMode::Recursive)
            .is_ok()
    }
}

impl Watch {
    pub fn new(on_event: fn(WatchEvent)) -> Self {
        Self {
            updates: RefCell::new(None),
            on_event,
        }
    }

    pub fn set(&self, c: Config) {
        let mut updates = self.updates.borrow_mut();
        if !c.copies && (!c.folders_on || c.folders.is_empty()) && updates.is_none() {
            return;
        }
        let _ = updates
            .get_or_insert_with(|| Self::start_worker(self.on_event))
            .send(Msg::Config(c));
    }

    fn start_worker(on_event: fn(WatchEvent)) -> Sender<Msg> {
        let (config, updates) = mpsc::channel::<Msg>();
        let events = config.clone();
        std::thread::spawn(move || {
            let mut c = Config::default();
            let mut seq = 0;
            let mut copies: Option<Copies> = None;
            let mut folders: Option<Folders> = None;
            let mut copy_at: Option<Instant> = None;
            loop {
                let next = [copy_at, folders.as_ref().and_then(Folders::next)]
                    .into_iter()
                    .flatten()
                    .min();
                let update = match next {
                    Some(next) => {
                        updates.recv_timeout(next.saturating_duration_since(Instant::now()))
                    }
                    None => updates.recv().map_err(|_| RecvTimeoutError::Disconnected),
                };
                match update {
                    Ok(Msg::Config(new)) => {
                        if new.copies && !c.copies {
                            let events = events.clone();
                            copies = Copies::start(move || {
                                let _ = events.send(Msg::Copied);
                            });
                            seq = copies.as_ref().map_or(0, Copies::seq);
                            copy_at = None;
                        } else if !new.copies {
                            copies = None;
                            copy_at = None;
                        }
                        if new.folders_on && !new.folders.is_empty() {
                            match folders.as_mut() {
                                Some(f) => f.set_roots(&new.folders, &events),
                                None => folders = Some(Folders::new(&new.folders, &events)),
                            }
                        } else {
                            folders = None;
                        }
                        c = new;
                    }
                    Ok(Msg::Copied) => {
                        if copies.is_some() && copy_at.is_none() {
                            copy_at = Some(Instant::now() + COPY_SETTLE);
                        }
                    }
                    Ok(Msg::Files(event)) => {
                        if let Some(f) = folders.as_mut() {
                            f.event(event);
                        }
                    }
                    Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => {}
                }
                let now = Instant::now();
                if copy_at.is_some_and(|at| now >= at) {
                    copy_at = None;
                    if let Some(source) = copies.as_ref() {
                        let current = source.seq();
                        if current != seq {
                            seq = current;
                            let files = source.files();
                            let over = files.iter().any(|p| {
                                !ours(p)
                                    && std::fs::metadata(p)
                                        .is_ok_and(|m| m.is_file() && m.len() > c.limit)
                            });
                            if over {
                                on_event(WatchEvent::Copied(files));
                            }
                        }
                    }
                }
                if let Some(f) = folders.as_mut() {
                    f.tick(now, c.limit, |path| on_event(WatchEvent::Clip(path)));
                }
            }
        });
        config
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        // The worker holds a sender of its own (for the watchers), so say it outright.
        if let Some(updates) = self.updates.get_mut() {
            let _ = updates.send(Msg::Stop);
        }
    }
}

/// Clip folders that exist on this PC: Medal, the Xbox Game Bar, AMD ReLive.
pub fn suggested_folders() -> Vec<PathBuf> {
    let Some(videos) = sys::videos_dir() else {
        return Vec::new();
    };
    ["Medal", "Captures", "Radeon ReLive"]
        .iter()
        .map(|n| videos.join(n))
        .filter(|p| p.is_dir())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_our_own_results() {
        assert!(ours(Path::new("C:/v/clip.kompacter.mp4")));
        assert!(!ours(Path::new("C:/v/clip.mp4")));
        assert!(!ours(Path::new("C:/v/kompacter.mp4")));
    }

    #[test]
    fn a_saved_clip_fires_once_and_replacements_are_seen() {
        let mut clips = Clips::default();
        let t = SystemTime::now();
        let path = PathBuf::from("clip.mp4");
        let events = RefCell::new(Vec::new());
        let on_clip = |path| events.borrow_mut().push(path);
        let file = RefCell::new((50, t));
        let stat = |_: &Path| Some(*file.borrow());

        // Seen twice (overlapping roots), then looked at until it holds still.
        clips.touched(path.clone());
        clips.touched(path.clone());
        clips.check(stat, t, 10, on_clip);
        clips.check(stat, t, 10, on_clip);
        assert!(events.borrow().is_empty());
        clips.check(stat, t, 10, on_clip);
        assert_eq!(events.borrow().len(), 1);
        assert!(clips.pending.is_empty());

        // Opening or rescanning it again changes nothing.
        clips.touched(path.clone());
        clips.found(path.clone(), 50, t, t);
        for _ in 0..3 {
            clips.check(stat, t, 10, on_clip);
        }
        assert_eq!(events.borrow().len(), 1);

        // Replaced by a new recording with the same name.
        *file.borrow_mut() = (50, t + Duration::from_secs(1));
        clips.touched(path.clone());
        for _ in 0..3 {
            clips.check(stat, t, 10, on_clip);
        }
        assert_eq!(*events.borrow(), [path.clone(), path.clone()]);

        // Deleted.
        clips.touched(path);
        clips.check(|_| None, t, 10, on_clip);
        assert!(clips.files.is_empty() && clips.pending.is_empty());
    }

    #[test]
    fn a_growing_clip_waits_and_old_or_small_ones_never_fire() {
        let mut clips = Clips::default();
        let t = SystemTime::now();
        let events = RefCell::new(Vec::new());
        let on_clip = |path| events.borrow_mut().push(path);
        let size = RefCell::new(1);
        let growing = PathBuf::from("growing.mp4");
        clips.touched(growing.clone());
        for _ in 0..5 {
            *size.borrow_mut() += 100;
            clips.check(|_| Some((*size.borrow(), t)), t, 10, on_clip);
        }
        assert!(events.borrow().is_empty());
        clips.check(|_| Some((*size.borrow(), t)), t, 10, on_clip);
        clips.check(|_| Some((*size.borrow(), t)), t, 10, on_clip);
        assert_eq!(*events.borrow(), [growing]);

        let old = PathBuf::from("old.mp4");
        clips.found(old.clone(), 50, t - Duration::from_secs(1), t);
        clips.touched(old);
        let small = PathBuf::from("small.mp4");
        clips.touched(small);
        for _ in 0..3 {
            clips.check(
                |p| {
                    Some(if p.ends_with("old.mp4") {
                        (50, t - Duration::from_secs(1))
                    } else {
                        (5, t)
                    })
                },
                t,
                10,
                on_clip,
            );
        }
        assert_eq!(events.borrow().len(), 1);
        assert!(clips.pending.is_empty());
    }

    #[test]
    fn finds_videos_a_few_folders_down() {
        let d = tempfile::tempdir().unwrap();
        let game = d.path().join("Game");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(game.join("a.MP4"), b"x").unwrap();
        std::fs::write(game.join("a.kompacter.mp4"), b"x").unwrap();
        std::fs::write(game.join("notes.txt"), b"x").unwrap();
        let mut out = Vec::new();
        walk(d.path(), DEPTH, &mut out);
        let names: Vec<_> = out
            .iter()
            .map(|(p, _, _)| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.MP4"]);
    }

    #[test]
    fn depth_is_counted_from_the_closest_root() {
        let roots = [PathBuf::from("/v"), PathBuf::from("/v/Game")];
        assert_eq!(depth_below(&roots, Path::new("/v")), Some(0));
        assert_eq!(depth_below(&roots, Path::new("/v/Game/a/b.mp4")), Some(2));
        assert_eq!(depth_below(&roots, Path::new("/w/b.mp4")), None);
    }

    /// File-change events find a clip saved in a new folder, but not one too deep.
    #[test]
    fn events_find_new_clips_within_reach() {
        let d = tempfile::tempdir().unwrap();
        let root = std::path::absolute(d.path()).unwrap();
        let (tx, rx) = mpsc::channel();
        let mut folders = Folders::new(std::slice::from_ref(&root), &tx);
        assert!(folders.rescanned.is_empty(), "the watch should start here");
        let game = root.join("Game");
        std::fs::create_dir(&game).unwrap();
        // Let the new folder's watch start before saving into it.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Msg::Files(event)) => folders.event(event),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        std::fs::write(game.join("clip.mp4"), b"video").unwrap();
        let deep = game.join("a/b/c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("deep.mp4"), b"video").unwrap();
        let clip = game.join("clip.mp4");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !folders.clips.pending.contains(&clip) && Instant::now() < deadline {
            if let Ok(Msg::Files(event)) = rx.recv_timeout(Duration::from_millis(100)) {
                folders.event(event);
            }
        }
        while let Ok(Msg::Files(event)) = rx.recv_timeout(Duration::from_millis(300)) {
            folders.event(event);
        }
        assert!(folders.clips.pending.contains(&clip));
        assert!(!folders.clips.pending.contains(&deep.join("deep.mp4")));
        assert!(folders.next().is_none());
        folders.tick(Instant::now(), 1, |_| {});
        assert!(folders.next().is_some());
    }
}
