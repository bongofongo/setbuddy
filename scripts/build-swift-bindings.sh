#!/usr/bin/env bash
# Build the Rust core and generate Swift bindings for the macOS app.
#
#   scripts/build-swift-bindings.sh [debug|release|universal]
#
# Produces, in apps/mac/Generated/:
#   setbuddy.swift         the Swift API
#   setbuddyFFI.h          the C header
#   setbuddyFFI.modulemap  the module map
# and copies the dylib next to them.
#
# `universal` is the shipping profile: both macOS architectures, lipo'd into one
# dylib under target/universal/release. A cask serves Intel machines too, and a
# thin arm64 build would simply not launch on one.
set -euo pipefail

PROFILE="${1:-debug}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/apps/mac/Generated"
UNIVERSAL_TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)

case "$PROFILE" in
  debug)   LIB="$ROOT/target/debug/libsetbuddy_ffi.dylib" ;;
  release) LIB="$ROOT/target/release/libsetbuddy_ffi.dylib" ;;
  universal) LIB="$ROOT/target/universal/release/libsetbuddy_ffi.dylib" ;;
  *) echo "usage: $0 [debug|release|universal]" >&2; exit 1 ;;
esac

echo "==> building setbuddy-ffi ($PROFILE)"
case "$PROFILE" in
  debug)   cargo build -p setbuddy-ffi ;;
  release) cargo build -p setbuddy-ffi --release ;;
  universal)
    for target in "${UNIVERSAL_TARGETS[@]}"; do
      rustup target list --installed | grep -qx "$target" ||
        { echo "missing Rust target $target; run: rustup target add $target" >&2; exit 1; }
    done
    cargo build -p setbuddy-ffi --release "${UNIVERSAL_TARGETS[@]/#/--target=}"
    slices=()
    for target in "${UNIVERSAL_TARGETS[@]}"; do
      slices+=("$ROOT/target/$target/release/libsetbuddy_ffi.dylib")
    done
    mkdir -p "$(dirname "$LIB")"
    lipo -create -output "$LIB" "${slices[@]}"
    ;;
esac

echo "==> generating Swift bindings"
mkdir -p "$OUT"
# The generator dlopen's the library, so it needs a slice for the host. A fat
# dylib carries one; --target is deliberately not passed to this run.
cargo run -q -p setbuddy-ffi --bin uniffi-bindgen \
    $([ "$PROFILE" = debug ] || echo --release) -- \
    generate --library "$LIB" --language swift --out-dir "$OUT"

cp "$LIB" "$OUT/"

echo "==> wrote:"
ls -1 "$OUT"
