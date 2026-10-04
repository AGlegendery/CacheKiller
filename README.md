# 🗑️ CacheKiller

A fast, accurate, terminal-based cache cleaner for **Linux and Windows**. Each one is a single native binary with nothing to install.

## Run it

**Linux:** clone it and run:
```bash
git clone https://github.com/AGlegendery/CacheKiller && cd CacheKiller
./cachekiller.sh
```
Or double-click `CacheKiller.desktop`.

**Arch Linux:** install it from the AUR, or with the package attached to the release:
```bash
yay -S cachekiller-bin                                   # or: paru -S cachekiller-bin
sudo pacman -U cachekiller-bin-9.0.0-1-x86_64.pkg.tar.zst   # package from the release
```
After that, run `cachekiller`, or open **CacheKiller** from the app menu.

**Windows:** clone it (or download the ZIP), then double-click **`CacheKiller.cmd`**. You can also run `bin\cachekiller-windows-x86_64.exe` directly.
The exe isn't code-signed, so the first time SmartScreen may show *"Windows protected your PC"*. Click **More info → Run anyway**.

No Rust, .NET or PowerShell is needed: `bin/` holds prebuilt native binaries.
The same binaries are attached to every [release](https://github.com/AGlegendery/CacheKiller/releases/latest) if you just want the file.

---

## Why it's fast and accurate

| | |
|---|---|
| **Parallel scanner** | Walks every cache location on all CPU cores at once (rayon work-stealing). On Linux it scans about 115k files in under 0.1 s once they are in the OS cache. |
| **Exact sizes** | Linux: allocated blocks, identical to `du`. Windows: the real NTFS allocation size (Explorer's *size on disk*), read in bulk per directory with `GetFileInformationByHandleEx`, so no file is opened one by one. |
| **Only real savings** | Hard-linked files are counted once and *not* as reclaimable, because deleting one link frees nothing. |
| **Honest about permissions** | Bytes you can't delete without root/admin are shown as **locked**, not promised as free space. Press `E` to restart as `sudo` / Administrator (UAC). |
| **Never escapes** | It never follows symlinks or junctions, never crosses into another filesystem or mounted volume, and re-checks every path right before deleting it. Protected system folders are hard-blocked. |
| **Safe temp cleaning** | Temp entries changed in the last 24 h are left alone, as are live sockets and session dirs on Linux. Read-only files on Windows are handled, and files in use are reported, not forced. |

## What it finds

**Both:** browser caches (Chrome, Edge, Brave, Vivaldi, Opera, Firefox, LibreWolf, Thunderbird); **auto-detected Electron apps** (VS Code, Discord, Slack, Teams, Spotify …); Telegram media cache; dev caches (npm, cargo, pip, yarn, pnpm, bun, gradle, NuGet, Go, conda, JetBrains logs); Steam / Epic caches and logs; crash dumps; trash.

**Linux:** every app in `~/.cache`, thumbnails, Flatpak & Snap caches, `/tmp`, `/var/tmp`. With sudo: apt `.deb` archives, pacman/dnf/zypper caches, rotated logs, archived journal files, core dumps.

**Windows:** `%TEMP%`, `Windows\Temp`, service-profile temp, Windows Update download cache, Delivery Optimization, INetCache, thumbnail/icon cache, DirectX/NVIDIA/AMD/Intel shader caches, Store app temp caches, Windows logs, WER reports, minidumps and `MEMORY.DMP`, and the **Recycle Bin** (all drives).

Deliberately **not** touched on Windows, even though other cleaners delete them: `ProgramData\Package Cache` (needed to repair and uninstall programs), `Prefetch` (deleting it slows boot), `Installer` and `WinSxS` (use DISM for those), `OfficeFileCache` (can hold OneDrive edits that haven't synced yet), and `Packages\*\LocalCache` (some Store apps keep real data there).

## Keys

| Key | Action |
|---|---|
| `↑ ↓` `j k` `PgUp PgDn` `g G` | Move |
| `Space` | Select / unselect |
| `Enter` | Delete the selected items (asks to confirm first) |
| `A` / `U` | Select all visible items / unselect all |
| `L` | Select items over 200 MB |
| `C` | Select the whole category of the current item |
| `Tab` | Filter by category |
| `S` | Sort by size, age, category or path |
| `B` | Show reclaimable, disk or apparent size |
| `R` | Rescan |
| `E` | Restart as sudo / Administrator |
| `?` | Help |
| `q` | Quit |

## Options

```text
cachekiller --list            print a report without opening the TUI (also used automatically when output is piped)
cachekiller --min-age 48      temp entries must be untouched for 48h before they're offered
./cachekiller.sh --sudo       (Linux) start as root
./cachekiller.sh --install    (Linux) add `cachekiller` to ~/.local/bin and to your app menu
```

## Logs

Linux: `~/.local/state/cachekiller/` (owned by you, even under sudo). Windows: `%LOCALAPPDATA%\CacheKiller\`.
Both use `delete.log` and `failed.log`.

## Building

```bash
cargo build --release      # native build for the current OS
cargo test
./build-static.sh          # rebuild both bin/ binaries (static Linux + Windows .exe; needs podman or docker)
```

Arch package: `packaging/aur/` holds the `PKGBUILD` and `.SRCINFO` for `cachekiller-bin` (it installs the release binary). For a new version:
1. Bump `pkgver` and the checksums (`updpkgsums`).
2. Run `makepkg --printsrcinfo > .SRCINFO`.
3. Push both files to `ssh://aur@aur.archlinux.org/cachekiller-bin.git`.

Code layout: `src/platform/` holds the OS layer (`unix.rs`, `windows.rs`) and `src/locations/` holds the cache lists (`linux.rs`, `windows.rs`). The UI, scanner and safe deleter are shared.

## License

MIT, by AGlegend
