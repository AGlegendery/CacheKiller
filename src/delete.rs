//! Safe, parallel deletion. Every target is re-validated right before removal.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use rayon::prelude::*;

use crate::locations::Special;
use crate::platform::{self, Ctx, Entry, EntryKind};

pub struct DelTask {
    pub label: String,
    pub targets: Vec<PathBuf>,
    pub roots: Vec<PathBuf>,
    pub special: Option<Special>,
    /// Size the scan reported; used for API-cleaned items that can't count bytes.
    pub expected: u64,
}

pub struct DelResult {
    pub label: String,
    pub freed: u64,
    pub removed: u64,
    pub error_count: u64,
    pub errors: Vec<String>,
}

#[derive(Default)]
pub struct DelProgress {
    pub removed: AtomicU64,
    pub freed: AtomicU64,
    pub done: AtomicUsize,
    pub total: AtomicUsize,
    pub current: Mutex<String>,
}

const MAX_ERRORS_KEPT: usize = 20;

/// Protected paths and all of their ancestors can never be removed.
fn is_critical(p: &Path, ctx: &Ctx) -> bool {
    ctx.protected.iter().any(|x| x == p || x.starts_with(p))
}

fn validate(target: &Path, roots: &[PathBuf], ctx: &Ctx) -> Result<(), String> {
    if !target.is_absolute() || target.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return Err("not a normalized absolute path".into());
    }
    if target.components().filter(|c| matches!(c, Component::Normal(_))).count() < 2 {
        return Err("path too shallow".into());
    }
    if is_critical(target, ctx) {
        return Err("protected system path".into());
    }
    if !roots.iter().any(|r| target.starts_with(r) && target != r) {
        return Err("outside of its cache location".into());
    }
    // A symlink/junction swapped into the parent chain since the scan must not redirect us.
    let parent = target.parent().ok_or("no parent")?;
    match platform::path_is_real(parent) {
        Ok(true) => Ok(()),
        Ok(false) => Err("parent path changed (link) since scan".into()),
        Err(e) => Err(e.to_string()),
    }
}

struct Remover<'a> {
    freed: AtomicU64,
    removed: AtomicU64,
    err_count: AtomicU64,
    errors: Mutex<Vec<String>>,
    prog: &'a DelProgress,
}

impl Remover<'_> {
    fn err(&self, path: &Path, msg: impl std::fmt::Display) {
        self.err_count.fetch_add(1, Ordering::Relaxed);
        let mut errs = self.errors.lock().unwrap();
        if errs.len() < MAX_ERRORS_KEPT {
            errs.push(format!("{}: {msg}", path.display()));
        }
    }

    fn gone(&self, bytes: u64) {
        self.freed.fetch_add(bytes, Ordering::Relaxed);
        self.removed.fetch_add(1, Ordering::Relaxed);
        self.prog.freed.fetch_add(bytes, Ordering::Relaxed);
        self.prog.removed.fetch_add(1, Ordering::Relaxed);
    }

    fn unlink(&self, e: &Entry) {
        match platform::remove(e) {
            // A file with other hard links frees nothing.
            Ok(()) => self.gone(if e.hardlink.is_some() { 0 } else { e.alloc }),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            // Already reported the child that kept it non-empty.
            Err(err) if err.kind() == io::ErrorKind::DirectoryNotEmpty && e.kind == EntryKind::Dir => {}
            Err(err) => self.err(&e.path, err),
        }
    }

    /// Remove `e` recursively. Never follows links, never enters another filesystem.
    fn remove(&self, e: Entry, parent_dev: u64) {
        if e.kind != EntryKind::Dir {
            return self.unlink(&e);
        }
        if e.dev != parent_dev {
            return self.err(&e.path, "mount point, skipped");
        }
        match platform::read_dir(&e.path) {
            Ok((children, _)) => children.into_par_iter().for_each(|c| self.remove(c, e.dev)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => return,
            Err(err) => return self.err(&e.path, err),
        }
        self.unlink(&e);
    }
}

fn clean_special(s: Special, task: &DelTask, r: &Remover) {
    match s {
        Special::RecycleBin => match platform::recycle_bin_empty() {
            Ok(()) => r.gone(task.expected),
            Err(e) => r.err(Path::new(&task.label), e),
        },
    }
}

pub fn run(tasks: Vec<DelTask>, ctx: &Ctx, prog: &DelProgress) -> Vec<DelResult> {
    prog.total.store(tasks.len(), Ordering::Relaxed);
    tasks
        .into_iter()
        .map(|task| {
            *prog.current.lock().unwrap() = task.label.clone();
            let r = Remover {
                freed: AtomicU64::new(0),
                removed: AtomicU64::new(0),
                err_count: AtomicU64::new(0),
                errors: Mutex::new(Vec::new()),
                prog,
            };
            if let Some(s) = task.special {
                clean_special(s, &task, &r);
            }
            task.targets.par_iter().for_each(|t| {
                if let Err(e) = validate(t, &task.roots, ctx) {
                    return r.err(t, format!("refused: {e}"));
                }
                let parent = t.parent().map(platform::stat);
                match (platform::stat(t), parent) {
                    (Ok(e), Some(Ok(p))) => r.remove(e, p.dev),
                    (Err(e), _) if e.kind() == io::ErrorKind::NotFound => {}
                    (Err(e), _) | (_, Some(Err(e))) => r.err(t, e),
                    (_, None) => r.err(t, "no parent"),
                }
            });
            prog.done.fetch_add(1, Ordering::Relaxed);
            DelResult {
                label: task.label,
                freed: r.freed.into_inner(),
                removed: r.removed.into_inner(),
                error_count: r.err_count.into_inner(),
                errors: r.errors.into_inner().unwrap(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn ctx() -> Ctx {
        platform::detect_ctx(0)
    }

    #[test]
    fn refuses_dangerous_paths() {
        let c = ctx();
        let tmp = std::env::temp_dir();
        let roots = vec![tmp.clone()];
        assert!(validate(&tmp, &roots, &c).is_err());
        assert!(validate(&tmp.join("..").join("x"), &roots, &c).is_err());
        assert!(validate(&c.home, &[c.home.parent().unwrap().to_path_buf()], &c).is_err());
        assert!(validate(&c.home.join("x"), &roots, &c).is_err(), "outside root");
    }

    #[test]
    fn deletes_nested_and_readonly() {
        let base = std::env::temp_dir().join(format!("ck-ro-{}", std::process::id()));
        fs::create_dir_all(base.join("cache").join("a").join("b")).unwrap();
        let root = platform::canonical(&base.join("cache")).unwrap();
        let f = root.join("a").join("b").join("ro.bin");
        fs::write(&f, vec![1u8; 64 * 1024]).unwrap();
        let mut perm = fs::metadata(&f).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&f, perm).unwrap();

        let prog = DelProgress::default();
        let task = DelTask { label: "t".into(), targets: vec![root.join("a")], roots: vec![root.clone()], special: None, expected: 0 };
        let res = run(vec![task], &ctx(), &prog);
        assert_eq!(res[0].error_count, 0, "{:?}", res[0].errors);
        assert!(!root.join("a").exists());
        assert!(root.exists(), "root must be kept");
        assert!(res[0].freed >= 64 * 1024, "freed {}", res[0].freed);
        fs::remove_dir_all(&base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn deletes_tree_and_counts_hardlinks() {
        let base = std::env::temp_dir().join(format!("ck-test-{}", std::process::id()));
        let root = base.join("cache");
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("a/b/f"), vec![1u8; 8192]).unwrap();
        fs::write(base.join("outside"), vec![1u8; 8192]).unwrap();
        fs::hard_link(base.join("outside"), root.join("a/link")).unwrap();
        std::os::unix::fs::symlink(&base, root.join("a/sym")).unwrap();

        let prog = DelProgress::default();
        let res = run(
            vec![DelTask {
                label: "t".into(),
                targets: vec![root.join("a")],
                roots: vec![root.clone()],
                special: None,
                expected: 0,
            }],
            &ctx(),
            &prog,
        );
        assert_eq!(res[0].error_count, 0, "{:?}", res[0].errors);
        assert!(!root.join("a").exists());
        assert!(base.join("outside").exists(), "symlink was followed");
        // 8 KiB file + 2 dirs; the hard link frees nothing.
        assert!(res[0].freed >= 8192 && res[0].freed < 8192 * 2 + 4096 * 4);
        fs::remove_dir_all(&base).unwrap();
    }
}
