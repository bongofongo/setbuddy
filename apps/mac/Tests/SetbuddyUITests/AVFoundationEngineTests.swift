import AVFoundation
import AppKit
import XCTest
@testable import SetbuddyAV

/// The engine on its own, without the core or the app around it.
@MainActor
final class AVFoundationEngineTests: XCTestCase {
    private func asset(_ name: String) -> String {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()   // .../Tests/SetbuddyUITests
            .deletingLastPathComponent()   // .../Tests
            .deletingLastPathComponent()   // .../apps/mac
            .deletingLastPathComponent()   // .../apps
            .deletingLastPathComponent()   // repository root
            .appendingPathComponent("crates/setbuddy-mpv/tests/assets")
            .appendingPathComponent(name)
            .path
    }

    private func waitUntil(
        _ what: String,
        timeout: TimeInterval = 5,
        _ condition: () -> Bool
    ) async throws {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition() {
            if Date() > deadline { return XCTFail("timed out waiting for \(what)") }
            try await Task.sleep(for: .milliseconds(40))
        }
    }

    /// Put the view on screen, the way the app's panel does. Without a window
    /// the engine is right to keep video decoding switched off, so nothing
    /// would ever draw.
    private func host(_ view: NSView) -> NSWindow {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 320, height: 180),
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        window.contentView = view
        window.orderFrontRegardless()
        return window
    }

    /// The panel view is built by the app only once a snapshot says the set is
    /// out, so on the first summon after launch it arrives *after* the picture
    /// was handed over. It used to come up black, and to work on every summon
    /// after that, because by then the view already existed.
    func testAPanelViewBuiltAfterTheSummonStillGetsThePicture() async throws {
        let engine = AVFoundationEngine()
        defer { engine.shutdown() }

        engine.setVideoSurface(.panel)
        try engine.load(path: asset("tiny.mp4"), startAt: nil)
        try engine.setVideoVisible(visible: true)
        // Deliberately nothing asks for the view until after the summon.
        try await Task.sleep(for: .milliseconds(200))

        let view = try XCTUnwrap(engine.panelVideoView() as? PanelVideoView)
        let window = host(view)
        defer { window.orderOut(nil) }
        try await waitUntil("the picture to reach the panel") { view.isAttached }
        try await waitUntil("a frame to arrive") { view.isReadyForDisplay }
    }

    /// Video decoding is switched off while nothing is showing. Switching it
    /// back on has to actually produce frames again.
    func testShowingTheSetAgainAfterItWasHiddenStillDraws() async throws {
        let engine = AVFoundationEngine()
        defer { engine.shutdown() }

        engine.setVideoSurface(.panel)
        let view = try XCTUnwrap(engine.panelVideoView() as? PanelVideoView)
        let window = host(view)
        defer { window.orderOut(nil) }
        try engine.load(path: asset("tiny.mp4"), startAt: nil)
        // Long enough for the item to become ready with the picture switched
        // off, which is what a set played before the player is expanded does.
        try await Task.sleep(for: .milliseconds(400))

        try engine.setVideoVisible(visible: true)
        try await waitUntil("a frame after the first summon") { view.isReadyForDisplay }

        try engine.setVideoVisible(visible: false)
        try await Task.sleep(for: .milliseconds(200))
        try engine.setVideoVisible(visible: true)
        try await waitUntil("a frame after the second summon") { view.isReadyForDisplay }
    }
}
