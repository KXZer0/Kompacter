//! Automatic: notices big files without being asked. One background thread checks the
//! clipboard twice a second (a single number that changes on every copy) and the clip
//! folders every few seconds. It only looks; the UI thread decides what to do.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crate::sys;

pub enum WatchEvent {
    /// Files were copied and at least one is over the limit.
    Copied(Vec<PathBuf>),
    /// A new clip over the limit finished saving in a clip folder.
    Clip(PathBuf),
}

#[derive(Clone, Default)]
pub struct Config {
    pub copies: bool,
    pub folders_on: bool,
    pub folders: Vec<PathBuf>,
    pub limit: u64,
}

#[derive(Clone)]
pub struct Watch(Arc<Mutex<Config>>);

const VIDEO: [&str; 6] = ["mp4", "mkv", "mov", "webm", "avi", "m4v"];
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
        let Ok(meta) = e.metadata() else { continue };
        let path = e.path();
        if meta.is_dir() {
            if depth > 0 {
                walk(&path, depth - 1, out);
            }
        } else if path
            .extension()
            .is_some_and(|x| VIDEO.contains(&x.to_string_lossy().to_lowercase().as_str()))
            && !ours(&path)
        {
            if let Ok(m) = meta.modified() {
                out.push((path, meta.len(), m));
            }
        }
    }
}

impl Watch {
    pub fn start(on_event: impl Fn(WatchEvent) + Send + 'static) -> Watch {
        let cfg = Arc::new(Mutex::new(Config::default()));
        let shared = cfg.clone();
        std::thread::spawn(move || {
            let mut seq = sys::clipboard_seq();
            // Only clips saved after the folders were turned on count.
            let mut since: Option<SystemTime> = None;
            // Size at the last look and how many looks it stayed the same.
            let mut growing: HashMap<PathBuf, (u64, u32)> = HashMap::new();
            let mut done: HashSet<PathBuf> = HashSet::new();
            let mut tick = 0u32;
            loop {
                std::thread::sleep(Duration::from_millis(500));
                tick = tick.wrapping_add(1);
                let c = shared.lock().map(|c| c.clone()).unwrap_or_default();

                let now = sys::clipboard_seq();
                if now != seq {
                    seq = now;
                    if c.copies {
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

                if !c.folders_on || c.folders.is_empty() {
                    since = None;
                    growing.clear();
                    continue;
                }
                let start = *since.get_or_insert_with(SystemTime::now);
                // Every 3 s: a clip counts once its size stayed the same twice (saved).
                if !tick.is_multiple_of(6) {
                    continue;
                }
                let mut found = Vec::new();
                for f in &c.folders {
                    walk(f, DEPTH, &mut found);
                }
                let mut alive = HashSet::new();
                for (path, size, modified) in found {
                    if modified < start || done.contains(&path) {
                        continue;
                    }
                    alive.insert(path.clone());
                    let entry = growing.entry(path.clone()).or_insert((size, 0));
                    if entry.0 == size && size > 0 {
                        entry.1 += 1;
                    } else {
                        *entry = (size, 0);
                    }
                    if entry.1 >= 2 {
                        growing.remove(&path);
                        done.insert(path.clone());
                        if size > c.limit {
                            on_event(WatchEvent::Clip(path));
                        }
                    }
                }
                growing.retain(|p, _| alive.contains(p));
            }
        });
        Watch(cfg)
    }

    pub fn set(&self, c: Config) {
        if let Ok(mut g) = self.0.lock() {
            *g = c;
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
