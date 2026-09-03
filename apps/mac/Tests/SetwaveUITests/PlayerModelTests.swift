import XCTest
@testable import SetwaveUI

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
            .appending("setwave-ui-tests-\(ProcessInfo.processInfo.processIdentifier)")
        setenv("SETWAVE_STATE_DIR", dir, 1)
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
            .deletingLastPathComponent()   // .../Tests/SetwaveUITests
            .deletingLastPathComponent()   // .../Tests
            .deletingLastPathComponent()   // .../apps/mac
            .deletingLastPathComponent()   // .../apps
            .deletingLastPathComponent()   // repository root
            .appendingPathComponent("crates/setwave-mpv/tests/assets")
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
