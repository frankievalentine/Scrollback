#!/bin/sh
# Checks, from a Mac, that Scrollback still builds for Linux and Windows:
# runs the tests on Linux and builds every crate for Windows, inside a
# Rust container. Needs Docker. The first run takes several minutes; later
# ones reuse what is kept in the three "scrollback-linux-*" Docker volumes.
#
# This is a stand-in for CI while working locally. The Windows half builds
# and links the programs with the GNU toolchain, icon and all, but cannot
# run them, and does not compile GPUI's shaders the way a release build on
# Windows does.
set -eu
cd "$(dirname "$0")/.."

docker run --rm \
    -v "$PWD":/src:ro \
    -v scrollback-linux-target:/target \
    -v scrollback-linux-cargo:/usr/local/cargo/registry \
    -v scrollback-linux-rustup:/usr/local/rustup \
    -e CARGO_TARGET_DIR=/target \
    -w /src rust:1-bookworm sh -eu -c '
        apt-get update -qq >/dev/null
        DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
            pkg-config cmake clang \
            libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
            libx11-xcb-dev libxcb1-dev libfontconfig-dev libfreetype-dev \
            libssl-dev libzstd-dev libvulkan-dev libasound2-dev \
            gcc-mingw-w64-x86-64 g++-mingw-w64-x86-64 >/dev/null 2>&1
        echo "== Linux: build and test"
        cargo test --workspace --locked
        echo "== Windows: build"
        rustup target add x86_64-pc-windows-gnu >/dev/null 2>&1
        cargo build --workspace --locked --target x86_64-pc-windows-gnu
    '
echo "Linux tests pass and the Windows build links."
