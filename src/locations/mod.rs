//! Where cache lives, and how each location is turned into deletable items.
//!
//! Three shapes of location:
//! * children  – every entry of a directory is its own item (~/.cache, /tmp, %TEMP%)
//! * contents  – one item that empties one or more directories but keeps them
//! * files     – one item grouping files that match a rule (*.deb, rotated logs, thumbcache)
//!
//! The per-OS lists live in `linux.rs` and `windows.rs`.

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::platform::{self, Ctx, EntryKind};

#[cfg(unix)]
mod linux;
#[cfg(unix)]
use linux::add_all;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows::add_all;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum Category {
    Temp,
    Cache,
    Browser,
    App,
    Dev,
    Game,
    Package,
    Log,
    Crash,
    Trash,
}

impl Category {
    pub const ALL: [Category; 10] = [
        Category::Temp,
        Category::Cache,
        Category::Browser,
        Category::App,
        Category::Dev,
        Category::Game,
        Category::Package,
        Category::Log,
        Category::Crash,
        Category::Trash,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Category::Temp => "Temp",
            Category::Cache => "Cache",
            Category::Browser => "Browser",
            Category::App => "App",
            Category::Dev => "Dev",
            Category::Game => "Game",
            Category::Package => "Package",
            Category::Log => "Log",
            Category::Crash => "Crash",
            Category::Trash => "Trash",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Dir,
    File,
    Link,
    Group,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Dir => "DIR",
            Kind::File => "FILE",
            Kind::Link => "LINK",
            Kind::Group => "GRP",
        }
    }
}

/// Items cleaned through an OS API instead of by deleting paths.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(unix, allow(dead_code))]
pub enum Special {
    RecycleBin,
}

#[derive(Debug)]
pub struct Candidate {
    pub path: PathBuf,
    pub note: Option<String>,
    pub category: Category,
    pub kind: Kind,
    /// Paths removed entirely (recursively) when the item is cleaned.
    pub targets: Vec<PathBuf>,
    /// Every target must live strictly inside one of these.
    pub roots: Vec<PathBuf>,
    /// Subject to the temp minimum-age rule.
    pub tmp: bool,
    pub special: Option<Special>,
}

/// Simple `*` wildcard match within a single path component.
fn wild_match(pat: &str, name: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == name;
    }
    let mut rest = name;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(part) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            match rest.find(part) {
                Some(pos) => rest = &rest[pos + part.len()..],
                None => return false,
            }
        }
    }
    true
}

/// Expand a path pattern with `*` components. Symlinked directories / junctions are
/// never traversed by a wildcard, which avoids duplicates (`~/snap/x/current`) and escapes.
pub fn expand(pattern: &Path) -> Vec<PathBuf> {
    let comps: Vec<Component> = pattern.components().collect();
    let mut cur = vec![PathBuf::new()];
    for (i, comp) in comps.iter().enumerate() {
        let last = i + 1 == comps.len();
        let name = comp.as_os_str().to_string_lossy();
        let mut next = Vec::new();
        for base in &cur {
            if matches!(comp, Component::Normal(_)) && name.contains('*') {
                let Ok(rd) = fs::read_dir(base) else { continue };
                for e in rd.flatten() {
                    let n = e.file_name();
                    let Some(n) = n.to_str() else { continue };
                    if !wild_match(&name, n) {
                        continue;
                    }
                    let Ok(ft) = e.file_type() else { continue };
                    if ft.is_dir() || (last && !ft.is_symlink()) {
                        next.push(e.path());
                    }
                }
            } else {
                let p = base.join(comp);
                if !matches!(comp, Component::Normal(_)) || fs::symlink_metadata(&p).is_ok() {
                    next.push(p);
                }
            }
        }
        cur = next;
        if cur.is_empty() {
            break;
        }
    }
    cur.sort();
    cur
}

const CHROMIUM_CACHE_DIRS: &[&str] = &[
    "Cache",
    "Code Cache",
    "GPUCache",
    "DawnCache",
    "DawnGraphiteCache",
    "DawnWebGPUCache",
    "GrShaderCache",
    "GraphiteDawnCache",
    "ShaderCache",
    "Service Worker/CacheStorage",
    "Service Worker/ScriptCache",
    "CachedData",
    "CachedExtensionVSIXs",
    "CachedProfilesData",
    "Crashpad/completed",
    "Crashpad/pending",
    "Crashpad/reports",
    "component_crx_cache",
    "extensions_crx_cache",
    "logs",
];

const CHROMIUM_MARKERS: &[&str] = &["GPUCache", "Code Cache", "DawnCache", "DawnGraphiteCache", "Local State"];

const BROWSER_NAMES: &[&str] =
    &["chrome", "chromium", "brave", "edge", "vivaldi", "opera", "yandex", "thorium"];

const MOZILLA_CACHE_DIRS: &[&str] = &[
    "cache2",
    "startupCache",
    "thumbnails",
    "shader-cache",
    "minidumps",
    "crashes/events",
    "saved-telemetry-pings",
    "datareporting/archived",
];

/// Entries of `dir`, skipping anything on another filesystem (mount points).
fn list_children(dir: &Path) -> Vec<platform::Entry> {
    let Ok(d) = platform::stat(dir) else { return Vec::new() };
    let Ok((entries, _)) = platform::read_dir(dir) else { return Vec::new() };
    entries.into_iter().filter(|e| !(e.kind == EntryKind::Dir && e.dev != d.dev)).collect()
}

fn canonical_dir(p: &Path) -> Option<PathBuf> {
    let c = platform::canonical(p)?;
    c.is_dir().then_some(c)
}

fn is_real_dir(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
}

pub struct Builder<'a> {
    pub ctx: &'a Ctx,
    out: Vec<Candidate>,
}

impl Builder<'_> {
    fn push(&mut self, c: Candidate) {
        if !c.targets.is_empty() || c.special.is_some() {
            self.out.push(c);
        }
    }

    /// Each entry of `dir` becomes an item.
    pub fn children_of(&mut self, dir: &Path, category: Category, tmp: bool) {
        let Some(dir) = canonical_dir(dir) else { return };
        for e in list_children(&dir) {
            let name = e.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if tmp && platform::tmp_protected(&name, e.kind) {
                continue;
            }
            let kind = match e.kind {
                EntryKind::Link => Kind::Link,
                EntryKind::Dir => Kind::Dir,
                _ => Kind::File,
            };
            self.push(Candidate {
                targets: vec![e.path.clone()],
                path: e.path,
                note: None,
                category,
                kind,
                roots: vec![dir.clone()],
                tmp,
                special: None,
            });
        }
    }

    /// One item that empties every directory in `dirs` (the directories themselves stay).
    pub fn group(&mut self, display: &Path, dirs: &[PathBuf], category: Category, note: Option<String>) {
        let mut roots = Vec::new();
        let mut targets = Vec::new();
        for d in dirs {
            let Some(d) = canonical_dir(d) else { continue };
            if roots.contains(&d) {
                continue;
            }
            targets.extend(list_children(&d).into_iter().map(|e| e.path));
            roots.push(d);
        }
        let kind = if roots.len() > 1 { Kind::Group } else { Kind::Dir };
        let path = if roots.len() == 1 && note.is_none() { roots[0].clone() } else { display.to_path_buf() };
        self.push(Candidate { path, note, category, kind, targets, roots, tmp: false, special: None });
    }

    pub fn contents(&mut self, dir: &Path, category: Category) {
        self.group(dir, &[dir.to_path_buf()], category, None);
    }

    /// Like `children_of`, but the entries get the temp minimum-age rule.
    pub fn temp_dir(&mut self, dir: &Path) {
        self.children_of(dir, Category::Temp, true);
    }

    /// One item grouping files under `dir` whose name matches `matcher`.
    pub fn files(
        &mut self,
        dir: &Path,
        recursive: bool,
        matcher: fn(&str) -> bool,
        skip_dirs: &[&str],
        category: Category,
        note: &str,
    ) {
        let Some(root) = canonical_dir(dir) else { return };
        let mut targets = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(d) = stack.pop() {
            for e in list_children(&d) {
                let name = e.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                match e.kind {
                    EntryKind::Dir => {
                        if recursive && !skip_dirs.contains(&name.as_str()) {
                            stack.push(e.path);
                        }
                    }
                    EntryKind::File if matcher(&name) => targets.push(e.path),
                    _ => {}
                }
            }
        }
        targets.sort();
        self.push(Candidate {
            path: root.clone(),
            note: Some(note.to_string()),
            category,
            kind: Kind::Group,
            targets,
            roots: vec![root],
            tmp: false,
            special: None,
        });
    }

    #[cfg_attr(unix, allow(dead_code))]
    pub fn special(&mut self, special: Special, path: &Path, category: Category, note: &str) {
        self.push(Candidate {
            path: path.to_path_buf(),
            note: Some(note.into()),
            category,
            kind: Kind::Group,
            targets: Vec::new(),
            roots: Vec::new(),
            tmp: false,
            special: Some(special),
        });
    }

    /// Find Chromium/Electron profiles (browsers, VS Code, Discord, Slack, Teams, …)
    /// up to `depth` levels below `base`, skipping `skip` names at the first level.
    pub fn chromium_apps(&mut self, base: &Path, depth: usize, skip: &[&str]) {
        let Ok(rd) = fs::read_dir(base) else { return };
        for e in rd.flatten() {
            if !e.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            if skip.iter().any(|s| e.file_name().to_string_lossy().eq_ignore_ascii_case(s)) {
                continue;
            }
            let app = e.path();
            if !self.chromium_app(&app) && depth > 1 {
                self.chromium_apps(&app, depth - 1, &[]);
            }
        }
    }

    fn chromium_app(&mut self, app: &Path) -> bool {
        if !CHROMIUM_MARKERS.iter().any(|m| app.join(m).exists()) {
            return false;
        }
        let mut bases = vec![app.to_path_buf()];
        for sub in [app.join("*"), app.join("Partitions").join("*")] {
            bases.extend(expand(&sub));
        }
        let mut dirs = Vec::new();
        for b in &bases {
            for name in CHROMIUM_CACHE_DIRS {
                let d = b.join(name);
                if is_real_dir(&d) {
                    dirs.push(d);
                }
            }
        }
        if dirs.is_empty() {
            return true;
        }
        let lpath = app.to_string_lossy().to_lowercase();
        let category = if BROWSER_NAMES.iter().any(|b| lpath.contains(b)) { Category::Browser } else { Category::App };
        let n = dirs.len();
        self.group(app, &dirs, category, Some(format!("{n} cache dirs")));
        true
    }

    pub fn mozilla_profiles(&mut self, pattern: &Path) {
        for prof in expand(pattern) {
            let dirs: Vec<PathBuf> = MOZILLA_CACHE_DIRS.iter().map(|n| prof.join(n)).filter(|d| d.is_dir()).collect();
            if !dirs.is_empty() {
                let n = dirs.len();
                self.group(&prof, &dirs, Category::Browser, Some(format!("{n} cache dirs")));
            }
        }
    }
}

pub fn resolve(ctx: &Ctx) -> Vec<Candidate> {
    let mut b = Builder { ctx, out: Vec::new() };
    add_all(&mut b);
    dedupe(b.out)
}

/// Drop targets already covered by another candidate (same path or an ancestor),
/// so nothing is counted or deleted twice.
fn dedupe(mut cands: Vec<Candidate>) -> Vec<Candidate> {
    let mut all: Vec<(PathBuf, usize)> = cands
        .iter()
        .enumerate()
        .flat_map(|(i, c)| c.targets.iter().map(move |t| (t.clone(), i)))
        .collect();
    all.sort();
    let mut keep: Vec<HashSet<PathBuf>> = vec![HashSet::new(); cands.len()];
    let mut last: Option<PathBuf> = None;
    for (t, i) in all {
        if let Some(l) = &last
            && t.starts_with(l)
        {
            continue;
        }
        keep[i].insert(t.clone());
        last = Some(t);
    }
    for (c, k) in cands.iter_mut().zip(keep) {
        c.targets.retain(|t| k.contains(t));
    }
    cands.retain(|c| !c.targets.is_empty() || c.special.is_some());
    cands
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard() {
        assert!(wild_match("user_data*", "user_data"));
        assert!(wild_match("user_data*", "user_data#2"));
        assert!(wild_match("*.deb", "foo.deb"));
        assert!(!wild_match("*.deb", "foo.debx"));
        assert!(wild_match("a*b*c", "aXXbYYc"));
    }

    #[test]
    fn dedupe_nested() {
        let c = |t: &[&str]| Candidate {
            path: PathBuf::from("/x"),
            note: None,
            category: Category::Cache,
            kind: Kind::Dir,
            targets: t.iter().map(PathBuf::from).collect(),
            roots: vec![],
            tmp: false,
            special: None,
        };
        let out = dedupe(vec![c(&["/a/b", "/a/b c"]), c(&["/a/b/c", "/a/b", "/z"])]);
        let all: Vec<_> = out.iter().flat_map(|c| c.targets.clone()).collect();
        assert_eq!(all.len(), 3);
    }
}
