//! Windows cache locations.
//!
//! Deliberately NOT included (they look like junk but aren't):
//! * `ProgramData\Package Cache` – installers needed to repair/uninstall programs
//! * `Windows\Prefetch`         – deleting it only makes boot and app start slower
//! * `Windows\Installer`, `WinSxS` – use DISM, never delete by hand
//! * `Office\16.0\OfficeFileCache` – can hold edits not yet synced to OneDrive
//! * `Packages\*\LocalCache`    – some Store apps (e.g. Python) keep real data there
//! * `Recent`, `WebCache`       – history/indexes, not cache; always locked anyway

use std::path::{Path, PathBuf};

use super::{Builder, Category, Special, expand};

fn is_explorer_cache(n: &str) -> bool {
    let n = n.to_ascii_lowercase();
    (n.starts_with("thumbcache_") || n.starts_with("iconcache_")) && n.ends_with(".db")
}

fn is_log_file(n: &str) -> bool {
    let n = n.to_ascii_lowercase();
    [".log", ".etl", ".old", ".bak", ".cab", ".txt", ".lo_"].iter().any(|s| n.ends_with(s))
}

pub fn add_all(b: &mut Builder) {
    let ctx = b.ctx;
    let os = &ctx.os;
    let (h, local, roaming, win, pd) = (&ctx.home, &os.local, &os.roaming, &os.windir, &os.program_data);
    let l = |s: &str| local.join(s);
    let r = |s: &str| roaming.join(s);
    let hp = |s: &str| h.join(s);

    // ---- Temp
    b.temp_dir(&os.temp);
    b.temp_dir(&l("Temp"));
    b.temp_dir(&win.join("Temp"));
    b.temp_dir(&win.join(r"ServiceProfiles\LocalService\AppData\Local\Temp"));
    b.temp_dir(&win.join(r"ServiceProfiles\NetworkService\AppData\Local\Temp"));

    // ---- Recycle Bin (all drives, via the Shell API)
    b.special(Special::RecycleBin, Path::new("Recycle Bin"), Category::Trash, "all drives");

    // ---- Windows Update & delivery
    b.contents(&win.join(r"SoftwareDistribution\Download"), Category::Package);
    b.contents(
        &win.join(r"ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization\Cache"),
        Category::Package,
    );
    b.contents(&pd.join(r"NVIDIA Corporation\Downloader"), Category::Package);

    // ---- Logs
    b.files(&win.join("Logs"), true, is_log_file, &[], Category::Log, "Windows logs");
    b.files(&win.join("Panther"), true, is_log_file, &[], Category::Log, "setup logs");
    b.contents(&pd.join(r"USOShared\Logs"), Category::Log);

    // ---- Crash dumps & error reports
    b.contents(&win.join("Minidump"), Category::Crash);
    b.files(win, false, |n| n.eq_ignore_ascii_case("MEMORY.DMP"), &[], Category::Crash, "kernel memory dump");
    b.contents(&win.join("LiveKernelReports"), Category::Crash);
    for d in ["ReportArchive", "ReportQueue", "Temp"] {
        b.contents(&pd.join(r"Microsoft\Windows\WER").join(d), Category::Crash);
    }
    b.contents(&l("CrashDumps"), Category::Crash);
    b.contents(&l(r"Microsoft\Windows\WER\ReportArchive"), Category::Crash);
    b.contents(&l(r"Microsoft\Windows\WER\ReportQueue"), Category::Crash);

    // ---- System & GPU caches
    b.contents(&l(r"Microsoft\Windows\INetCache"), Category::Cache);
    b.files(
        &l(r"Microsoft\Windows\Explorer"),
        false,
        is_explorer_cache,
        &[],
        Category::Cache,
        "thumbnail & icon cache (in use by Explorer)",
    );
    for d in [
        "D3DSCache",
        r"NVIDIA\DXCache",
        r"NVIDIA\GLCache",
        r"NVIDIA Corporation\NV_Cache",
        r"AMD\DxCache",
        r"AMD\DxcCache",
        r"AMD\VkCache",
        r"AMD\GLCache",
        r"Intel\ShaderCache",
        r"Microsoft\Terminal Server Client\Cache",
    ] {
        b.contents(&l(d), Category::Cache);
    }
    for pat in [r"Packages\*\AC\INetCache", r"Packages\*\AC\Temp", r"Packages\*\TempState"] {
        for d in expand(&l(pat)) {
            b.contents(&d, Category::App);
        }
    }

    // ---- Browsers & Electron apps, auto-detected
    // (Chrome/Edge/Brave live 3 levels down: Vendor\Product\User Data)
    b.chromium_apps(local, 3, &["Temp", "Packages", "CrashDumps", "D3DSCache"]);
    b.chromium_apps(roaming, 2, &[]);
    for pat in [
        r"Mozilla\Firefox\Profiles\*",
        r"Thunderbird\Profiles\*",
        r"LibreWolf\Profiles\*",
        r"Waterfox\Profiles\*",
    ] {
        b.mozilla_profiles(&l(pat));
        b.mozilla_profiles(&r(pat));
    }

    // ---- Apps
    for d in expand(&r(r"Telegram Desktop\tdata\user_data*")) {
        b.group(&d, &[d.join("cache"), d.join("media_cache")], Category::App, Some("Telegram media cache".into()));
    }
    for d in [
        r(r"Spotify\Data"),
        l(r"Spotify\Data"),
        r(r"Zoom\logs"),
        l(r"Zoom\logs"),
        r(r"vlc\art"),
        r(r"Adobe\Common\Media Cache Files"),
        r(r"Adobe\Common\Media Cache"),
        l(r"Adobe\Common\Media Cache Files"),
        l(r"Docker\log"),
    ] {
        b.contents(&d, Category::App);
    }

    // ---- Developer tools
    b.group(
        &l("npm-cache"),
        &[l(r"npm-cache\_cacache"), l(r"npm-cache\_logs"), l(r"npm-cache\_npx")],
        Category::Dev,
        Some("npm cache".into()),
    );
    b.group(
        &hp(".cargo"),
        &[hp(r".cargo\registry\cache"), hp(r".cargo\registry\src"), hp(r".cargo\git\checkouts")],
        Category::Dev,
        Some("cargo download cache".into()),
    );
    for d in [
        l(r"pip\Cache"),
        l(r"Yarn\Cache"),
        l(r"pnpm\store"),
        l(r"NuGet\v3-cache"),
        l(r"NuGet\plugins-cache"),
        l("go-build"),
        l(r"Microsoft\vscode-cpptools\ipch"),
        hp(r".gradle\caches"),
        hp(r".gradle\daemon"),
        hp(r".bun\install\cache"),
        hp(r".yarn\berry\cache"),
        hp(r".conda\pkgs"),
    ] {
        b.contents(&d, Category::Dev);
    }
    for d in expand(&l(r"JetBrains\*\log")) {
        b.contents(&d, Category::Dev);
    }

    // ---- Games
    let mut steams: Vec<PathBuf> = os.program_files.iter().map(|pf| pf.join("Steam")).collect();
    steams.dedup();
    for steam in steams {
        for d in [r"appcache\httpcache", "logs", "dumps"] {
            b.contents(&steam.join(d), Category::Game);
        }
        b.children_of(&steam.join(r"steamapps\shadercache"), Category::Game, false);
    }
    b.contents(&l(r"Steam\htmlcache"), Category::Game);
    for d in expand(&l(r"EpicGamesLauncher\Saved\webcache*")) {
        b.contents(&d, Category::Game);
    }
    b.contents(&l(r"EpicGamesLauncher\Saved\Logs"), Category::Game);
}
