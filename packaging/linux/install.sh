#!/bin/sh
# Copies Scrollback into place from an unpacked tarball.
#
#   ./install.sh              into ~/.local, for this user
#   PREFIX=/usr/local sudo ./install.sh     for everyone
#   ./install.sh --remove     take it out again
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
prefix="${PREFIX:-$HOME/.local}"

files="bin/scrollback-app bin/scrollback share/applications/scrollback.desktop"
for size in 16 32 48 64 128 256 512; do
    files="$files share/icons/hicolor/${size}x${size}/apps/scrollback.png"
done

if [ "${1:-}" = "--remove" ]; then
    for file in $files; do
        rm -f "$prefix/$file"
    done
    echo "Removed Scrollback from $prefix. Its index, in ~/.local/share/scrollback, is left alone."
else
    for file in $files; do
        mkdir -p "$(dirname "$prefix/$file")"
        # Copied beside it and renamed over it, so a copy that is running
        # (as when Scrollback updates itself) is replaced, not overwritten.
        cp "$here/$file" "$prefix/$file.new"
        mv -f "$prefix/$file.new" "$prefix/$file"
    done
    # The launcher names the program by its full path, so it works whether
    # or not this prefix is on the PATH.
    sed -i "s|^Exec=.*|Exec=$prefix/bin/scrollback-app|" "$prefix/share/applications/scrollback.desktop"
    echo "Installed Scrollback into $prefix."
    case ":$PATH:" in
        *":$prefix/bin:"*) ;;
        *) echo "$prefix/bin is not on your PATH; add it to use the scrollback command in a terminal." ;;
    esac
fi

# Let the desktop notice the launcher and icon, where it has the tools to.
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$prefix/share/applications" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$prefix/share/icons/hicolor" 2>/dev/null || true
