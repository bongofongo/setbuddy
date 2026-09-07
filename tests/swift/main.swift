// Proves the v2 escape hatch works while it is still cheap to change.
//
// A playback engine written entirely in Swift is handed to the Rust core, which
// selects it, drives it through the same contract mpv satisfies, and reports its
// state back through `snapshot()`. If this compiles and passes, adding an
// AVFoundation engine in v2 is a matter of writing one class — no change to the
// core, the CLI, or the FFI surface.
//
// Built and run by scripts/check-swift-bindings.sh. Named main.swift because
// Swift only permits top-level code in a file with that name.

import Foundation

var failures = 0

func check(_ condition: Bool, _ message: String) {
    if condition {
        print("  ok   \(message)")
    } else {
        print("  FAIL \(message)")
        failures += 1
    }
}

/// A stub engine in Swift. Records what it was asked to do and simulates
/// playback well enough for the core to treat it as real.
final class SwiftNullEngine: PlaybackEngine, @unchecked Sendable {
    private let lock = NSLock()
    private let id: String
    private let containers: [String]
    private var calls: [String] = []
    private var path: String?
    private var position: Double = 0
    private var paused = false
    private var videoVisible = false
    private var didShutdown = false

    init(id: String, containers: [String]) {
        self.id = id
        self.containers = containers
    }

    private func record(_ call: String) {
        lock.lock(); defer { lock.unlock() }
        calls.append(call)
    }

    func recordedCalls() -> [String] {
        lock.lock(); defer { lock.unlock() }
        return calls
    }

    func wasShutDown() -> Bool {
        lock.lock(); defer { lock.unlock() }
        return didShutdown
    }

    // MARK: PlaybackEngine

    func capabilities() -> EngineCapabilities {
        EngineCapabilities(
            id: id,
            displayName: "Swift \(id)",
            containers: containers,
            video: true,
            ontopWindow: true,
            // The distinguishing capability of an AVFoundation engine.
            nativePip: true
        )
    }

    func load(path: String, startAt: Double?) throws {
        record("load(\(path), \(startAt.map { String($0) } ?? "nil"))")
        lock.lock(); defer { lock.unlock() }
        self.path = path
        self.position = startAt ?? 0
        self.paused = false
    }

    func setPaused(paused: Bool) throws {
        record("setPaused(\(paused))")
        lock.lock(); defer { lock.unlock() }
        self.paused = paused
    }

    func seekAbsolute(seconds: Double) throws {
        record("seekAbsolute(\(seconds))")
        lock.lock(); defer { lock.unlock() }
        self.position = seconds
    }

    func setVolume(percent: Double) throws { record("setVolume(\(percent))") }
    func setSpeed(rate: Double) throws { record("setSpeed(\(rate))") }

    func setVideoVisible(visible: Bool) throws {
        record("setVideoVisible(\(visible))")
        lock.lock(); defer { lock.unlock() }
        self.videoVisible = visible
    }

    func setVideoOntop(ontop: Bool) throws { record("setVideoOntop(\(ontop))") }

    func setVideoWindowLayout(spec: String) throws { record("setVideoWindowLayout(\(spec))") }

    func stop() throws {
        record("stop()")
        lock.lock(); defer { lock.unlock() }
        self.path = nil
        self.position = 0
    }

    func snapshot() -> EngineSnapshot {
        lock.lock(); defer { lock.unlock() }
        return EngineSnapshot(
            positionSecs: path == nil ? nil : position,
            durationSecs: path == nil ? nil : 3600,
            paused: paused,
            idle: path == nil,
            eof: false,
            hasVideo: path?.hasSuffix(".webm") ?? false,
            videoVisible: videoVisible,
            path: path
        )
    }

    func shutdown() {
        record("shutdown()")
        lock.lock(); defer { lock.unlock() }
        didShutdown = true
    }
}

/// Receives snapshots pushed from the Rust ticker thread.
final class SnapshotRecorder: PlayerObserver, @unchecked Sendable {
    private let lock = NSLock()
    private var received: [PlayerSnapshot] = []

    func onSnapshot(snapshot: PlayerSnapshot) {
        lock.lock(); defer { lock.unlock() }
        received.append(snapshot)
    }

    func count() -> Int {
        lock.lock(); defer { lock.unlock() }
        return received.count
    }

    func latest() -> PlayerSnapshot? {
        lock.lock(); defer { lock.unlock() }
        return received.last
    }
}

// ---------------------------------------------------------------------------

let assetDir = CommandLine.arguments.count > 1
    ? CommandLine.arguments[1]
    : "crates/setbuddy-mpv/tests/assets"
let mp3 = "\(assetDir)/tiny.mp3"

print("1. a Swift engine is accepted and preferred over mpv")
let swiftEngine = SwiftNullEngine(id: "swiftnull", containers: ["mp3", "wav", "m4a"])
let setbuddy: Setbuddy
do {
    setbuddy = try Setbuddy.withEngines(engines: [swiftEngine])
} catch {
    print("  FAIL could not construct Setbuddy: \(error)")
    exit(1)
}

let ids = setbuddy.engineIds()
check(ids == ["swiftnull", "mpv"], "registration order is preference order: \(ids)")

print("2. the core routes a file to the Swift engine")
do {
    let track = try setbuddy.playPath(path: mp3)
    check(track.displayLabel == "tiny", "indexed and named the track: \(track.displayLabel)")
} catch {
    print("  FAIL playPath threw: \(error)")
    failures += 1
}

let loads = swiftEngine.recordedCalls().filter { $0.hasPrefix("load(") }
check(loads.count == 1, "Swift engine received exactly one load: \(loads)")
check(loads.first?.hasSuffix("tiny.mp3, nil)") ?? false,
      "loaded the requested file from the start")

print("3. state flows back out through the same contract")
do {
    let snap = try setbuddy.snapshot()
    check(snap.engineId == "swiftnull", "snapshot reports the Swift engine: \(snap.engineId ?? "nil")")
    check(snap.durationSecs == 3600, "duration came from the Swift engine")
    check(!snap.idle, "core sees the Swift engine as playing")
} catch {
    print("  FAIL snapshot threw: \(error)")
    failures += 1
}

print("4. commands reach the Swift engine")
do {
    _ = try setbuddy.togglePaused()
    try setbuddy.seekAbsolute(seconds: 1200)
    let calls = swiftEngine.recordedCalls()
    check(calls.contains { $0.hasPrefix("setPaused(") }, "pause reached the engine")
    check(calls.contains("seekAbsolute(1200.0)"), "seek reached the engine")
} catch {
    print("  FAIL command threw: \(error)")
    failures += 1
}

print("5. the window layout reaches the engine as the settings string")
do {
    try setbuddy.setVideoWindowLayout(spec: "1280+100+50/0")
    check(swiftEngine.recordedCalls().contains("setVideoWindowLayout(1280+100+50/0)"),
          "the engine was given the layout verbatim")
} catch {
    print("  FAIL setVideoWindowLayout threw: \(error)")
    failures += 1
}

print("6. errors cross the boundary as typed cases")
do {
    _ = try setbuddy.playPath(path: "\(assetDir)/definitely-not-here.mp3")
    check(false, "playing a missing file should throw")
} catch let error as SetbuddyError {
    // The path does not exist, so indexing rejects it before any engine is asked.
    if case .Playback = error {
        check(true, "missing file surfaced as a typed SetbuddyError")
    } else if case .UnsupportedFile = error {
        check(true, "missing file surfaced as a typed SetbuddyError")
    } else {
        check(false, "unexpected SetbuddyError case: \(error)")
    }
} catch {
    check(false, "expected SetbuddyError, got \(error)")
}

print("7. handing a file to another engine stops the first one")
do {
    // A webm belongs to mpv, so this crosses engines. The old engine must be
    // stopped before the new one loads, or both hold the audio device.
    _ = try setbuddy.playPath(path: "\(assetDir)/tiny.webm")
    let calls = swiftEngine.recordedCalls()
    check(calls.contains("stop()"), "the Swift engine was stopped on the way out")
    check(calls.last == "stop()", "and nothing was asked of it afterwards: \(calls.suffix(3))")
    let snap = try setbuddy.snapshot()
    check(snap.engineId == "mpv", "the webm went to mpv: \(snap.engineId ?? "nil")")
    // Back to the Swift engine, so the ticker checks below see a known state.
    _ = try setbuddy.playPath(path: mp3)
} catch {
    print("  FAIL engine handoff threw: \(error)")
    failures += 1
}

print("8. observers are called from the Rust ticker thread")
let recorder = SnapshotRecorder()
setbuddy.addObserver(observer: recorder)
setbuddy.startTicker(intervalMs: 50)
Thread.sleep(forTimeInterval: 0.5)
setbuddy.stopTicker()
check(recorder.count() > 1, "received \(recorder.count()) snapshots from Rust")
check(recorder.latest()?.engineId == "swiftnull", "pushed snapshots describe the Swift engine")

print("9. shutdown reaches the Swift engine")
do {
    try setbuddy.quit()
} catch {
    print("  FAIL quit threw: \(error)")
    failures += 1
}
check(swiftEngine.wasShutDown(), "Swift engine was shut down by the core")

print("")
if failures == 0 {
    print("foreign-trait check passed — a Swift engine satisfies the Rust contract")
    exit(0)
} else {
    print("foreign-trait check FAILED with \(failures) failure(s)")
    exit(1)
}
