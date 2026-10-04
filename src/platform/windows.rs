//! Windows 10/11.
//!
//! Directories are listed with `GetFileInformationByHandleEx(FileFullDirectoryInfo)`,
//! which returns the real allocation size ("size on disk") of every entry in bulk —
//! no per-file open, so it is both faster and more accurate than FindFirstFile.

use std::env;
use std::ffi::c_void;
use std::fs;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf, Prefix};
use std::ptr::{addr_of, null, null_mut};

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_NO_MORE_FILES, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_READONLY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_FULL_DIR_INFO,
    FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_STANDARD_INFO, FileFullDirectoryInfo, FileStandardInfo, GetFileInformationByHandle,
    GetFileInformationByHandleEx, OPEN_EXISTING, SetFileAttributesW,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::UI::Shell::{
    SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND, SHEmptyRecycleBinW, SHQUERYRBINFO, SHQueryRecycleBinW,
    ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use super::{Ctx, Entry, EntryKind};

pub const ADMIN: &str = "admin";
pub const ROOT_LABEL: &str = "Administrator";
pub const SPINNER: &[&str] = &["|", "/", "-", "\\"];
pub const LOCK_MARK: &str = " - ";

pub struct OsDirs {
    pub local: PathBuf,
    pub roaming: PathBuf,
    pub temp: PathBuf,
    pub windir: PathBuf,
    pub program_data: PathBuf,
    pub program_files: Vec<PathBuf>,
}

fn env_path(var: &str) -> Option<PathBuf> {
    env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute())
}

fn is_elevated() -> bool {
    // SAFETY: plain Win32 calls with properly sized out-params; the token handle is closed.
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut el: TOKEN_ELEVATION = zeroed();
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut el as *mut _ as *mut c_void,
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        ok != 0 && el.TokenIsElevated != 0
    }
}

pub fn detect_ctx(tmp_min_age_secs: i64) -> Ctx {
    let drive = env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    let drive_root = PathBuf::from(format!("{drive}\\"));
    let home = env_path("USERPROFILE").unwrap_or_else(|| drive_root.join("Users\\Default"));
    let local = env_path("LOCALAPPDATA").unwrap_or_else(|| home.join("AppData\\Local"));
    let roaming = env_path("APPDATA").unwrap_or_else(|| home.join("AppData\\Roaming"));
    let temp = env_path("TEMP").unwrap_or_else(|| local.join("Temp"));
    let windir = env_path("SystemRoot").or_else(|| env_path("windir")).unwrap_or_else(|| drive_root.join("Windows"));
    let program_data = env_path("ProgramData").unwrap_or_else(|| drive_root.join("ProgramData"));
    let mut program_files: Vec<PathBuf> =
        ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"].iter().filter_map(|v| env_path(v)).collect();
    program_files.dedup();

    let mut protected = vec![
        drive_root.clone(),
        drive_root.join("Users"),
        home.clone(),
        local.clone(),
        roaming.clone(),
        temp.clone(),
        local.join("Temp"),
        local.join("Microsoft"),
        local.join("Packages"),
        roaming.join("Microsoft"),
        windir.clone(),
        windir.join("System32"),
        windir.join("SysWOW64"),
        windir.join("WinSxS"),
        program_data.clone(),
    ];
    protected.extend(program_files.iter().cloned());
    // Compare against real long names (TEMP is often an 8.3 short path).
    let protected = protected.into_iter().map(|p| canonical(&p).unwrap_or(p)).collect();

    Ctx {
        is_admin: is_elevated(),
        elevated_for_user: false,
        user: env::var("USERNAME").unwrap_or_else(|_| "user".into()),
        log_dir: local.join("CacheKiller"),
        log_owner: None,
        protected,
        tmp_min_age_secs,
        home,
        os: OsDirs { local, roaming, temp, windir, program_data, program_files },
    }
}

/// NUL-terminated wide path with the `\\?\` prefix so paths beyond MAX_PATH work.
fn wide(p: &Path) -> Vec<u16> {
    let disk = matches!(p.components().next(), Some(Component::Prefix(pr)) if matches!(pr.kind(), Prefix::Disk(_)));
    let mut v: Vec<u16> = if disk { r"\\?\".encode_utf16().collect() } else { Vec::new() };
    v.extend(p.as_os_str().encode_wide().map(|c| if c == '/' as u16 { '\\' as u16 } else { c }));
    v.push(0);
    v
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful CreateFileW.
        unsafe { CloseHandle(self.0) };
    }
}

fn open(path: &Path, access: u32) -> io::Result<Handle> {
    let w = wide(path);
    // SAFETY: `w` is NUL-terminated and outlives the call.
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE { Err(io::Error::last_os_error()) } else { Ok(Handle(h)) }
}

/// FILETIME (100 ns ticks since 1601) → unix seconds.
fn unix_secs(t: i64) -> i64 {
    t / 10_000_000 - 11_644_473_600
}

fn kind_of(attrs: u32) -> EntryKind {
    let dir = attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
    if dir && attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        EntryKind::Link // junction, directory symlink, mounted volume: never entered
    } else if dir {
        EntryKind::Dir
    } else {
        EntryKind::File
    }
}

pub fn stat(path: &Path) -> io::Result<Entry> {
    let h = open(path, FILE_READ_ATTRIBUTES)?;
    // SAFETY: out-structs are plain data, sized correctly for each call.
    unsafe {
        let mut bh: BY_HANDLE_FILE_INFORMATION = zeroed();
        if GetFileInformationByHandle(h.0, &mut bh) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut st: FILE_STANDARD_INFO = zeroed();
        if GetFileInformationByHandleEx(
            h.0,
            FileStandardInfo,
            &mut st as *mut _ as *mut c_void,
            size_of::<FILE_STANDARD_INFO>() as u32,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let ft = |f: windows_sys::Win32::Foundation::FILETIME| ((f.dwHighDateTime as i64) << 32) | f.dwLowDateTime as i64;
        let kind = kind_of(bh.dwFileAttributes);
        Ok(Entry {
            path: path.to_path_buf(),
            kind,
            alloc: st.AllocationSize.max(0) as u64,
            size: st.EndOfFile.max(0) as u64,
            newest: unix_secs(ft(bh.ftLastWriteTime).max(ft(bh.ftCreationTime))),
            dev: 0,
            hardlink: (kind == EntryKind::File && bh.nNumberOfLinks > 1).then(|| {
                (
                    bh.dwVolumeSerialNumber as u64,
                    ((bh.nFileIndexHigh as u64) << 32) | bh.nFileIndexLow as u64,
                )
            }),
            owner: 0,
            attrs: bh.dwFileAttributes,
        })
    }
}

/// Children of `dir` plus the number of entries that couldn't be read.
pub fn read_dir(dir: &Path) -> io::Result<(Vec<Entry>, u64)> {
    let h = open(dir, FILE_LIST_DIRECTORY)?;
    let mut buf = vec![0u64; 16 * 1024]; // 128 KiB, 8-byte aligned
    let mut out = Vec::new();
    loop {
        // SAFETY: buffer is valid for its full byte length.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                h.0,
                FileFullDirectoryInfo,
                buf.as_mut_ptr() as *mut c_void,
                (buf.len() * 8) as u32,
            )
        };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            if err == ERROR_NO_MORE_FILES {
                break;
            }
            return Err(io::Error::from_raw_os_error(err as i32));
        }
        let base = buf.as_ptr() as *const u8;
        let mut off = 0usize;
        loop {
            // SAFETY: the kernel filled a chain of FILE_FULL_DIR_INFO records inside `buf`;
            // NextEntryOffset keeps us within it and records are 8-byte aligned.
            let (next, entry) = unsafe {
                let info = base.add(off) as *const FILE_FULL_DIR_INFO;
                let name_ptr = addr_of!((*info).FileName) as *const u16;
                let name = std::slice::from_raw_parts(name_ptr, (*info).FileNameLength as usize / 2);
                let entry = if name == [46] || name == [46, 46] {
                    None
                } else {
                    let attrs = (*info).FileAttributes;
                    let kind = kind_of(attrs);
                    Some(Entry {
                        path: dir.join(String::from_utf16_lossy(name)),
                        kind,
                        alloc: if kind == EntryKind::File { (*info).AllocationSize.max(0) as u64 } else { 0 },
                        size: (*info).EndOfFile.max(0) as u64,
                        newest: unix_secs((*info).LastWriteTime.max((*info).ChangeTime)),
                        dev: 0,
                        hardlink: None,
                        owner: 0,
                        attrs,
                    })
                };
                ((*info).NextEntryOffset as usize, entry)
            };
            out.extend(entry);
            if next == 0 {
                break;
            }
            off += next;
        }
    }
    Ok((out, 0))
}

/// `canonicalize` without the `\\?\` prefix, so paths stay readable and comparable.
pub fn canonical(p: &Path) -> Option<PathBuf> {
    let c = fs::canonicalize(p).ok()?;
    let s = c.to_string_lossy();
    Some(match s.strip_prefix(r"\\?\") {
        Some(rest) if rest.starts_with(r"UNC\") => PathBuf::from(format!(r"\\{}", &rest[4..])),
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => c,
    })
}

/// False if a junction/symlink was swapped into the path since it was scanned.
pub fn path_is_real(p: &Path) -> io::Result<bool> {
    let c = canonical(p).ok_or_else(|| io::Error::other("cannot resolve path"))?;
    Ok(c.to_string_lossy().to_lowercase() == p.to_string_lossy().to_lowercase())
}

fn system_owned(p: &Path, ctx: &Ctx) -> bool {
    let o = &ctx.os;
    p.starts_with(&o.windir) || p.starts_with(&o.program_data) || o.program_files.iter().any(|pf| p.starts_with(pf))
}

/// Can entries inside `dir` be removed by us? System folders need an elevated process.
pub fn dir_writable(dir: &Path, ctx: &Ctx) -> bool {
    ctx.is_admin || !system_owned(dir, ctx)
}

pub fn can_unlink(e: &Entry, ctx: &Ctx) -> bool {
    e.path.parent().is_some_and(|p| dir_writable(p, ctx))
}

fn clear_readonly(e: &Entry) -> bool {
    let w = wide(&e.path);
    let attrs = match e.attrs & !FILE_ATTRIBUTE_READONLY {
        0 => FILE_ATTRIBUTE_NORMAL,
        a => a,
    };
    // SAFETY: `w` is NUL-terminated.
    unsafe { SetFileAttributesW(w.as_ptr(), attrs) != 0 }
}

/// Remove one entry (a file, a link, or an already emptied directory).
/// Read-only files are made writable first; links are removed, never their targets.
pub fn remove(e: &Entry) -> io::Result<()> {
    let as_dir = e.kind == EntryKind::Dir || (e.kind == EntryKind::Link && e.attrs & FILE_ATTRIBUTE_DIRECTORY != 0);
    let go = || if as_dir { fs::remove_dir(&e.path) } else { fs::remove_file(&e.path) };
    match go() {
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied && e.attrs & FILE_ATTRIBUTE_READONLY != 0 => {
            if clear_readonly(e) { go() } else { Err(err) }
        }
        r => r,
    }
}

pub fn tmp_protected(_name: &str, _kind: EntryKind) -> bool {
    false
}

/// Start an elevated copy through UAC, then exit this one. Returns only on failure.
pub fn relaunch_elevated(args: &[String]) -> io::Error {
    let exe = match env::current_exe() {
        Ok(e) => e,
        Err(e) => return e,
    };
    let params = args.iter().map(|a| format!("\"{}\"", a.replace('"', "\\\""))).collect::<Vec<_>>().join(" ");
    let w = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (verb, file, params) = (w("runas"), w(&exe.to_string_lossy()), w(&params));
    // SAFETY: all strings are NUL-terminated and outlive the call.
    let r = unsafe { ShellExecuteW(null_mut(), verb.as_ptr(), file.as_ptr(), params.as_ptr(), null(), SW_SHOWNORMAL) };
    if r as isize > 32 {
        std::process::exit(0);
    }
    io::Error::other("elevation was cancelled")
}

pub fn chown(_p: &Path, _owner: Option<(u32, u32)>) {}

/// (bytes, items) currently in the Recycle Bin on all drives.
pub fn recycle_bin_stats() -> Option<(u64, u64)> {
    // SAFETY: cbSize is set as the API requires.
    unsafe {
        let mut q: SHQUERYRBINFO = zeroed();
        q.cbSize = size_of::<SHQUERYRBINFO>() as u32;
        if SHQueryRecycleBinW(null(), &mut q) != 0 {
            return None;
        }
        let (size, n) = (q.i64Size, q.i64NumItems);
        Some((size.max(0) as u64, n.max(0) as u64))
    }
}

pub fn recycle_bin_empty() -> io::Result<()> {
    // SAFETY: null root = all drives; flags suppress UI.
    let hr = unsafe { SHEmptyRecycleBinW(null_mut(), null(), SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND) };
    // E_UNEXPECTED just means it was already empty.
    if hr == 0 || hr == 0x8000_FFFFu32 as i32 {
        Ok(())
    } else {
        Err(io::Error::other(format!("SHEmptyRecycleBin failed (HRESULT {:#010x})", hr as u32)))
    }
}
