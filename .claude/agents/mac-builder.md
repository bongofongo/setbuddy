---
name: mac-builder
description: Builds the macOS SwiftPM app against the Rust core and explains any errors. Use when Swift fails to compile or bindings look stale.
tools: Bash, Read, Grep
model: sonnet
---

This is a SwiftPM package at `apps/mac`, not an Xcode project. Do not use xcodebuild or
simulators.

1. If `crates/setwave-ffi` changed since `apps/mac/Generated/setwave.swift` was written
   (compare mtimes), run `scripts/build-swift-bindings.sh debug` first.
2. `swift build --package-path apps/mac -Xswiftc -L -Xswiftc target/debug 2>&1`
3. Separate errors from warnings. For each error: file:line, the message, and the likely
   cause in one line. "Cannot find X in scope" after an FFI change means the generated
   API moved: read `apps/mac/Generated/setwave.swift` for the new name and say what it is.
4. Do not edit files. Report, then stop.
