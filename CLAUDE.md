# Setwave

macOS menu-bar player for downloaded DJ sets and music (yt-dlp webm/mkv, mp3, flac…).
Rust core + Swift GUI. Playback goes through a pluggable engine: mpv today, AVFoundation
next. Both must coexist in the MVP, selected per file by capability.

## Priorities, in order

1. **Snappy.** Every user action reflects in the UI within one frame. Nothing on the main
   thread may block on an engine, the disk, or a subprocess.
2. **Fast iteration.** Prefer the cheapest check that proves the change (see Commands).
   Keep builds incremental; never add a dependency without a reason that survives review.
3. **Minimal complexity.** Deep modules, narrow interfaces (Ousterhout). See Design rules.

## Layout

| Path | Role | May depend on |
|---|---|---|
| `crates/setwave-engine` | The playback contract: `PlaybackEngine` trait, records, `NullEngine`. | `thiserror` only |
| `crates/setwave-mpv` | The only code that knows mpv exists. JSON IPC over a socket. | engine |
| `crates/setwave-core` | Library index (SQLite), queue, resume, `EngineRegistry`, `Player` facade. | engine |
| `crates/setwave-cli` | `setwave` binary. Short-lived; adopts the running mpv. | core, engine, mpv |
| `crates/setwave-ffi` | All UniFFI. Exports `Setwave` object + `PlaybackEngine` foreign trait. | core, engine, mpv |
| `apps/mac` | SwiftPM. `SetwaveUI` (model + views, testable) and `Setwave` (the `@main`). | generated bindings |
| `scripts/` | Build, bindings, checks. Each is a single deep entry point. | |
| `docs/` | Local notes, git-ignored. `session-log.md` gets one entry per session. | |

## Engine boundary — hard invariants

- `setwave-engine` and `setwave-core` never name a backend. No `mpv`, `AVFoundation`,
  `AVPlayer` in code or `Cargo.toml`. Selection asks `EngineCapabilities`, never identity.
- Every type crossing `PlaybackEngine` stays UniFFI-representable: `String`, `f64`, `bool`,
  `u32`, `Option`, `Vec`, plain records, enums. No `Path`, no lifetimes, no generics.
- `setwave-ffi` mirrors the trait `with_foreign`; a Swift engine is wrapped by
  `ForeignEngine` and registered ahead of mpv via `Setwave::with_engines`.
  `tests/swift/main.swift` proves the direction. Changing the trait means: engine crate,
  `NullEngine`, `setwave-mpv`, the FFI mirror, the Swift check, in that order.
- `snapshot()` never blocks on the engine; return last-known state.
- mpv is always spawned `--no-config --load-scripts=no` with every option explicit
  (user config once hijacked resume and the IPC socket). Canonical args live in
  `setwave-mpv/src/lib.rs`, not in docs.
- Adding an engine touches: one new module implementing the trait, plus one line of
  registration. If it touches more, the boundary is wrong; fix the boundary, not the caller.

## Commands

Fastest proof first. Run the narrowest tier that covers the change.

```sh
scripts/check.sh fast    # cargo check (all targets) + core/engine tests. No mpv. ~seconds.
scripts/check.sh rust    # + mpv/cli integration tests against a real mpv, isolated state dir.
scripts/check.sh swift   # + regenerate bindings, swift build, swift test, foreign-engine check.
scripts/check.sh all     # everything above.
scripts/run-mac-app.sh   # build Setwave.app (debug) and relaunch it.
cargo run -p setwave-cli -- play <file|query>   # drive the same core from a shell.
```

Changed only Swift under `apps/mac`? `swift build --package-path apps/mac -Xswiftc -L -Xswiftc target/debug`
is enough; bindings only change when `setwave-ffi` does. Changed `setwave-ffi`? Run the
`swift` tier — the generated API moved.

State lives in `~/Library/Application Support/Setwave` unless `SETWAVE_STATE_DIR` is set.
Tests and scripts always set it; never let a test touch the real library.

## Design rules

- **Deep modules.** A module earns its existence by hiding something hard behind a small
  surface: `MpvEngine` hides a process, a socket, timeouts and reconnects behind ten
  methods. Do not add a module that is mostly pass-through.
- **One facade per layer.** GUI and CLI talk to `Player` (Rust) / `Setwave` (FFI). Views
  talk to `PlayerModel`. Views never touch FFI types directly.
- **Pull complexity downward.** If the caller has to know an ordering, a unit, a retry, or
  a special case, move it into the callee. The FFI layer converts units and shapes once;
  Swift gets values it can display without arithmetic.
- **Errors are information, not control flow.** Define errors where they are meaningful
  (`EngineError`, `CoreError`); the FFI flattens them once into `SetwaveError`. Don't
  invent new error types for one call site.
- **Comments state what the code cannot:** why, invariants, measured facts. Every non-
  obvious decision has one line saying what it costs to reverse.
- **No speculative generality.** Build for mpv and AVFoundation, not "any N engines" beyond
  what the registry already gives for free.
- **Settings are strings in the store.** Parse at the edge (`VideoWindowLayout::parse`),
  keep typed values inside.

## Performance rules

- Main thread: read one `PlayerSnapshot`, render. Every FFI call from a view handler must
  return in well under a frame; anything slower (scan, probe, artwork, ffprobe) goes off-
  main and publishes back.
- Engine calls are already bounded by their own timeout; the UI must still never await
  them in a `@MainActor` context on the render path. Fire, then let the ticker's push
  update the view.
- Ticker pushes snapshots on a Rust thread; `SnapshotBridge` hops to main. Keep the
  snapshot cheap to build — no disk reads in `snapshot_of` beyond the resume lookup.
- Scrubbing: optimistic local position (`pendingSeek`) until the engine catches up.
  Preserve that pattern for any control that has visible latency.
- SQLite: one open `Store`, prepared statements, indexed lookups. Batch writes in a
  transaction. Resume writes at most every 15 s.
- Measure before optimising; `docs/mpv-notes.md` shows the format (ms numbers, method).

## Working here

- Before a change, name the tier that proves it and run it after. Report the output.
- Long-running things (app, tests in a loop) go in a tmux pane, not the foreground.
- Rust is rustfmt-clean (`rustfmt.toml`); Swift follows the surrounding file.
- Tests: `NullEngine` for core logic (deterministic, no sleep); real mpv only behind the
  `integration` feature. Every bug fix gets a test at the lowest layer that can see it.
- New engine? Follow `.claude/skills/add-engine`.
- End of session: append a few lines to `docs/session-log.md` (what changed, one decision
  worth keeping, tags).

## Roadmap to MVP

1. AVFoundation engine as a Swift `PlaybackEngine` in `apps/mac/Sources/SetwaveAV`
   (`AVPlayer` + an `NSWindow` for video; `native_pip: true`). Registered ahead of mpv,
   claiming mp3/m4a/mp4/mov/aac/wav/aiff; mpv keeps webm/mkv/opus/flac and anything else.
2. Engine policy UI: auto / force mpv / force AVFoundation, already in the store.
3. Handoff on `next` across engines: stop old, load new, no audio overlap.
