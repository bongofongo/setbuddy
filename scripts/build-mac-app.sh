#!/usr/bin/env bash
# Build Setbuddy.app.
#
#   scripts/build-mac-app.sh [debug|release]
#
# SwiftPM cannot emit an .app, so this builds the executable and assembles the
# bundle around it: Info.plist (with LSUIElement so there is no Dock icon), the
# Rust dylib in Frameworks/, and an ad-hoc signature so macOS will run it.
set -euo pipefail

PROFILE="${1:-debug}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP="$ROOT/target/Setbuddy.app"

case "$PROFILE" in
  debug)   SWIFT_FLAGS=() ;;
  release) SWIFT_FLAGS=(-c release) ;;
  *) echo "usage: $0 [debug|release]" >&2; exit 1 ;;
esac

"$ROOT/scripts/build-swift-bindings.sh" "$PROFILE" >/dev/null
echo "==> building Swift app ($PROFILE)"
swift build --package-path "$ROOT/apps/mac" "${SWIFT_FLAGS[@]}" \
    -Xswiftc -L -Xswiftc "$ROOT/target/$PROFILE" \
    -Xlinker -rpath -Xlinker @executable_path/../Frameworks

BIN="$(swift build --package-path "$ROOT/apps/mac" "${SWIFT_FLAGS[@]}" --show-bin-path)/Setbuddy"
DYLIB="$ROOT/target/$PROFILE/libsetbuddy_ffi.dylib"

echo "==> assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Frameworks" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/Setbuddy"
cp "$DYLIB" "$APP/Contents/Frameworks/"

# The dylib records an absolute build path as its install name; rewrite both
# sides to @rpath so the copy inside the bundle is the one that gets loaded.
install_name_tool -id "@rpath/libsetbuddy_ffi.dylib" \
    "$APP/Contents/Frameworks/libsetbuddy_ffi.dylib"
OLD_REF="$(otool -L "$APP/Contents/MacOS/Setbuddy" | awk '/libsetbuddy_ffi\.dylib/ {print $1; exit}')"
if [ -n "$OLD_REF" ] && [ "$OLD_REF" != "@rpath/libsetbuddy_ffi.dylib" ]; then
    install_name_tool -change "$OLD_REF" "@rpath/libsetbuddy_ffi.dylib" \
        "$APP/Contents/MacOS/Setbuddy"
fi

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>            <string>Setbuddy</string>
    <key>CFBundleDisplayName</key>     <string>Setbuddy</string>
    <key>CFBundleIdentifier</key>      <string>com.toadmountain.setbuddy</string>
    <key>CFBundleExecutable</key>      <string>Setbuddy</string>
    <key>CFBundlePackageType</key>     <string>APPL</string>
    <key>CFBundleShortVersionString</key> <string>0.1.0</string>
    <key>CFBundleVersion</key>         <string>1</string>
    <key>LSMinimumSystemVersion</key>  <string>14.0</string>
    <!-- Menu bar only: no Dock icon, no app switcher entry. -->
    <key>LSUIElement</key>             <true/>
    <key>NSHighResolutionCapable</key> <true/>
</dict>
</plist>
PLIST

# Ad-hoc signature: unsigned bundles that load a dylib are killed on launch.
codesign --force --deep --sign - "$APP" >/dev/null 2>&1 || \
    echo "warning: ad-hoc codesign failed; the app may not launch" >&2

echo "==> built $APP"
