#!/usr/bin/env bash
# Build Setwave.app and (re)launch it. The dev loop for GUI work.
#
#   scripts/run-mac-app.sh [debug|release]
#
# Kills a running Setwave first so the new binary is the one on screen, and
# leaves the app's own mpv alone: the core adopts it on the well-known socket.
set -euo pipefail
PROFILE="${1:-debug}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
"$ROOT/scripts/build-mac-app.sh" "$PROFILE"
pkill -x Setwave 2>/dev/null || true
open "$ROOT/target/Setwave.app"
echo "==> launched; logs: log stream --predicate 'process == \"Setwave\"' --level debug"
