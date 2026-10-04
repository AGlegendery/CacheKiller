//! Linux cache locations.

use std::path::{Path, PathBuf};

use super::{Builder, Category, expand};

fn is_rotated_log(name: &str) -> bool {
    const SUFFIX: &[&str] = &[".gz", ".xz", ".bz2", ".zst", ".lz4", ".old", ".journal~"];
    if SUFFIX.iter().any(|s| name.ends_with(s)) {
        return true;
    }
    // syslog.1, kern.log.3
    if let Some((_, last)) = name.rsplit_once('.')
        && !last.is_empty()
        && last.len() <= 3
        && last.bytes().all(|b| b.is_ascii_digit())
    {
        return true;
    }
    // messages-20260901
    if let Some((_, last)) = name.rsplit_once('-') {
        return last.len() == 8 && last.bytes().all(|b| b.is_ascii_digit());
    }
    false
}

fn is_archived_journal(name: &str) -> bool {
    name.contains('@') && (name.ends_with(".journal") || name.ends_with(".journal~"))
}

pub fn add_all(b: &mut Builder) {
    let ctx = b.ctx;
    let os = &ctx.os;
    let h = ctx.home.clone();
    let data = os.data_home.clone();
    let p = |s: &str| h.join(s);

    // ---- User cache (XDG): every app's cache folder is an item
    b.children_of(&os.cache_home, Category::Cache, false);
    if ctx.is_admin && h != Path::new("/root") {
        b.children_of(Path::new("/root/.cache"), Category::Cache, false);
    }
    b.contents(&p(".thumbnails"), Category::Cache);

    // ---- Temp
    b.temp_dir(Path::new("/tmp"));
    b.temp_dir(Path::new("/var/tmp"));

    // ---- Trash (home + removable drives)
    let trash_dirs = |t: &Path| [t.join("files"), t.join("info"), t.join("expunged")];
    let trash = data.join("Trash");
    b.group(&trash, &trash_dirs(&trash), Category::Trash, Some("trash bin".into()));
    for pat in [
        format!("/media/{}/*/.Trash-{}", ctx.user, os.uid),
        format!("/run/media/{}/*/.Trash-{}", ctx.user, os.uid),
        format!("/mnt/*/.Trash-{}", os.uid),
    ] {
        for t in expand(Path::new(&pat)) {
            b.group(&t, &trash_dirs(&t), Category::Trash, Some("trash bin".into()));
        }
    }

    // ---- Browsers & Electron apps (Chrome, Brave, Edge, VS Code, Discord, Slack, Teams, …)
    b.chromium_apps(&os.config_home, 2, &[]);
    for cfg in expand(&p(".var/app/*/config")) {
        b.chromium_apps(&cfg, 2, &[]);
    }
    for pat in [
        ".mozilla/firefox/*",
        ".var/app/org.mozilla.firefox/.mozilla/firefox/*",
        "snap/firefox/common/.mozilla/firefox/*",
        ".librewolf/*",
        ".waterfox/*",
        ".thunderbird/*",
        ".var/app/org.mozilla.Thunderbird/.thunderbird/*",
    ] {
        b.mozilla_profiles(&p(pat));
    }

    // ---- Sandboxed apps
    for d in expand(&p(".var/app/*/cache")) {
        b.contents(&d, Category::App);
    }
    for d in expand(&p("snap/*/*/.cache")) {
        b.contents(&d, Category::App);
    }

    // ---- Messengers
    for pat in [
        data.join("TelegramDesktop/tdata/user_data*"),
        p(".var/app/org.telegram.desktop/data/TelegramDesktop/tdata/user_data*"),
        p("snap/telegram-desktop/current/.local/share/TelegramDesktop/tdata/user_data*"),
    ] {
        for d in expand(&pat) {
            b.group(&d, &[d.join("cache"), d.join("media_cache")], Category::App, Some("Telegram media cache".into()));
        }
    }

    // ---- Developer tools
    b.group(&p(".npm"), &[p(".npm/_cacache"), p(".npm/_logs"), p(".npm/_npx")], Category::Dev, Some("npm cache".into()));
    b.group(
        &p(".cargo"),
        &[p(".cargo/registry/cache"), p(".cargo/registry/src"), p(".cargo/git/checkouts")],
        Category::Dev,
        Some("cargo download cache".into()),
    );
    for rel in [
        ".gradle/caches",
        ".gradle/daemon",
        ".yarn/berry/cache",
        ".bun/install/cache",
        ".android/cache",
        ".conda/pkgs",
        "miniconda3/pkgs",
        "anaconda3/pkgs",
        ".dotnet/TelemetryStorageService",
        ".nv/ComputeCache",
    ] {
        b.contents(&p(rel), Category::Dev);
    }
    b.contents(&data.join("pnpm/store"), Category::Dev);
    for d in expand(&data.join("JetBrains/*/log")) {
        b.contents(&d, Category::Dev);
    }

    // ---- Games
    let steams: [PathBuf; 4] = [
        data.join("Steam"),
        p(".steam/steam"),
        p(".steam/debian-installation"),
        p(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
    ];
    for steam in steams {
        b.contents(&steam.join("appcache/httpcache"), Category::Game);
        b.contents(&steam.join("logs"), Category::Game);
        b.children_of(&steam.join("steamapps/shadercache"), Category::Game, false);
    }

    // ---- User logs
    b.files(&h, false, |n| n.starts_with(".xsession-errors"), &[], Category::Log, "X session error logs");
    b.files(&data.join("xorg"), false, |n| n.ends_with(".old"), &[], Category::Log, "old Xorg logs");

    // ---- System (most need root to delete)
    let sys = |s: &str| PathBuf::from(s);
    b.files(&sys("/var/cache/apt/archives"), true, |n| n.ends_with(".deb"), &[], Category::Package, "downloaded .deb packages");
    b.files(&sys("/var/cache/apt"), false, |n| n.ends_with(".bin"), &[], Category::Package, "apt package cache index");
    b.files(&sys("/var/cache/pacman/pkg"), false, |n| n.contains(".pkg.tar"), &[], Category::Package, "pacman packages");
    for d in ["/var/cache/dnf", "/var/cache/yum", "/var/cache/zypp/packages", "/var/cache/PackageKit", "/var/lib/snapd/cache"] {
        b.contents(Path::new(d), Category::Package);
    }
    b.files(&sys("/var/log"), true, is_rotated_log, &["journal"], Category::Log, "rotated / compressed logs");
    b.files(&sys("/var/log/journal"), true, is_archived_journal, &[], Category::Log, "archived journal files");
    for d in ["/var/crash", "/var/lib/systemd/coredump", "/var/lib/apport/coredump"] {
        b.contents(Path::new(d), Category::Crash);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotated() {
        for n in ["syslog.1", "kern.log.2.gz", "messages-20260901", "Xorg.0.log.old", "dpkg.log.12"] {
            assert!(is_rotated_log(n), "{n}");
        }
        for n in ["syslog", "kern.log", "Xorg.0.log", "auth.log", "boot.log", "lastlog", "wtmp"] {
            assert!(!is_rotated_log(n), "{n}");
        }
        assert!(is_archived_journal("system@0006a7-abc.journal"));
        assert!(!is_archived_journal("system.journal"));
    }
}
