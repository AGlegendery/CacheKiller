//! Linux / Unix: plain users, `sudo` and `pkexec`.

use std::env;
use std::ffi::{CStr, CString, OsStr};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::{Ctx, Entry, EntryKind};

pub const ADMIN: &str = "sudo";
pub const ROOT_LABEL: &str = "root";
pub const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub const LOCK_MARK: &str = " ⊘ ";

pub struct OsDirs {
    pub euid: u32,
    pub uid: u32,
    pub cache_home: PathBuf,
    pub config_home: PathBuf,
    pub data_home: PathBuf,
}

struct Pw {
    name: String,
    uid: u32,
    gid: u32,
    home: PathBuf,
}

fn pw_from(p: *const libc::passwd) -> Option<Pw> {
    if p.is_null() {
        return None;
    }
    // SAFETY: getpw* returned a valid entry; we copy the fields out immediately
    // (called once at startup, before any other thread exists).
    unsafe {
        Some(Pw {
            name: CStr::from_ptr((*p).pw_name).to_string_lossy().into_owned(),
            uid: (*p).pw_uid,
            gid: (*p).pw_gid,
            home: PathBuf::from(OsStr::from_bytes(CStr::from_ptr((*p).pw_dir).to_bytes())),
        })
    }
}

fn pw_by_name(name: &str) -> Option<Pw> {
    let c = CString::new(name).ok()?;
    pw_from(unsafe { libc::getpwnam(c.as_ptr()) })
}

fn pw_by_uid(uid: u32) -> Option<Pw> {
    pw_from(unsafe { libc::getpwuid(uid) })
}

fn xdg(var: &str, fallback: PathBuf, honor_env: bool) -> PathBuf {
    if honor_env
        && let Some(v) = env::var_os(var)
    {
        let p = PathBuf::from(v);
        if p.is_absolute() {
            return p;
        }
    }
    fallback
}

pub fn detect_ctx(tmp_min_age_secs: i64) -> Ctx {
    let euid = unsafe { libc::geteuid() };
    let is_admin = euid == 0;

    // When elevated, clean the *invoking* user's home, not /root.
    let invoker = if is_admin {
        env::var("SUDO_USER")
            .ok()
            .filter(|u| !u.is_empty() && u != "root")
            .and_then(|u| pw_by_name(&u))
            .or_else(|| {
                env::var("PKEXEC_UID")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .filter(|&uid: &u32| uid != 0)
                    .and_then(pw_by_uid)
            })
    } else {
        None
    };
    let elevated_for_user = invoker.is_some();

    let (user, uid, gid, home) = match invoker.or_else(|| pw_by_uid(euid)) {
        Some(pw) => {
            // Trust $HOME only for ourselves; under sudo it may point anywhere.
            let home = if elevated_for_user {
                pw.home
            } else {
                env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or(pw.home)
            };
            (pw.name, pw.uid, pw.gid, home)
        }
        None => (
            euid.to_string(),
            euid,
            unsafe { libc::getegid() },
            env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into()),
        ),
    };

    let own_env = !elevated_for_user;
    let cache_home = xdg("XDG_CACHE_HOME", home.join(".cache"), own_env);
    let config_home = xdg("XDG_CONFIG_HOME", home.join(".config"), own_env);
    let data_home = xdg("XDG_DATA_HOME", home.join(".local/share"), own_env);
    let state_home = xdg("XDG_STATE_HOME", home.join(".local/state"), own_env);

    let mut protected: Vec<PathBuf> = [
        "/", "/bin", "/boot", "/dev", "/etc", "/home", "/lib", "/lib32", "/lib64", "/libx32", "/media", "/mnt", "/opt",
        "/proc", "/root", "/run", "/sbin", "/srv", "/sys", "/usr", "/var", "/var/lib", "/var/log", "/var/cache",
        "/var/tmp", "/tmp", "/snap",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    protected.extend([home.clone(), cache_home.clone(), config_home.clone(), data_home.clone(), state_home.clone()]);

    Ctx {
        is_admin,
        elevated_for_user,
        user,
        log_dir: state_home.join("cachekiller"),
        log_owner: elevated_for_user.then_some((uid, gid)),
        protected,
        tmp_min_age_secs,
        os: OsDirs { euid, uid, cache_home, config_home, data_home },
        home,
    }
}

fn entry(path: PathBuf, md: &fs::Metadata) -> Entry {
    let ft = md.file_type();
    let kind = if ft.is_symlink() {
        EntryKind::Link
    } else if ft.is_dir() {
        EntryKind::Dir
    } else if ft.is_file() {
        EntryKind::File
    } else {
        EntryKind::Special
    };
    Entry {
        path,
        kind,
        alloc: md.blocks() * 512,
        size: md.len(),
        newest: md.mtime().max(md.ctime()),
        dev: md.dev(),
        hardlink: (kind != EntryKind::Dir && md.nlink() > 1).then(|| (md.dev(), md.ino())),
        owner: md.uid(),
        attrs: md.mode(),
    }
}

pub fn stat(path: &Path) -> io::Result<Entry> {
    fs::symlink_metadata(path).map(|md| entry(path.to_path_buf(), &md))
}

/// Children of `dir` plus the number of entries that couldn't be read.
pub fn read_dir(dir: &Path) -> io::Result<(Vec<Entry>, u64)> {
    let mut out = Vec::new();
    let mut errors = 0;
    for e in fs::read_dir(dir)? {
        // DirEntry::metadata does not follow symlinks.
        match e.and_then(|e| e.metadata().map(|md| entry(e.path(), &md))) {
            Ok(en) => out.push(en),
            Err(_) => errors += 1,
        }
    }
    Ok((out, errors))
}

pub fn canonical(p: &Path) -> Option<PathBuf> {
    fs::canonicalize(p).ok()
}

/// False if a symlink was swapped into the path since it was scanned.
pub fn path_is_real(p: &Path) -> io::Result<bool> {
    Ok(fs::canonicalize(p)? == p)
}

fn access(path: &Path, mode: libc::c_int) -> bool {
    match CString::new(path.as_os_str().as_bytes()) {
        Ok(c) => unsafe { libc::access(c.as_ptr(), mode) == 0 },
        Err(_) => false,
    }
}

/// Can entries inside `dir` be removed by us?
pub fn dir_writable(dir: &Path, ctx: &Ctx) -> bool {
    ctx.is_admin || access(dir, libc::W_OK | libc::X_OK)
}

/// Can `e` itself be unlinked? Respects the sticky bit (/tmp, /var/tmp).
pub fn can_unlink(e: &Entry, ctx: &Ctx) -> bool {
    if ctx.is_admin {
        return true;
    }
    let Some(parent) = e.path.parent() else { return false };
    let Ok(pmd) = fs::metadata(parent) else { return false };
    if !dir_writable(parent, ctx) {
        return false;
    }
    let sticky = pmd.mode() & libc::S_ISVTX as u32 != 0;
    let euid = ctx.os.euid;
    !sticky || pmd.uid() == euid || e.owner == euid
}

/// Remove one entry (a file, a link, or an already emptied directory).
pub fn remove(e: &Entry) -> io::Result<()> {
    match e.kind {
        EntryKind::Dir => fs::remove_dir(&e.path),
        _ => fs::remove_file(&e.path),
    }
}

/// Names in /tmp that belong to live sessions or services.
pub fn tmp_protected(name: &str, kind: EntryKind) -> bool {
    const EXACT: &[&str] = &[
        ".X11-unix",
        ".ICE-unix",
        ".XIM-unix",
        ".font-unix",
        ".Test-unix",
        "lost+found",
        "snap-private-tmp",
    ];
    const PREFIX: &[&str] = &[
        "systemd-private-",
        "tmux-",
        "ssh-",
        "pulse-",
        "krb5cc",
        "dbus-",
        "screen-",
        "runtime-",
        ".org.chromium.",
        "gnome-software-",
        "sddm-",
        "lightdm",
        "xauth",
        "clr-debug-pipe-",
        "vscode-",
    ];
    kind == EntryKind::Special
        || EXACT.contains(&name)
        || PREFIX.iter().any(|p| name.starts_with(p))
        || (name.starts_with(".X") && name.ends_with("-lock"))
}

/// Replace this process with `sudo <self> …` (falls back to pkexec). Returns only on failure.
pub fn relaunch_elevated(args: &[String]) -> io::Error {
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    let exe = env::current_exe().unwrap_or_else(|_| "cachekiller".into());
    println!("Restarting with sudo to unlock system caches…");
    let e1 = Command::new("sudo").arg(&exe).args(args).exec();
    let e2 = Command::new("pkexec").arg(&exe).args(args).exec();
    io::Error::other(format!("sudo: {e1}; pkexec: {e2}"))
}

pub fn chown(p: &Path, owner: Option<(u32, u32)>) {
    if let Some((u, g)) = owner {
        let _ = std::os::unix::fs::chown(p, Some(u), Some(g));
    }
}

/// The Linux trash is plain directories; only Windows needs the Shell API.
pub fn recycle_bin_stats() -> Option<(u64, u64)> {
    None
}

pub fn recycle_bin_empty() -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}
