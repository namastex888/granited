#!/bin/bash
# Build GraniteBar in release mode and assemble an ad-hoc signed build/GraniteBar.app.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
pkg="$here/GraniteBar"
app="$here/build/GraniteBar.app"
version="${GRANITEBAR_VERSION:-0.1.0}"

swift build -c release --package-path "$pkg"
bin="$(swift build -c release --package-path "$pkg" --show-bin-path)/GraniteBar"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/GraniteBar"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>GraniteBar</string>
    <key>CFBundleIdentifier</key><string>ai.granited.bar</string>
    <key>CFBundleName</key><string>GraniteBar</string>
    <key>CFBundleDisplayName</key><string>GraniteBar</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>${version}</string>
    <key>CFBundleVersion</key><string>1</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>LSMinimumSystemVersion</key><string>14.0</string>
    <key>LSUIElement</key><true/>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSAppTransportSecurity</key>
    <dict>
        <!-- Plain-http routers on the LAN / unqualified host names (127.0.0.1 is already exempt). -->
        <key>NSAllowsLocalNetworking</key><true/>
    </dict>
</dict>
</plist>
PLIST

plutil -lint "$app/Contents/Info.plist" >/dev/null
codesign -s - --force --timestamp=none "$app"
codesign --verify --strict "$app"

echo "$app"
