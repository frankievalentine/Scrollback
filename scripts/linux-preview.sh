#!/bin/sh
# Shows, from a Mac, what Scrollback looks like on Linux: builds it in a
# container, runs it on a desktop with no screen (sway, under Wayland) and
# saves a picture of that desktop. Needs Docker; the first run takes several
# minutes.
#
#   scripts/linux-preview.sh                          the session window
#   scripts/linux-preview.sh SCROLLBACK_DEV_FRAME=1   with the app's own title bar
#   scripts/linux-preview.sh SCROLLBACK_DEV_MENU=1    with the first menu open
#   scripts/linux-preview.sh SCROLLBACK_DEV_SETTINGS=general
#
# Why sway: GPUI needs a keyboard-and-pointer seat, which weston has none of
# without a screen, and under Xvfb it never draws a frame. sway always draws
# window frames itself, so the app's own title bar only shows when asked for
# with SCROLLBACK_DEV_FRAME=1; dragging and resizing by it cannot be tried
# here.
#
# The picture is left at target/linux-preview.png. So there is something to
# show, your Claude Code sessions (~/.claude/projects) are handed to the
# container read-only; nothing is summarised and nothing leaves the machine.
set -eu
cd "$(dirname "$0")/.."

docker build -q -t scrollback-linux-preview - >/dev/null <<'DOCKERFILE'
FROM rust:1-bookworm
RUN apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
    pkg-config cmake clang libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
    libx11-xcb-dev libxcb1-dev libfontconfig-dev libfreetype-dev libssl-dev libzstd-dev \
    libvulkan-dev libasound2-dev \
    sway grim dbus dbus-x11 mesa-vulkan-drivers libvulkan1 libegl1 libgles2 libgl1-mesa-dri \
    fonts-dejavu-core fonts-noto-core \
    && rm -rf /var/lib/apt/lists/* && useradd -m viewer
DOCKERFILE

mkdir -p target
sessions="$HOME/.claude/projects"
[ -d "$sessions" ] || sessions=$(mktemp -d)

docker run --rm \
    -v "$PWD":/src:ro \
    -v "$PWD/target":/out \
    -v "$sessions":/home/viewer/.claude/projects:ro \
    -v scrollback-linux-target:/target \
    -v scrollback-linux-cargo:/usr/local/cargo/registry \
    -e CARGO_TARGET_DIR=/target \
    -w /src scrollback-linux-preview sh -eu -c '
        cargo build --locked -p scrollback-app
        mkdir -p /tmp/run && chown viewer /tmp/run && chmod 700 /tmp/run
        # The app, on a desktop of its own, as an ordinary user.
        cat > /tmp/show.sh <<SHOW
export HOME=/home/viewer XDG_RUNTIME_DIR=/tmp/run
export WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman
eval \$(dbus-launch --sh-syntax)
printf "output HEADLESS-1 resolution 1400x900\ndefault_border none\n" > /tmp/sway.conf
sway -c /tmp/sway.conf >/dev/null 2>&1 &
sleep 3
export WAYLAND_DISPLAY=\$(ls /tmp/run | grep "^wayland-[0-9]*\$" | head -1)
env SCROLLBACK_NO_CLAUDE=1 SCROLLBACK_FIRST_RUN=0 "\$@" /target/debug/scrollback-app &
sleep 8
grim /tmp/run/preview.png
SHOW
        runuser -u viewer -- sh /tmp/show.sh "$@"
        cp /tmp/run/preview.png /out/linux-preview.png
    ' preview "$@"
echo "Saved target/linux-preview.png"
