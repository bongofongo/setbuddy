#!/usr/bin/env bash
# Compile and run the Swift-side check of the FFI bindings.
#
# This is the load test for the v2 escape hatch: it builds a playback engine
# written in Swift against the generated bindings and drives it through the Rust
# core. Run it whenever the FFI surface changes.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GEN="$ROOT/apps/mac/Generated"
BUILD="$ROOT/target/swift-check"

"$ROOT/scripts/build-swift-bindings.sh" debug >/dev/null

mkdir -p "$BUILD"
echo "==> compiling Swift check"
swiftc \
    -swift-version 5 \
    -Xcc -fmodule-map-file="$GEN/setwaveFFI.modulemap" \
    -I "$GEN" \
    -L "$ROOT/target/debug" -lsetwave_ffi \
    "$GEN/setwave.swift" \
    "$ROOT/tests/swift/main.swift" \
    -o "$BUILD/foreign-engine-check"

echo "==> running"
# Isolated state so the check never touches a real library or a running mpv.
STATE="$BUILD/state"
rm -rf "$STATE"
SETWAVE_STATE_DIR="$STATE" \
DYLD_LIBRARY_PATH="$ROOT/target/debug" \
    "$BUILD/foreign-engine-check" "$ROOT/crates/setwave-mpv/tests/assets"
