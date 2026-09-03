---
name: add-engine
description: Playbook for adding a playback engine (AVFoundation or any other) behind the PlaybackEngine contract without touching core, CLI, or views.
---

An engine is one module implementing `PlaybackEngine` plus one registration line. If a
step below needs more than that, stop and fix the boundary instead.

## Swift engine (AVFoundation — the MVP one)

1. Read `setwave-engine/src/lib.rs` (the contract) and `tests/swift/main.swift`
   (`SwiftNullEngine`, the proven shape). Read `apps/mac/Generated/setwave.swift` for the
   Swift-side `PlaybackEngine` protocol and `EngineCapabilities`/`EngineSnapshot` records.
2. Create `apps/mac/Sources/SetwaveAV/AVFoundationEngine.swift` as a new SwiftPM target
   depending on `SetwaveCore`. `final class AVFoundationEngine: PlaybackEngine,
   @unchecked Sendable`. Own: one `AVPlayer`, one lazily created `NSWindow` hosting an
   `AVPlayerView` (gives native PiP), a lock around state, and a cached snapshot updated
   from `addPeriodicTimeObserver` (never computed on demand: `snapshot()` must not block).
3. Capabilities: `id: "avfoundation"`, containers mp3 m4a mp4 mov aac wav aiff (what
   AVFoundation decodes natively; leave webm/mkv/opus to mpv), `video: true`,
   `ontopWindow: true` (`window.level = .floating`), `nativePip: true`.
4. Map each method. `load` = replace `currentItem`, seek to `startAt`, `play()`.
   `setVideoVisible` = order the window in/out, audio unaffected. `setVideoWindowLayout`
   = parse the same forms `VideoWindowLayout::parse` accepts (already a string across the
   FFI). `stop` = `replaceCurrentItem(with: nil)`. `shutdown` = idempotent teardown.
   All AVFoundation calls hop to main via `DispatchQueue.main.async`; the contract call
   returns immediately.
5. Register: in `PlayerModel` construct via `Setwave.withEngines([AVFoundationEngine()])`
   instead of `Setwave()`. That is the one line. Engine policy in settings already offers
   force-by-id.
6. Prove it: extend `tests/swift/main.swift` or `PlayerModelTests` so the registry picks
   avfoundation for `tiny.mp3` and mpv for `tiny.webm`, and that `next` across engines
   stops the old one before the new one loads. Then `scripts/check.sh swift`.

## Rust engine

Same contract, in a new `crates/setwave-<name>` depending only on `setwave-engine`.
Register in `setwave-cli/src/main.rs` and `setwave-ffi` `Setwave::build`. Test with the
same asset set under `crates/setwave-mpv/tests/assets`, behind an `integration` feature
if it needs real hardware.
