#!/usr/bin/env bash
# CacheKiller launcher — clone the repo, then just run:  ./cachekiller.sh
#   ./cachekiller.sh            open the TUI
#   ./cachekiller.sh --sudo     open the TUI as root (also cleans apt, journal, logs, crash dumps)
#   ./cachekiller.sh --install  add `cachekiller` to ~/.local/bin and the app menu
#   ./cachekiller.sh --build    force a build from source (needs Rust)
#   any other flag is passed to cachekiller (e.g. --list, --min-age 48)
set -euo pipefail
here="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)"
cd "$here"

arch="$(uname -m)"
prebuilt="bin/cachekiller-linux-$arch"
built="target/release/cachekiller"

build() {
    if ! command -v cargo >/dev/null 2>&1; then
        echo "No prebuilt binary for '$arch' and Rust is not installed." >&2
        echo "Install Rust (https://rustup.rs) and run this again:" >&2
        echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
        exit 1
    fi
    echo "Building CacheKiller from source (first run only, ~1 min)…" >&2
    cargo build --release --quiet >&2
    echo "$built"
}

pick() {
    if [[ ${force_build:-0} == 1 ]]; then build; return; fi
    # Fresh local build wins (you're hacking on it); else the shipped static binary.
    if [[ -x $built ]] && [[ -z "$(find src Cargo.toml -newer "$built" -print -quit 2>/dev/null)" ]]; then
        echo "$built"
    elif [[ -x $prebuilt ]] && "$prebuilt" --version >/dev/null 2>&1; then
        echo "$prebuilt"
    else
        build
    fi
}

install_local() {
    local bin; bin="$(pick)"
    mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications"
    install -m 755 "$bin" "$HOME/.local/bin/cachekiller"
    cat > "$HOME/.local/share/applications/cachekiller.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=CacheKiller
Comment=Fast, accurate cache cleaner
Exec=$HOME/.local/bin/cachekiller
Icon=user-trash-full
Terminal=true
Categories=System;Filesystem;
Keywords=cache;clean;junk;disk;
DESKTOP
    echo "Installed: ~/.local/bin/cachekiller  (+ CacheKiller in your app menu)"
    case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) echo "Note: add ~/.local/bin to your PATH to run 'cachekiller' from anywhere.";; esac
}

args=()
as_root=0
for a in "$@"; do
    case "$a" in
        --install) install_local; exit 0 ;;
        --build)   force_build=1 ;;
        --sudo)    as_root=1 ;;
        *)         args+=("$a") ;;
    esac
done

bin="$(pick)"
if [[ $as_root == 1 && $EUID -ne 0 ]]; then
    exec sudo "$here/$bin" "${args[@]}"
fi
exec "$here/$bin" "${args[@]}"
