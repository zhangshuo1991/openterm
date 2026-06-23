#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "macOS packaging requires Darwin" >&2
  exit 2
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"
APP_DIR="$ROOT_DIR/dist/OpenTerm.app"
CONTENTS_DIR="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RESOURCES_DIR="$CONTENTS_DIR/Resources"
DMG_PATH="$ROOT_DIR/dist/OpenTerm-0.1.0.dmg"

cd "$ROOT_DIR"

cargo build --release -p openterm-app

mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"
cat > "$CONTENTS_DIR/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleDisplayName</key>
  <string>OpenTerm</string>
  <key>CFBundleExecutable</key>
  <string>OpenTerm</string>
  <key>CFBundleIdentifier</key>
  <string>dev.openterm.app</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>OpenTerm</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>0.1.0</string>
  <key>CFBundleVersion</key>
  <string>1</string>
  <key>LSMinimumSystemVersion</key>
  <string>12.0</string>
  <key>CFBundleIconFile</key>
  <string>AppIcon</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSHumanReadableCopyright</key>
  <string>OpenTerm</string>
</dict>
</plist>
PLIST

cp "$ROOT_DIR/target/release/openterm-app" "$MACOS_DIR/OpenTerm"
chmod +x "$MACOS_DIR/OpenTerm"
cp "$ROOT_DIR/assets/AppIcon.icns" "$RESOURCES_DIR/AppIcon.icns"

# Refresh Launch Services so Dock shows the icon immediately.
touch "$APP_DIR"
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
  -f "$APP_DIR" 2>/dev/null || true

# Zip the .app for direct distribution (alongside the DMG).
ZIP_PATH="$ROOT_DIR/dist/OpenTerm-0.1.0.zip"
cd "$ROOT_DIR/dist"
zip -qr "$ZIP_PATH" OpenTerm.app
cd "$ROOT_DIR"
echo "created: $ZIP_PATH"

hdiutil create -volname OpenTerm -srcfolder "$APP_DIR" -ov -format UDZO "$DMG_PATH"
shasum -a 256 "$DMG_PATH"
