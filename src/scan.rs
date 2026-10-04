//! Parallel, `du`-accurate size scanner.
//!
//! * disk usage = bytes actually allocated (Linux: st_blocks × 512, like `du`;
//!   Windows: NTFS allocation size, like Explorer's "size on disk"); apparent size too
//! * hard-linked files are counted once, and treated as *not* reclaimable (deleting one
//!   link frees nothing while another link exists — e.g. pnpm/conda/snapd stores)
//! * never follows symlinks/junctions, never crosses into another filesystem
//! * bytes we lack permission to delete are reported as "locked" instead of reclaimable

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use crate::locations::{self, Category, Kind, Special};
use crate::platform::{self, Ctx, Entry, EntryKind};
use crate::util::now_secs;

#[derive(Default, Clone, Copy, Debug)]
pub struct Stats {
    pub disk: u64,
    pub apparent: u64,
    /// Hard-linked elsewhere: deleting won't free these.
    pub shared: u64,
    /// No permission to delete.
    pub locked: u64,
    pub files: u64,
    pub dirs: u64,
    pub errors: u64,
    pub mounts: u64,
    /// Newest mtime/ctime seen anywhere in the tree.
    pub newest: i64,
}

impl Stats {
    pub fn reclaimable(&self) -> u64 {
        self.disk.saturating_sub(self.shared + self.locked)
    }

    fn merge(mut self, o: Stats) -> Stats {
        self.disk += o.disk;
        self.apparent += o.apparent;
        self.shared += o.shared;
        self.locked += o.locked;
        self.files += o.files;
        self.dirs += o.dirs;
        self.errors += o.errors;
        self.mounts += o.mounts;
        self.newest = self.newest.max(o.newest);
        self
    }
}

#[derive(Debug, Clone)]
pub struct Item {
    pub path: PathBuf,
    pub note: Option<String>,
    pub category: Category,
    pub kind: Kind,
    pub targets: Vec<PathBuf>,
    pub roots: Vec<PathBuf>,
    pub special: Option<Special>,
    pub stats: Stats,
    pub selected: bool,
}

impl Item {
    pub fn selectable(&self) -> bool {
        self.stats.reclaimable() > 0 || (self.stats.locked == 0 && self.stats.files > 0)
    }
}

#[derive(Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub bytes: AtomicU64,
    pub done: AtomicUsize,
    pub total: AtomicUsize,
    pub current: Mutex<String>,
}

pub struct ScanResult {
    pub items: Vec<Item>,
    pub elapsed: Duration,
    pub files: u64,
    pub hidden_recent_tmp: usize,
}

struct Walker<'a> {
    ctx: &'a Ctx,
    seen: &'a Mutex<HashSet<(u64, u64)>>,
    prog: &'a Progress,
}

impl Walker<'_> {
    fn account(&self, e: &Entry, locked: bool, st: &mut Stats) {
        if let Some(id) = e.hardlink
            && !self.seen.lock().unwrap().insert(id)
        {
            return; // another link already counted
        }
        st.disk += e.alloc;
        st.apparent += e.size;
        if locked {
            st.locked += e.alloc;
        } else if e.hardlink.is_some() {
            st.shared += e.alloc;
        }
        if e.kind == EntryKind::Dir {
            st.dirs += 1;
        } else {
            st.files += 1;
        }
        st.newest = st.newest.max(e.newest);
    }

    fn target(&self, path: &Path) -> Stats {
        let mut st = Stats::default();
        let Ok(e) = platform::stat(path) else {
            st.errors += 1;
            return st;
        };
        self.account(&e, !platform::can_unlink(&e, self.ctx), &mut st);
        if e.kind == EntryKind::Dir {
            st = st.merge(self.dir(path, e.dev));
        } else {
            self.prog.files.fetch_add(1, Ordering::Relaxed);
            self.prog.bytes.fetch_add(st.disk, Ordering::Relaxed);
        }
        st
    }

    fn dir(&self, dir: &Path, dev: u64) -> Stats {
        let mut st = Stats::default();
        let entries = match platform::read_dir(dir) {
            Ok((entries, errors)) => {
                st.errors += errors;
                entries
            }
            Err(_) => {
                st.errors += 1;
                return st;
            }
        };
        let writable = platform::dir_writable(dir, self.ctx);
        let mut subdirs = Vec::new();
        for e in entries {
            if e.kind == EntryKind::Dir {
                if e.dev != dev {
                    st.mounts += 1;
                    continue;
                }
                self.account(&e, !writable, &mut st);
                subdirs.push(e.path);
            } else {
                self.account(&e, !writable, &mut st);
            }
        }
        self.prog.files.fetch_add(st.files, Ordering::Relaxed);
        self.prog.bytes.fetch_add(st.disk, Ordering::Relaxed);

        if subdirs.is_empty() {
            st
        } else {
            let sub = subdirs.par_iter().map(|d| self.dir(d, dev)).reduce(Stats::default, Stats::merge);
            st.merge(sub)
        }
    }
}

fn special_stats(s: Special) -> Stats {
    match s {
        Special::RecycleBin => match platform::recycle_bin_stats() {
            Some((bytes, items)) => Stats {
                disk: bytes,
                apparent: bytes,
                files: items,
                newest: now_secs(),
                ..Stats::default()
            },
            None => Stats::default(),
        },
    }
}

pub fn scan(ctx: &Ctx, prog: &Progress) -> ScanResult {
    let start = Instant::now();
    *prog.current.lock().unwrap() = "resolving cache locations".into();
    let cands = locations::resolve(ctx);
    prog.total.store(cands.len(), Ordering::Relaxed);

    let seen = Mutex::new(HashSet::new());
    let walker = Walker { ctx, seen: &seen, prog };

    let scanned: Vec<(bool, Item)> = cands
        .into_par_iter()
        .map(|c| {
            *prog.current.lock().unwrap() = c.path.display().to_string();
            let stats = match c.special {
                Some(s) => special_stats(s),
                None => c.targets.par_iter().map(|t| walker.target(t)).reduce(Stats::default, Stats::merge),
            };
            prog.done.fetch_add(1, Ordering::Relaxed);
            let item = Item {
                path: c.path,
                note: c.note,
                category: c.category,
                kind: c.kind,
                targets: c.targets,
                roots: c.roots,
                special: c.special,
                stats,
                selected: false,
            };
            (c.tmp, item)
        })
        .collect();

    // Temp rule: only offer entries left untouched for at least tmp_min_age_secs.
    let now = now_secs();
    let mut hidden_recent_tmp = 0;
    let mut items: Vec<Item> = Vec::with_capacity(scanned.len());
    for (tmp, it) in scanned {
        if tmp && now - it.stats.newest < ctx.tmp_min_age_secs {
            hidden_recent_tmp += 1;
        } else {
            items.push(it);
        }
    }

    items.retain(|it| it.stats.files > 0);
    items.sort_by(|a, b| b.stats.reclaimable().cmp(&a.stats.reclaimable()).then(b.stats.disk.cmp(&a.stats.disk)));

    ScanResult {
        files: items.iter().map(|i| i.stats.files).sum(),
        items,
        elapsed: start.elapsed(),
        hidden_recent_tmp,
    }
}
