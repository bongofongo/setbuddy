#!/usr/bin/env bash
# Build Setbuddy.app.
#
#   scripts/build-mac-app.sh [debug|release|universal]
#
# SwiftPM cannot emit an .app, so this builds the executable and assembles the
# bundle around it: Info.plist (with LSUIElement so there is no Dock icon), the
# Rust dylib in Frameworks/, and a signature so macOS will run it.
#
# `universal` is the shipping profile — both architectures, and the `setbuddy`
# CLI alongside the app so a cask can expose it. Signing is ad-hoc unless
# SETBUDDY_SIGN_IDENTITY names a Developer ID, in which case the bundle is
# signed inside-out with the hardened runtime, which is what notarisation needs.
set -euo pipefail

PROFILE="${1:-debug}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP="$ROOT/target/Setbuddy.app"
# One deployment target, stated once: the Info.plist and the Swift triples must
# agree or the app links against a newer libswift than it claims to support.
MACOS_MIN="14.0"
UNIVERSAL_TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
UNIVERSAL_TRIPLES=(arm64-apple-macosx$MACOS_MIN x86_64-apple-macosx$MACOS_MIN)

case "$PROFILE" in
  debug)     SWIFT_FLAGS=(); LIBDIR="$ROOT/target/debug" ;;
  release)   SWIFT_FLAGS=(-c release); LIBDIR="$ROOT/target/release" ;;
  universal) SWIFT_FLAGS=(-c release); LIBDIR="$ROOT/target/universal/release" ;;
  *) echo "usage: $0 [debug|release|universal]" >&2; exit 1 ;;
esac

# The version the app reports is the workspace version. Two places to bump was
# one too many; releases are tagged from this number.
VERSION="$(awk -F'"' '/^version = /{print $2; exit}' "$ROOT/Cargo.toml")"

"$ROOT/scripts/build-swift-bindings.sh" "$PROFILE" >/dev/null
echo "==> building Swift app ($PROFILE)"

swift_build() {  # swift_build <extra flags...>; echoes nothing, builds in place
    swift build --package-path "$ROOT/apps/mac" "${SWIFT_FLAGS[@]}" "$@" \
        -Xswiftc -L -Xswiftc "$LIBDIR" \
        -Xlinker -rpath -Xlinker @executable_path/../Frameworks
}
swift_bin_path() {
    swift build --package-path "$ROOT/apps/mac" "${SWIFT_FLAGS[@]}" "$@" --show-bin-path
}

BUILT_BIN="$ROOT/target/Setbuddy-$PROFILE-bin"
if [ "$PROFILE" = universal ]; then
    slices=()
    for triple in "${UNIVERSAL_TRIPLES[@]}"; do
        echo "  -> $triple"
        swift_build --triple "$triple"
        slices+=("$(swift_bin_path --triple "$triple")/Setbuddy")
    done
    lipo -create -output "$BUILT_BIN" "${slices[@]}"
else
    swift_build
    cp "$(swift_bin_path)/Setbuddy" "$BUILT_BIN"
fi

echo "==> assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Frameworks" "$APP/Contents/Resources"
cp "$BUILT_BIN" "$APP/Contents/MacOS/Setbuddy"
cp "$LIBDIR/libsetbuddy_ffi.dylib" "$APP/Contents/Frameworks/"

# The CLI rides along in the bundle so a cask can link it onto PATH. Only for a
# shipping build: adding a cargo build of it to every GUI iteration would cost
# the dev loop more than it is worth.
if [ "$PROFILE" = universal ]; then
    cargo build -p setbuddy-cli --release "${UNIVERSAL_TARGETS[@]/#/--target=}"
    slices=()
    for target in "${UNIVERSAL_TARGETS[@]}"; do
        slices+=("$ROOT/target/$target/release/setbuddy")
    done
    lipo -create -output "$APP/Contents/MacOS/setbuddy" "${slices[@]}"
elif [ "$PROFILE" = release ]; then
    cargo build -p setbuddy-cli --release
    cp "$ROOT/target/release/setbuddy" "$APP/Contents/MacOS/setbuddy"
fi

# The dylib records an absolute build path as its install name; rewrite both
# sides to @rpath so the copy inside the bundle is the one that gets loaded.
install_name_tool -id "@rpath/libsetbuddy_ffi.dylib" \
    "$APP/Contents/Frameworks/libsetbuddy_ffi.dylib"
OLD_REF="$(otool -L "$APP/Contents/MacOS/Setbuddy" | awk '/libsetbuddy_ffi\.dylib/ {print $1; exit}')"
if [ -n "$OLD_REF" ] && [ "$OLD_REF" != "@rpath/libsetbuddy_ffi.dylib" ]; then
    install_name_tool -change "$OLD_REF" "@rpath/libsetbuddy_ffi.dylib" \
        "$APP/Contents/MacOS/Setbuddy"
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>            <string>Setbuddy</string>
    <key>CFBundleDisplayName</key>     <string>Setbuddy</string>
    <key>CFBundleIdentifier</key>      <string>com.toadmountain.setbuddy</string>
    <key>CFBundleExecutable</key>      <string>Setbuddy</string>
    <key>CFBundlePackageType</key>     <string>APPL</string>
    <key>CFBundleShortVersionString</key> <string>$VERSION</string>
    <key>CFBundleVersion</key>         <string>$VERSION</string>
    <key>LSMinimumSystemVersion</key>  <string>$MACOS_MIN</string>
    <!-- Menu bar only: no Dock icon, no app switcher entry. -->
    <key>LSUIElement</key>             <true/>
    <key>NSHighResolutionCapable</key> <true/>
</dict>
</plist>
PLIST

# Signing is inside-out — nested code first, the bundle last — because signing
# the bundle seals what is inside it. `--deep` would do this in one call but it
# is deprecated and cannot apply per-item options.
sign() {  # sign <path>
    if [ -n "${SETBUDDY_SIGN_IDENTITY:-}" ]; then
        # --timestamp and the hardened runtime are both required by notarisation
        # and cannot be added afterwards.
        codesign --force --sign "$SETBUDDY_SIGN_IDENTITY" \
            --options runtime --timestamp "$1"
    else
        # Unsigned bundles that load a dylib are killed on launch. Ad-hoc is
        # enough to run locally and not enough to distribute.
        codesign --force --sign - "$1" >/dev/null 2>&1 ||
            echo "warning: ad-hoc codesign failed; the app may not launch" >&2
    fi
}
sign "$APP/Contents/Frameworks/libsetbuddy_ffi.dylib"
if [ -f "$APP/Contents/MacOS/setbuddy" ]; then sign "$APP/Contents/MacOS/setbuddy"; fi
sign "$APP"

echo "==> built $APP ($VERSION, $(lipo -archs "$APP/Contents/MacOS/Setbuddy"))"
