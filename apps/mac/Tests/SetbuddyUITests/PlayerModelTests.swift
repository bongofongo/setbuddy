import SwiftUI
import XCTest
@testable import SetbuddyAV
@testable import SetbuddyUI

/// Drives the app's model headlessly against the real core and a real mpv.
///
/// This covers the M3 behaviours that do not require a mouse: a video file
/// playing audio-only from the menu bar, the pop-out toggling without
/// interrupting playback, and a position surviving a full restart. What remains
/// genuinely manual is the visual check of the floating window and a physical
/// media-key press.
@MainActor
final class PlayerModelTests: XCTestCase {
    /// One isolated state directory for the whole test process, so tests never
    /// touch a real library or adopt a real playback session.
    private static let stateDir: String = {
        let dir = NSTemporaryDirectory()
            .appending("setbuddy-ui-tests-\(ProcessInfo.processInfo.processIdentifier)")
        setenv("SETBUDDY_STATE_DIR", dir, 1)
        return dir
    }()

    override func setUp() {
        super.setUp()
        _ = Self.stateDir
    }

    override class func tearDown() {
        try? FileManager.default.removeItem(atPath: stateDir)
        super.tearDown()
    }

    private func asset(_ name: String) -> URL {
        // Tests run from the package directory; the fixtures live with the
        // engine crate that generated them.
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()   // .../Tests/SetbuddyUITests
            .deletingLastPathComponent()   // .../Tests
            .deletingLastPathComponent()   // .../apps/mac
            .deletingLastPathComponent()   // .../apps
            .deletingLastPathComponent()   // repository root
            .appendingPathComponent("crates/setbuddy-mpv/tests/assets")
            .appendingPathComponent(name)
    }

    /// Poll until `condition` holds, letting queued main-actor work run.
    private func waitUntil(
        _ what: String,
        timeout: TimeInterval = 10,
        _ condition: () -> Bool
    ) async throws {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition() {
            if Date() > deadline {
                XCTFail("timed out waiting for \(what)")
                return
            }
            try await Task.sleep(for: .milliseconds(40))
        }
    }

    func testVideoFilePlaysAudioFirstAndPopsOutOnDemand() async throws {
        let model = PlayerModel()
        XCTAssertTrue(model.engineAvailable, "mpv must be installed to run these tests")
        defer { model.shutdown() }

        model.openFile(at: asset("long.webm"))
        try await waitUntil("playback to start") { (model.snapshot?.positionSecs ?? 0) > 0 }

        XCTAssertNil(model.lastError)
        XCTAssertEqual(model.snapshot?.track?.displayLabel, "long")
        XCTAssertTrue(model.snapshot?.hasVideo == true, "the file carries video")
        XCTAssertFalse(
            model.snapshot?.videoVisible == true,
            "a set opened from the menu bar plays audio-only until popped out"
        )

        model.toggleVideo()
        try await waitUntil("video to appear") { model.snapshot?.videoVisible == true }

        // The headline guarantee: popping out does not interrupt playback.
        let atPopOut = model.snapshot?.positionSecs ?? 0
        try await Task.sleep(for: .milliseconds(500))
        let afterPopOut = model.snapshot?.positionSecs ?? 0
        XCTAssertGreaterThan(
            afterPopOut, atPopOut,
            "playback should keep advancing while the video window is open"
        )

        model.toggleVideo()
        try await waitUntil("video to hide") { model.snapshot?.videoVisible == false }
        XCTAssertGreaterThanOrEqual(
            model.snapshot?.positionSecs ?? 0, afterPopOut,
            "hiding the video must never rewind playback"
        )
    }

    func testPlayPauseAndSeekDriveTheEngine() async throws {
        let model = PlayerModel()
        defer { model.shutdown() }

        model.openFile(at: asset("long.webm"))
        try await waitUntil("playback to start") { (model.snapshot?.positionSecs ?? 0) > 0 }
        XCTAssertTrue(model.isPlaying)

        model.togglePlayPause()
        try await waitUntil("pause to register") { model.snapshot?.paused == true }
        XCTAssertFalse(model.isPlaying)

        let held = model.snapshot?.positionSecs ?? 0
        try await Task.sleep(for: .milliseconds(400))
        XCTAssertEqual(
            model.snapshot?.positionSecs ?? 0, held, accuracy: 0.2,
            "a paused player should not advance"
        )

        model.togglePlayPause()
        try await waitUntil("playback to resume") { model.snapshot?.paused == false }

        model.seek(to: 150)
        try await waitUntil("seek to land") { (model.snapshot?.positionSecs ?? 0) > 149 }
    }

    func testPositionSurvivesAFullRestart() async throws {
        let first = PlayerModel()
        first.openFile(at: asset("long.webm"))
        try await waitUntil("playback to start") { (first.snapshot?.positionSecs ?? 0) > 0 }

        first.seek(to: 120)
        try await waitUntil("seek to land") { (first.snapshot?.positionSecs ?? 0) > 119 }
        // Let the ticker persist the position, the way it would in normal use.
        try await Task.sleep(for: .milliseconds(700))
        first.shutdown()

        // A fresh launch: same database, no running engine.
        let second = PlayerModel()
        defer { second.shutdown() }
        second.reloadLibrary()
        try await waitUntil("library to load") { !second.recents.isEmpty }

        guard let track = second.recents.first else {
            return XCTFail("the played track should appear in recents")
        }
        XCTAssertNotNil(track.resumeSecs, "a saved position should be offered")

        second.play(track)
        try await waitUntil("resumed playback") { (second.snapshot?.positionSecs ?? 0) > 119 }
        XCTAssertGreaterThan(
            second.snapshot?.positionSecs ?? 0, 119,
            "reopening should pick up where it left off"
        )
    }

    /// Releasing the scrubber must not show the old position again.
    ///
    /// The engine acknowledges a seek before it reports the new position, and
    /// the model used to hand the display straight back to the last pushed
    /// snapshot on release — so the thumb visibly sprang back to where the drag
    /// started before jumping forward a tick later.
    func testReleasingTheScrubberDoesNotSpringBack() async throws {
        let model = PlayerModel()
        defer { model.shutdown() }

        model.openFile(at: asset("long.webm"))
        try await waitUntil("playback to start") { (model.snapshot?.positionSecs ?? 0) > 0.2 }

        // Playback may start anywhere — an earlier test in this process leaves a
        // saved position for this fixture, and resuming is correct — so choose a
        // target well away from wherever it actually began.
        let start = model.displayPosition
        let target: Double = start > 100 ? 20 : 150
        XCTAssertGreaterThan(abs(target - start), 50, "target must be a real move")

        // Drag the thumb there and let go.
        model.isScrubbing = true
        model.scrubPosition = target
        model.seek(to: model.scrubPosition)
        model.isScrubbing = false

        // Immediately, with nothing awaited: the display already reads the new
        // position rather than the one the drag started from.
        XCTAssertEqual(
            model.displayPosition, target, accuracy: 0.5,
            "the released thumb should stay where it was dropped"
        )

        // And it must not fall back at any point while the engine catches up.
        for _ in 0..<14 {
            try await Task.sleep(for: .milliseconds(50))
            XCTAssertLessThan(
                abs(model.displayPosition - target), 10,
                "scrubber sprang back to \(model.displayPosition) from \(target)"
            )
        }
        XCTAssertLessThan(
            abs((model.snapshot?.positionSecs ?? 0) - target), 10,
            "and the engine really did move there"
        )
    }

    /// Skipping is clamped by the core, so the display must follow the clamped
    /// target rather than the requested delta.
    func testSkippingShowsTheClampedTargetImmediately() async throws {
        let model = PlayerModel()
        defer { model.shutdown() }

        model.openFile(at: asset("long.webm"))
        try await waitUntil("playback to start") { (model.snapshot?.positionSecs ?? 0) > 0.2 }

        model.skip(30)
        XCTAssertGreaterThan(model.displayPosition, 29, "forward skip shows at once")

        // Far past the end of a 200s file: the core clamps, and so must the UI.
        model.skip(10_000)
        XCTAssertLessThanOrEqual(model.displayPosition, 201)
        XCTAssertGreaterThan(model.displayPosition, 150)
    }

    func testArtworkIsExtractedForTheCurrentTrack() async throws {
        let model = PlayerModel()
        defer { model.shutdown() }

        model.openFile(at: asset("long.webm"))
        // A video file has no embedded cover, so this is a grabbed frame.
        try await waitUntil("artwork to load", timeout: 20) { model.artwork != nil }

        let size = model.artwork?.size ?? .zero
        XCTAssertGreaterThan(size.width, 0)
        XCTAssertGreaterThan(size.height, 0)
    }

    func testArtworkUsesEmbeddedCoverForTaggedAudio() async throws {
        let model = PlayerModel()
        defer { model.shutdown() }

        model.openFile(at: asset("tiny_art.mp3"))
        try await waitUntil("artwork to load", timeout: 20) { model.artwork != nil }
        XCTAssertGreaterThan(model.artwork?.size.width ?? 0, 0)
        XCTAssertEqual(model.snapshot?.track?.artist, "Test Artist",
                       "tags should have been read too")
    }

    func testRestartIgnoresTheSavedPosition() async throws {
        let model = PlayerModel()
        defer { model.shutdown() }

        model.openFile(at: asset("long.webm"))
        try await waitUntil("playback to start") { (model.snapshot?.positionSecs ?? 0) > 0 }
        model.seek(to: 120)
        try await waitUntil("seek to land") { (model.snapshot?.positionSecs ?? 0) > 119 }
        try await Task.sleep(for: .milliseconds(700))

        model.reloadLibrary()
        try await waitUntil("library to load") { !model.recents.isEmpty }
        guard let track = model.recents.first else {
            return XCTFail("expected a recent track")
        }

        model.restart(track)
        try await waitUntil("restart to take effect") {
            (model.snapshot?.positionSecs ?? 999) < 20
        }
    }

    /// Files reach the engine that can decode them, and crossing engines
    /// hands playback over rather than leaving two of them running.
    func testEachContainerReachesTheEngineThatDecodesIt() async throws {
        let model = PlayerModel()
        XCTAssertTrue(model.engineAvailable, "mpv must be installed to run these tests")
        defer { model.shutdown() }

        XCTAssertEqual(model.engineIds, ["avfoundation", "mpv"],
                       "AVFoundation is preferred, mpv is the fallback")

        model.openFile(at: asset("tiny.mp3"))
        try await waitUntil("AVFoundation to take the mp3") {
            model.snapshot?.engineId == "avfoundation"
        }
        try await waitUntil("AVFoundation to start playing") {
            (model.snapshot?.positionSecs ?? 0) > 0
        }
        XCTAssertNil(model.lastError)
        XCTAssertEqual(model.snapshot?.durationSecs ?? 0, 6, accuracy: 0.5,
                       "duration is read back from AVFoundation")

        // A webm AVFoundation cannot open goes to mpv, and playback moves with
        // it: the previous engine is stopped before the new one loads.
        model.openFile(at: asset("long.webm"))
        try await waitUntil("mpv to take the webm") { model.snapshot?.engineId == "mpv" }
        try await waitUntil("mpv to start playing") { (model.snapshot?.positionSecs ?? 0) > 0 }
        XCTAssertNil(model.lastError)
        XCTAssertEqual(model.snapshot?.track?.displayLabel, "long")
    }

    /// The set can play as the expanded player's backdrop instead of opening a
    /// window — but only while the engine holding the file lives in this
    /// process, which mpv never does.
    func testTheSetCanPlayBehindThePlayerInsteadOfInItsOwnWindow() async throws {
        let model = PlayerModel()
        defer {
            model.setVideoSurface(.window)
            model.shutdown()
        }
        XCTAssertTrue(model.panelVideoSupported, "an in-process engine is registered")

        model.setVideoSurface(.panel)
        XCTAssertEqual(model.videoSurface, .panel)

        model.openFile(at: asset("tiny.mp4"))
        try await waitUntil("AVFoundation to start playing") {
            model.snapshot?.engineId == "avfoundation" && (model.snapshot?.positionSecs ?? 0) > 0
        }
        try await waitUntil("the video track to be noticed") { model.snapshot?.hasVideo == true }
        XCTAssertFalse(model.panelVideoShowing, "nothing shows until the set is summoned")

        model.toggleVideo()
        try await waitUntil("the set to come out") { model.snapshot?.videoVisible == true }
        XCTAssertTrue(model.panelVideoShowing, "it plays behind the player, not in a window")
        XCTAssertNil(model.lastError)

        // Audio keeps going while it is on screen: the panel is a surface, not
        // a restart.
        let atSummon = model.snapshot?.positionSecs ?? 0
        try await Task.sleep(for: .milliseconds(400))
        XCTAssertGreaterThan(model.snapshot?.positionSecs ?? 0, atSummon)

        // A file only mpv can open ignores the setting: mpv has no surface to
        // hand over, so it opens its own window as always.
        model.openFile(at: asset("long.webm"))
        try await waitUntil("mpv to take the webm") { model.snapshot?.engineId == "mpv" }
        XCTAssertFalse(model.panelVideoShowing, "mpv cannot draw inside this process")
    }

    /// Every setting reads back the way it was left — the panel used to show
    /// "Automatic" after a restart however the engine had been forced.
    func testSettingsReadBackTheWayTheyWereLeft() async throws {
        let forced = try XCTUnwrap(PlayerModel().engineIds.first)

        let first = PlayerModel()
        first.setVideoSurface(.panel)
        first.setVideoWindowLayout("1280+100+50/0")
        first.setEnginePolicy(forced)
        XCTAssertNil(first.lastError)
        XCTAssertEqual(first.enginePolicy, forced, "the model reports what it just set")
        first.shutdown()

        let second = PlayerModel()
        defer {
            // Leave the shared state dir as the other tests expect to find it.
            second.setEnginePolicy("auto")
            second.setVideoSurface(.window)
            second.setVideoWindowLayout("40%")
            second.shutdown()
        }
        XCTAssertEqual(second.videoSurface, .panel, "where the set plays")
        XCTAssertEqual(second.videoWindowLayout, "1280+100+50/0", "the window layout")
        XCTAssertEqual(second.enginePolicy, forced, "the forced engine")
    }

    /// Expanding the player is what summons the set when the set plays there:
    /// there is no disc to press inside the expanded view.
    func testExpandingThePlayerSummonsTheSetWhenItPlaysInThePanel() async throws {
        let model = PlayerModel()
        defer {
            model.setExpanded(false)
            model.setVideoSurface(.window)
            model.shutdown()
        }
        model.setVideoSurface(.panel)

        model.openFile(at: asset("tiny.mp4"))
        try await waitUntil("the video track to be noticed") { model.panelVideoPossible }
        XCTAssertFalse(model.panelVideoShowing, "collapsed: nothing is showing")

        model.setExpanded(true)
        try await waitUntil("the set to come up with the player") { model.panelVideoShowing }

        model.setExpanded(false)
        try await waitUntil("the set to go away with it") {
            model.snapshot?.videoVisible == false
        }
        XCTAssertNil(model.lastError)
    }

    /// With the window as the surface, expanding is still only a view change:
    /// the set comes out when the disc is pressed and not before.
    func testExpandingLeavesTheWindowSurfaceAlone() async throws {
        let model = PlayerModel()
        defer {
            model.setExpanded(false)
            model.shutdown()
        }
        model.setVideoSurface(.window)

        model.openFile(at: asset("tiny.mp4"))
        try await waitUntil("playback to start") { (model.snapshot?.positionSecs ?? 0) > 0 }
        model.setExpanded(true)

        try await Task.sleep(for: .milliseconds(600))
        XCTAssertFalse(model.panelVideoPossible, "the panel is not the surface")
        XCTAssertEqual(model.snapshot?.videoVisible, false, "no window was opened by expanding")
    }

    /// The keys the expanded player answers. Mapped in the model so this can
    /// be checked without a window to type into.
    func testTheKeyboardMapIsWhatTheHiddenButtonsWere() {
        typealias Key = PlayerModel.PlayerKey
        XCTAssertEqual(Key.from(.space), .playPause)
        XCTAssertEqual(Key.from(.leftArrow), .skipBack)
        XCTAssertEqual(Key.from(.rightArrow), .skipForward)
        XCTAssertEqual(Key.from(.leftArrow, modifiers: .command), .previous)
        XCTAssertEqual(Key.from(.rightArrow, modifiers: .command), .next)
        XCTAssertEqual(Key.from(.upArrow), .volumeUp)
        XCTAssertEqual(Key.from(.downArrow), .volumeDown)
        XCTAssertEqual(Key.from(.escape), .collapse)
        XCTAssertEqual(Key.from(KeyEquivalent("r")), .restart)
        XCTAssertNil(Key.from(KeyEquivalent("q")), "an unclaimed key is left alone")
    }

    /// Every key goes through the core to the engine, so the menu bar and
    /// AVFoundation never hold different opinions about what is playing.
    func testTheKeyboardDrivesPlaybackThroughTheCore() async throws {
        let model = PlayerModel()
        defer {
            model.setExpanded(false)
            model.setVideoSurface(.window)
            model.shutdown()
        }
        model.setVideoSurface(.panel)
        model.openFile(at: asset("tiny.mp4"))
        try await waitUntil("AVFoundation to start playing") {
            model.snapshot?.engineId == "avfoundation" && (model.snapshot?.positionSecs ?? 0) > 0
        }

        model.perform(.playPause)
        try await waitUntil("the pause to reach AVFoundation") { model.snapshot?.paused == true }
        model.perform(.playPause)
        try await waitUntil("playback to resume") { model.snapshot?.paused == false }

        model.perform(.skipBack)
        XCTAssertEqual(model.displayPosition, 0, accuracy: 0.5, "a skip back near the top lands at the top")

        model.perform(.volumeDown)
        XCTAssertEqual(model.volume, 95, "volume steps without a slider")

        model.setExpanded(true)
        model.perform(.collapse)
        XCTAssertFalse(model.isExpanded, "escape puts the player away")
        XCTAssertNil(model.lastError)
    }

    /// Settings that cannot do anything are not offered. Forcing the engine
    /// that lives in this process leaves no window for anyone to place.
    func testUnusableSettingsAreNotOffered() async throws {
        let model = PlayerModel()
        defer {
            model.setEnginePolicy("auto")
            model.setVideoSurface(.window)
            model.shutdown()
        }

        XCTAssertTrue(model.windowPlacementPossible, "automatic can still reach mpv")
        XCTAssertTrue(model.videoWindowSettingsApply)
        XCTAssertFalse(
            model.outOfProcessEngines.contains { $0.id == "avfoundation" },
            "the in-process engine is not one that can be placed"
        )

        model.setEnginePolicy("avfoundation")
        XCTAssertFalse(model.windowPlacementPossible, "nothing left with a window to place")
        XCTAssertTrue(model.videoWindowSettingsApply, "its own window is still sized here")

        model.setVideoSurface(.panel)
        XCTAssertFalse(
            model.videoWindowSettingsApply,
            "no window anywhere: the whole section has nothing to govern"
        )

        // Engines are named, never spelled out as ids in a view.
        XCTAssertEqual(
            model.engines.first { $0.id == "avfoundation" }?.displayName,
            "AVFoundation"
        )
    }

    /// A policy naming an engine that is not registered would make every file
    /// unplayable. The core refuses it, and the model must surface that rather
    /// than leaving settings showing a choice that never took.
    func testForcingAnEngineThatIsNotThereIsRefused() async throws {
        let model = PlayerModel()
        defer {
            model.setEnginePolicy("auto")
            model.shutdown()
        }

        model.setEnginePolicy("avfoundation")
        XCTAssertEqual(model.enginePolicy, "avfoundation")
        XCTAssertNil(model.lastError)

        model.setEnginePolicy("gstreamer")
        XCTAssertNotNil(model.lastError, "the refusal is shown, not swallowed")
        XCTAssertEqual(
            model.enginePolicy,
            "avfoundation",
            "settings keep showing what is really in force"
        )
    }

    /// Registration order is preference order, and it is the *engine list* the
    /// model reads — never an id spelled out in a view. A front end that
    /// registered different engines would still read correctly here.
    func testEnginesAreListedInPreferenceOrderWithTheirCapabilities() async throws {
        let model = PlayerModel()
        defer { model.shutdown() }

        XCTAssertFalse(model.engines.isEmpty)
        XCTAssertEqual(
            model.engineIds.first,
            "avfoundation",
            "the in-process engine is registered ahead of the subprocess one"
        )
        for engine in model.engines {
            XCTAssertFalse(engine.displayName.isEmpty, "\(engine.id) needs a name to show")
            XCTAssertFalse(engine.containers.isEmpty, "\(engine.id) claims nothing")
            XCTAssertEqual(
                engine.containers.map { $0.lowercased() },
                engine.containers,
                "containers are matched case-insensitively but stored lowercase"
            )
        }
        XCTAssertTrue(
            model.panelVideoSupported,
            "an engine in this process is what makes the panel surface possible"
        )
    }

    /// The staging workflow end to end: pick a folder, arrange what landed,
    /// then play it. Nothing may start playing until that last step.
    func testStagingAFolderQueuesItInOrderAndPlaysOnlyWhenAsked() async throws {
        let model = PlayerModel()
        XCTAssertTrue(model.engineAvailable, "mpv must be installed to run these tests")
        defer { model.shutdown() }

        model.clearQueue()
        let folder = asset("long.webm").deletingLastPathComponent()
        model.stage(folder)
        try await waitUntil("the folder to finish staging") {
            !model.isStaging && model.queue.count > 1
        }

        XCTAssertNil(model.lastError)
        XCTAssertNil(model.snapshot?.track, "staging must not start playback")
        // Path order, not label order: a tagged file's label bears no relation
        // to its filename.
        XCTAssertEqual(
            model.queue.map(\.path),
            model.queue.map(\.path).sorted(),
            "a staged folder arrives in path order"
        )

        let second = model.queue[1].id
        model.moveQueueItem(from: 1, to: 0)
        XCTAssertEqual(model.queue.first?.id, second, "the queue reorders")

        model.playStage()
        try await waitUntil("playback to start") { (model.snapshot?.positionSecs ?? 0) > 0 }
        XCTAssertEqual(model.snapshot?.track?.id, second, "plays from the top of the stage")
    }
}
