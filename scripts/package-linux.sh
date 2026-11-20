#!/bin/sh
# Packages scrollback for Linux, on Linux: a tarball anyone can unpack into
# their home folder, and a .deb where dpkg is there to build one.
#
#   scripts/package-linux.sh
#
# Leaves, under the build folder's package/ directory:
#   scrollback-<version>-linux-<arch>.tar.gz   unpack, then run ./install.sh
#   scrollback_<version>_<arch>.deb            sudo apt install ./scrollback_*.deb
#
# SCROLLBACK_MAINTAINER sets the .deb's maintainer line; the default is a
# placeholder that should be changed before the package is shared.
set -eu
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
arch=$(uname -m)
target="${CARGO_TARGET_DIR:-target}"
out="$target/package"
name="scrollback-$version-linux-$arch"

cargo build --release --locked -p scrollback-app -p scrollback-cli

# One tree, laid out the way it lands under a prefix such as /usr.
tree="$out/tree"
rm -rf "$out"
mkdir -p "$tree/bin" "$tree/share/applications"
install -m 755 "$target/release/scrollback-app" "$tree/bin/scrollback-app"
install -m 755 "$target/release/scrollback" "$tree/bin/scrollback"
strip "$tree/bin/scrollback-app" "$tree/bin/scrollback" 2>/dev/null || true
install -m 644 packaging/linux/scrollback.desktop "$tree/share/applications/scrollback.desktop"
for size in 16 32 48 64 128 256 512; do
    icons="$tree/share/icons/hicolor/${size}x${size}/apps"
    mkdir -p "$icons"
    install -m 644 "crates/app/icon/png/$size.png" "$icons/scrollback.png"
done

# The tarball: the tree and a script that copies it into place.
mkdir -p "$out/$name"
cp -R "$tree/bin" "$tree/share" "$out/$name/"
install -m 755 packaging/linux/install.sh "$out/$name/install.sh"
tar -C "$out" -czf "$out/$name.tar.gz" "$name"
rm -rf "$out/$name"
echo "Built $out/$name.tar.gz"

# The .deb: the same tree under /usr, with what it needs to run worked out
# from the libraries the programs link to.
if command -v dpkg-deb >/dev/null 2>&1; then
    deb_arch=$(dpkg --print-architecture)
    deb="$out/deb"
    mkdir -p "$deb/DEBIAN" "$deb/usr"
    cp -R "$tree/bin" "$tree/share" "$deb/usr/"
    # dpkg-shlibdeps wants to be run beside a debian/control file.
    needs=""
    if command -v dpkg-shlibdeps >/dev/null 2>&1; then
        work="$out/shlibdeps"
        mkdir -p "$work/debian"
        printf 'Source: scrollback\n\nPackage: scrollback\nArchitecture: any\n' > "$work/debian/control"
        needs=$(cd "$work" && dpkg-shlibdeps -O "../deb/usr/bin/scrollback-app" "../deb/usr/bin/scrollback" 2>/dev/null \
            | sed -n 's/^shlibs:Depends=//p')
        rm -rf "$work"
    fi
    # Vulkan is loaded when the app starts, not linked, so it is named here.
    needs="${needs:+$needs, }libvulkan1"
    size=$(du -sk "$deb/usr" | cut -f1)
    cat > "$deb/DEBIAN/control" <<CONTROL
Package: scrollback
Version: $version
Section: devel
Priority: optional
Architecture: $deb_arch
Maintainer: ${SCROLLBACK_MAINTAINER:-scrollback <scrollback@localhost>}
Installed-Size: $size
Depends: $needs
Recommends: libwayland-client0, mesa-vulkan-drivers
Description: Every coding-agent session on this computer in one place
 scrollback gathers the sessions of Claude Code, Codex, OpenCode, pi and
 Cursor into one window, each with a summary written beside it. It reads
 what those tools already keep and never writes to it.
CONTROL
    dpkg-deb --root-owner-group --build "$deb" "$out/scrollback_${version}_${deb_arch}.deb" >/dev/null
    rm -rf "$deb"
    echo "Built $out/scrollback_${version}_${deb_arch}.deb"
fi
rm -rf "$tree"
