//! Automatic: notices big files without being asked. The worker sleeps until a
//! watch is enabled, then only polls the sources the user chose.

use std::cell::RefCell;
use std::collections::{hash_map::Entry, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime};

use crate::sys;

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

pub struct Watch {
    updates: RefCell<Option<Sender<Config>>>,
    on_event: fn(WatchEvent),
}

const VIDEO: [&str; 6] = ["mp4", "mkv", "mov", "webm", "avi", "m4v"];
const CLIPBOARD_POLL: Duration = Duration::from_millis(500);
const FOLDER_POLL: Duration = Duration::from_secs(3);
/// How deep to look under each clip folder (ShadowPlay makes one folder per game).
const DEPTH: usize = 3;

/// Kompacter's own results end in ".kompacter.<ext>"; never pick those up again.
fn ours(p: &Path) -> bool {
    p.file_stem()
        .is_some_and(|s| s.to_string_lossy().ends_with(".kompacter"))
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
        } else if path
            .extension()
            .is_some_and(|ext| VIDEO.iter().any(|kind| ext.eq_ignore_ascii_case(kind)))
            && !ours(&path)
        {
            if let Ok(meta) = e.metadata() {
                if let Ok(modified) = meta.modified() {
                    out.push((path, meta.len(), modified));
                }
            }
        }
    }
}

#[derive(Default)]
struct Clips {
    generation: u64,
    files: HashMap<PathBuf, Clip>,
}

struct Clip {
    size: u64,
    modified: SystemTime,
    stable: u8,
    seen: u64,
}

impl Clips {
    fn scan(
        &mut self,
        found: impl IntoIterator<Item = (PathBuf, u64, SystemTime)>,
        since: SystemTime,
        limit: u64,
        on_clip: impl Fn(PathBuf),
    ) {
        self.generation = self.generation.wrapping_add(1);
        let scan = self.generation;
        for (path, size, modified) in found {
            if modified < since {
                continue;
            }
            let mut entry = match self.files.entry(path) {
                Entry::Occupied(entry) => entry,
                Entry::Vacant(entry) => {
                    entry.insert(Clip {
                        size,
                        modified,
                        stable: 0,
                        seen: scan,
                    });
                    continue;
                }
            };
            let state = entry.get_mut();
            // Overlapping watch roots can list a file twice in one scan.
            if state.seen == scan {
                continue;
            }
            state.seen = scan;
            if size != state.size || modified != state.modified {
                state.size = size;
                state.modified = modified;
                state.stable = 0;
            } else if size > 0 && state.stable < 2 {
                state.stable += 1;
                if state.stable == 2 && size > limit {
                    on_clip(entry.key().clone());
                }
            }
        }
        self.files.retain(|_, state| state.seen == scan);
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
            .send(c);
    }

    fn start_worker(on_event: fn(WatchEvent)) -> Sender<Config> {
        let (config, updates) = mpsc::channel::<Config>();
        std::thread::spawn(move || {
            let mut c = Config::default();
            let mut seq = 0;
            let mut clips = Clips::default();
            let mut since = SystemTime::now();
            let mut copy_at: Option<Instant> = None;
            let mut folder_at: Option<Instant> = None;
            loop {
                let next = [copy_at, folder_at].into_iter().flatten().min();
                let update = match next {
                    Some(next) => {
                        updates.recv_timeout(next.saturating_duration_since(Instant::now()))
                    }
                    None => updates.recv().map_err(|_| RecvTimeoutError::Disconnected),
                };
                let now = Instant::now();
                match update {
                    Ok(new) => {
                        if new.copies && !c.copies {
                            seq = sys::clipboard_seq();
                            copy_at = Some(now + CLIPBOARD_POLL);
                        } else if !new.copies {
                            copy_at = None;
                        }
                        if new.folders_on && !new.folders.is_empty() {
                            if folder_at.is_none() {
                                since = SystemTime::now();
                                folder_at = Some(now + FOLDER_POLL);
                            }
                        } else {
                            folder_at = None;
                            clips = Clips::default();
                        }
                        c = new;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => {}
                }
                if copy_at.is_some_and(|at| now >= at) {
                    copy_at = Some(now + CLIPBOARD_POLL);
                    let current = sys::clipboard_seq();
                    if current != seq {
                        seq = current;
                        let files = sys::clipboard_files();
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
                if folder_at.is_some_and(|at| now >= at) {
                    let mut found = Vec::new();
                    for folder in &c.folders {
                        walk(folder, DEPTH, &mut found);
                    }
                    clips.scan(found, since, c.limit, |path| {
                        on_event(WatchEvent::Clip(path))
                    });
                    folder_at = Some(Instant::now() + FOLDER_POLL);
                }
            }
        });
        config
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
        let original = (path.clone(), 50, t);
        clips.scan([original.clone(), original.clone()], t, 10, on_clip);
        clips.scan([original.clone()], t, 10, on_clip);
        assert!(events.borrow().is_empty());
        clips.scan([original.clone()], t, 10, on_clip);
        clips.scan([original], t, 10, on_clip);
        assert_eq!(events.borrow().len(), 1);

        let replacement = (path.clone(), 50, t + Duration::from_secs(1));
        for _ in 0..3 {
            clips.scan([replacement.clone()], t, 10, on_clip);
        }
        assert_eq!(*events.borrow(), [path.clone(), path]);
        clips.scan([], t, 10, on_clip);
        assert!(clips.files.is_empty());
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
}
