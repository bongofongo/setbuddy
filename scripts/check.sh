#!/usr/bin/env bash
# The one check entry point. Tiers, cheapest first; each includes the one before.
#
#   scripts/check.sh fast    cargo check (all targets) + tests that need no mpv
#   scripts/check.sh rust    + mpv and CLI integration tests against a real mpv
#   scripts/check.sh swift   + bindings, swift build, swift test, foreign-engine check
#   scripts/check.sh all     everything (== swift)
#
# Every tier runs with an isolated SETWAVE_STATE_DIR so nothing touches the real
# library or the mpv the user may have playing.
set -euo pipefail

TIER="${1:-fast}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export SETWAVE_STATE_DIR="${SETWAVE_STATE_DIR:-$ROOT/target/check-state}"
rm -rf "$SETWAVE_STATE_DIR"; mkdir -p "$SETWAVE_STATE_DIR"
cd "$ROOT"

step() { printf '\n==> %s\n' "$*"; }

fast() {
    step "cargo check --workspace --all-targets"
    cargo check --workspace --all-targets
    step "cargo test (no mpv)"
    cargo test --workspace
}

rust() {
    fast
    if ! command -v mpv >/dev/null; then
        echo "mpv not on PATH; skipping integration tests" >&2
        return
    fi
    step "mpv integration tests"
    cargo test -p setwave-mpv --features integration -- --test-threads=1
    step "cli end-to-end tests"
    cargo test -p setwave-cli --features integration -- --test-threads=1
}

swift_tier() {
    rust
    step "swift bindings + build"
    "$ROOT/scripts/build-swift-bindings.sh" debug >/dev/null
    swift build --package-path "$ROOT/apps/mac" -Xswiftc -L -Xswiftc "$ROOT/target/debug"
    step "swift model tests"
    swift test --package-path "$ROOT/apps/mac" -Xswiftc -L -Xswiftc "$ROOT/target/debug"
    step "foreign engine check"
    "$ROOT/scripts/check-swift-bindings.sh"
}

case "$TIER" in
    fast)  fast ;;
    rust)  rust ;;
    swift|all) swift_tier ;;
    *) echo "usage: $0 [fast|rust|swift|all]" >&2; exit 1 ;;
esac
printf '\n==> %s tier passed\n' "$TIER"
