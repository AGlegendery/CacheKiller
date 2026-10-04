#!/usr/bin/env bash
# Rebuild the portable binaries shipped in bin/ — run this before committing a new version.
#   bin/cachekiller-linux-x86_64        fully static (musl), runs on any x86_64 distro
#   bin/cachekiller-windows-x86_64.exe  Windows 10/11, no runtime needed
# Needs podman or docker. Pass "linux" or "windows" to build just one.
set -euo pipefail
cd "$(dirname "$(readlink -f "$0")")"
engine="$(command -v podman || command -v docker || true)"
[[ -n $engine ]] || { echo "podman or docker is required" >&2; exit 1; }
want="${1:-all}"

mkdir -p bin "$HOME/.cargo/registry"
image=localhost/cachekiller-build

# One-time: an Alpine Rust image with musl + mingw + the Windows target, so later
# builds need no network at all.
if ! "$engine" image exists "$image" 2>/dev/null; then
    echo "Creating build image $image (one time)…"
    "$engine" rm -f ck-builder >/dev/null 2>&1 || true
    "$engine" run --name ck-builder --network host docker.io/library/rust:alpine sh -c \
        'apk add --no-cache musl-dev mingw-w64-gcc >/dev/null && rustup target add x86_64-pc-windows-gnu'
    "$engine" commit ck-builder "$image" >/dev/null
    "$engine" rm -f ck-builder >/dev/null
fi

# Host network so a local HTTP(S)_PROXY on 127.0.0.1 keeps working inside the container.
run() {
    "$engine" run --rm --network host \
        -v "$PWD":/src:Z -w /src \
        -v "$HOME/.cargo/registry":/usr/local/cargo/registry:Z \
        "$image" sh -c "$1"
}

if [[ $want == all || $want == linux ]]; then
    run 'CARGO_TARGET_DIR=target/musl cargo build --release'
    install -m 755 target/musl/release/cachekiller bin/cachekiller-linux-x86_64
fi
if [[ $want == all || $want == windows ]]; then
    run 'CARGO_TARGET_DIR=target/win cargo build --release --target x86_64-pc-windows-gnu'
    install -m 755 target/win/x86_64-pc-windows-gnu/release/cachekiller.exe bin/cachekiller-windows-x86_64.exe
fi
ls -lh bin/
