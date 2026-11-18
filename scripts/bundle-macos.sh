#!/bin/sh
# Builds a release Oniko.app in target/release/bundle. Same bundle id as the Electron build, so
# macOS keeps its notification permission.
set -eu
cd "$(dirname "$0")/.."

cargo build --release -p oniko

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
app=target/release/bundle/Oniko.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/oniko "$app/Contents/MacOS/Oniko"

# The dock icon, from the 1024px PNG.
iconset=$(mktemp -d)/icon.iconset
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  sips -z $size $size crates/oniko/assets/icon.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z $double $double crates/oniko/assets/icon.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/icon.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Oniko</string>
  <key>CFBundleDisplayName</key><string>Oniko</string>
  <key>CFBundleIdentifier</key><string>site.orsik.oniko</string>
  <key>CFBundleExecutable</key><string>Oniko</string>
  <key>CFBundleIconFile</key><string>icon.icns</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

# Ad-hoc signing lets it run locally; distribute with a Developer ID instead.
codesign --force --deep --sign - "$app" >/dev/null 2>&1 || true
echo "$app"
