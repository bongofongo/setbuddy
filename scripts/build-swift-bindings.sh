#!/usr/bin/env bash
# Build the Rust core and generate Swift bindings for the macOS app.
#
#   scripts/build-swift-bindings.sh [debug|release]
#
# Produces, in apps/mac/Generated/:
#   setwave.swift         the Swift API
#   setwaveFFI.h          the C header
#   setwaveFFI.modulemap  the module map
# and copies the dylib next to them.
set -euo pipefail

PROFILE="${1:-debug}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/apps/mac/Generated"

case "$PROFILE" in
  debug)   CARGO_FLAGS=() ;;
  release) CARGO_FLAGS=(--release) ;;
  *) echo "usage: $0 [debug|release]" >&2; exit 1 ;;
esac

LIB="$ROOT/target/$PROFILE/libsetwave_ffi.dylib"

echo "==> building setwave-ffi ($PROFILE)"
cargo build -p setwave-ffi "${CARGO_FLAGS[@]}"

echo "==> generating Swift bindings"
mkdir -p "$OUT"
cargo run -q -p setwave-ffi --bin uniffi-bindgen "${CARGO_FLAGS[@]}" -- \
    generate --library "$LIB" --language swift --out-dir "$OUT"

cp "$LIB" "$OUT/"

echo "==> wrote:"
ls -1 "$OUT"
