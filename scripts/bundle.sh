#!/bin/bash
# Builds Scrollback.app: the release binary, its icon and an Info.plist,
# wrapped as a Mac application and signed for running on this machine.
#
#   scripts/bundle.sh             build target/release/bundle/Scrollback.app
#   scripts/bundle.sh --install   also copy it into /Applications
#
# SCROLLBACK_BUNDLE_ID sets the bundle identifier; the default is a
# placeholder that is fine locally and should be changed before sharing.
set -euo pipefail
cd "$(dirname "$0")/.."

bundle_id="${SCROLLBACK_BUNDLE_ID:-local.scrollback}"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
app="target/release/bundle/Scrollback.app"
icon_source="crates/app/icon/AppIcon.svg"
icon="crates/app/icon/AppIcon.icns"

cargo build --release -p scrollback-app

# The icon is kept in the repository and only redrawn when its drawing changes.
if [ ! -f "$icon" ] || [ "$icon_source" -nt "$icon" ]; then
    iconset="$(mktemp -d)/AppIcon.iconset"
    swift scripts/render-icon.swift "$icon_source" "$iconset"
    iconutil --convert icns --output "$icon" "$iconset"
    rm -rf "$(dirname "$iconset")"
fi

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/scrollback-app "$app/Contents/MacOS/scrollback"
cp "$icon" "$app/Contents/Resources/AppIcon.icns"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>Scrollback</string>
    <key>CFBundleDisplayName</key>
    <string>Scrollback</string>
    <key>CFBundleIdentifier</key>
    <string>${bundle_id}</string>
    <key>CFBundleExecutable</key>
    <string>scrollback</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleShortVersionString</key>
    <string>${version}</string>
    <key>CFBundleVersion</key>
    <string>${version}</string>
    <key>LSMinimumSystemVersion</key>
    <string>12.0</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.developer-tools</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key>
    <true/>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
</dict>
</plist>
PLIST

# An ad-hoc signature: enough for this Mac, not for handing to someone else.
codesign --force --sign - "$app"

echo "Built $app (version $version)"

if [ "${1:-}" = "--install" ]; then
    # The app was once named in lower case; on a disk that tells the two
    # apart, that copy would otherwise be left beside the new one.
    rm -rf "/Applications/scrollback.app" "/Applications/Scrollback.app"
    cp -R "$app" "/Applications/Scrollback.app"
    echo "Installed /Applications/Scrollback.app"
fi
