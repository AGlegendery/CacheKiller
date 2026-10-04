//! Everything OS-specific lives behind this module: who we are, how a directory is
//! listed, how an entry is removed, and which words the UI uses for "admin".

use std::path::PathBuf;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntryKind {
    File,
    Dir,
    /// Symlink / junction / mount point: removed itself, never followed.
    Link,
    /// Socket, FIFO, device node.
    #[cfg_attr(windows, allow(dead_code))]
    Special,
}

/// One directory entry with everything the scanner needs, read without following links.
#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub kind: EntryKind,
    /// Bytes actually allocated on disk.
    pub alloc: u64,
    /// Apparent size (file length).
    pub size: u64,
    /// Newest of modification / change time, unix seconds.
    pub newest: i64,
    /// Filesystem id; 0 where the OS gives us mount points as links instead.
    pub dev: u64,
    /// `(dev, inode)` when the file has more than one hard link.
    pub hardlink: Option<(u64, u64)>,
    #[cfg_attr(windows, allow(dead_code))]
    pub owner: u32,
    /// Unix mode bits / Windows file attributes.
    #[cfg_attr(unix, allow(dead_code))]
    pub attrs: u32,
}

pub struct Ctx {
    pub is_admin: bool,
    /// Running elevated on behalf of a regular user (sudo / pkexec).
    pub elevated_for_user: bool,
    pub user: String,
    pub home: PathBuf,
    pub log_dir: PathBuf,
    /// chown target for log files when elevated for a user.
    pub log_owner: Option<(u32, u32)>,
    /// Never delete these, nor any of their ancestors.
    pub protected: Vec<PathBuf>,
    /// Temp entries changed more recently than this are left alone.
    pub tmp_min_age_secs: i64,
    pub os: OsDirs,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn lists_with_sizes() {
        let dir = std::env::temp_dir().join(format!("ck-ls-{}", std::process::id()));
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("f.bin"), vec![7u8; 100_000]).unwrap();
        let (entries, errors) = read_dir(&dir).unwrap();
        assert_eq!(errors, 0);
        assert_eq!(entries.len(), 2);
        let f = entries.iter().find(|e| e.path.ends_with("f.bin")).unwrap();
        assert_eq!(f.kind, EntryKind::File);
        assert_eq!(f.size, 100_000);
        assert!(f.alloc >= 100_000, "alloc {}", f.alloc);
        assert!(entries.iter().any(|e| e.kind == EntryKind::Dir && e.path.ends_with("sub")));
        let st = stat(&dir.join("f.bin")).unwrap();
        assert_eq!((st.size, st.kind), (100_000, EntryKind::File));
        assert_eq!(stat(&dir).unwrap().kind, EntryKind::Dir);
        fs::remove_dir_all(&dir).unwrap();
    }
}
