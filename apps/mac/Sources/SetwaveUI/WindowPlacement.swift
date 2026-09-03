import AppKit
import SwiftUI

/// Reads a placed video window back into a layout.
///
/// mpv has no property for where its window is, but the window server does:
/// `CGWindowListCopyWindowInfo` hands out any on-screen window's bounds by
/// owner pid, with no permission prompt (only window *titles* are gated). The
/// bounds arrive in points from the main screen's top-left; mpv wants physical
/// pixels from the top-left of the usable area of whichever screen the window
/// is on. This does that conversion.
enum WindowPlacement {
    /// The layout spec — `width+x+y/screen` — for the largest on-screen window
    /// owned by `pid`, or nil when it has none.
    static func layoutSpec(forWindowOf pid: pid_t) -> String? {
        let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID)
            as? [[String: Any]] ?? []
        let bounds = windows
            .filter { ($0[kCGWindowOwnerPID as String] as? pid_t) == pid }
            .compactMap { $0[kCGWindowBounds as String] as? [String: CGFloat] }
            .map {
                CGRect(x: $0["X"] ?? 0, y: $0["Y"] ?? 0,
                       width: $0["Width"] ?? 0, height: $0["Height"] ?? 0)
            }
            .max { $0.width * $0.height < $1.width * $1.height }
        guard let bounds, bounds.width > 0, let main = NSScreen.screens.first else { return nil }

        // CG bounds are top-left-down from the main screen's top-left; NSScreen
        // frames are bottom-left-up. Convert each screen into CG space once.
        func cgFrame(_ screen: NSScreen) -> CGRect {
            CGRect(x: screen.frame.minX,
                   y: main.frame.height - screen.frame.maxY,
                   width: screen.frame.width,
                   height: screen.frame.height)
        }
        let center = CGPoint(x: bounds.midX, y: bounds.midY)
        let (index, screen) = NSScreen.screens.enumerated()
            .first { cgFrame($0.element).contains(center) }
            .map { ($0.offset, $0.element) } ?? (0, main)

        // mpv measures y from below the menu bar, and everything in physical
        // pixels — verified by round-tripping known geometries.
        let frame = cgFrame(screen)
        let menuBar = screen.frame.maxY - screen.visibleFrame.maxY
        let scale = screen.backingScaleFactor
        let width = Int((bounds.width * scale).rounded())
        let x = Int(((bounds.minX - frame.minX) * scale).rounded())
        let y = Int(((bounds.minY - frame.minY - menuBar) * scale).rounded())
        return "\(width)+\(max(x, 0))+\(max(y, 0))/\(index)"
    }

    /// Screen names by index, for showing which screen a layout names.
    static var screenNames: [String] {
        NSScreen.screens.map(\.localizedName)
    }
}

// MARK: - Placement overlay

/// Takes the screen over while the user places the set window.
///
/// One dimmed, click-swallowing window per screen, so the rest of the desktop
/// is visibly out of play — the way a screen-share picker does it — and a
/// separate small HUD window carrying the live readout and Save/Cancel.
///
/// Levels are the whole trick. mpv's window uses `ontop-level=system`, which
/// on macOS is one above the status bar, so the dimming sits *at* the status
/// bar level (under mpv, over everything else) and the HUD sits well above
/// mpv, so it stays clickable even when the set window is dragged over it.
///
/// Setwave's own panel and popover are hidden on the way in: a menu bar panel
/// closes the moment the user clicks the window they are placing anyway, and
/// leaving it half-open behind the dimming only confuses.
@MainActor
final class PlacementOverlay {
    private let session: PlacementSession
    private var dimmers: [PlacementWindow] = []
    private var hud: NSWindow?
    private var timer: Timer?

    init(pid: pid_t, onSave: @escaping () -> Void, onCancel: @escaping () -> Void) {
        session = PlacementSession(pid: pid, onSave: onSave, onCancel: onCancel)

        // Everything Setwave has up is the menu bar panel and its popover.
        for window in NSApp.windows where window.isVisible {
            window.orderOut(nil)
        }

        for screen in NSScreen.screens {
            let dimmer = PlacementWindow(frame: screen.frame, level: .statusBar, session: session)
            dimmer.backgroundColor = NSColor.black.withAlphaComponent(0.45)
            dimmer.orderFrontRegardless()
            dimmers.append(dimmer)
        }

        // The HUD goes on the screen with the mouse: that is where the user is
        // looking, and where the placement window most likely is.
        let mouse = NSEvent.mouseLocation
        let hudScreen = NSScreen.screens.first { $0.frame.contains(mouse) }
            ?? NSScreen.main ?? NSScreen.screens[0]
        let hosting = NSHostingView(rootView: PlacementHUD(session: session))
        let size = hosting.fittingSize
        let visible = hudScreen.visibleFrame
        let frame = NSRect(
            x: visible.midX - size.width / 2,
            y: visible.minY + 48,
            width: size.width,
            height: size.height
        )
        let hud = PlacementWindow(frame: frame, level: .screenSaver, session: session)
        hud.backgroundColor = .clear
        hud.contentView = hosting
        hud.orderFrontRegardless()
        self.hud = hud
        for dimmer in dimmers {
            dimmer.focusTarget = hud
        }

        // An accessory app has to ask for focus; without it the HUD cannot be
        // key and Return goes nowhere.
        NSApp.activate(ignoringOtherApps: true)
        hud.makeKeyAndOrderFront(nil)

        timer = Timer.scheduledTimer(withTimeInterval: 0.2, repeats: true) { [session] _ in
            Task { @MainActor in session.refresh() }
        }
        session.refresh()
    }

    func close() {
        timer?.invalidate()
        timer = nil
        hud?.orderOut(nil)
        hud = nil
        for dimmer in dimmers {
            dimmer.orderOut(nil)
        }
        dimmers.removeAll()
    }
}

/// What the HUD shows and what its keys do.
@MainActor
final class PlacementSession: ObservableObject {
    let pid: pid_t
    let onSave: () -> Void
    let onCancel: () -> Void
    /// The layout as it would be saved right now, in words.
    @Published var readout = "Looking for the window…"
    @Published var found = false

    init(pid: pid_t, onSave: @escaping () -> Void, onCancel: @escaping () -> Void) {
        self.pid = pid
        self.onSave = onSave
        self.onCancel = onCancel
    }

    func refresh() {
        guard let spec = WindowPlacement.layoutSpec(forWindowOf: pid) else {
            found = false
            readout = "Looking for the window…"
            return
        }
        found = true
        // "1280 px wide at 100, 50 on Built-in Retina Display"
        let (geometry, screen) = spec.split(separator: "/", maxSplits: 1)
            .map(String.init)
            .reduce(into: ("", "")) { pair, part in
                if pair.0.isEmpty { pair.0 = part } else { pair.1 = part }
            }
        let parts = geometry.split(separator: "+").map(String.init)
        var text = "\(parts.first ?? geometry) px wide"
        if parts.count == 3 {
            text += " at \(parts[1]), \(parts[2])"
        }
        let names = WindowPlacement.screenNames
        if names.count > 1, let index = Int(screen), names.indices.contains(index) {
            text += " on \(names[index])"
        }
        readout = text
    }
}

/// A borderless window used both for the dimming (one per screen) and for the
/// HUD. Can be key, and answers Return and Escape either way, so the keys work
/// whichever of them the user last clicked.
private final class PlacementWindow: NSWindow {
    private let session: PlacementSession
    /// Where a click on this window sends key focus — the HUD, for a dimmer —
    /// so Return works again after the user has been dragging mpv's window.
    weak var focusTarget: NSWindow?

    init(frame: NSRect, level: NSWindow.Level, session: PlacementSession) {
        self.session = session
        super.init(
            contentRect: frame,
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        self.level = level
        isOpaque = false
        hasShadow = false
        ignoresMouseEvents = false
        isReleasedWhenClosed = false
        collectionBehavior = [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary]
        setFrame(frame, display: false)
    }

    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { true }

    override func keyDown(with event: NSEvent) {
        switch event.keyCode {
        case 36, 76: session.onSave()   // Return, keypad Enter
        case 53: session.onCancel()     // Escape
        default: super.keyDown(with: event)
        }
    }

    override func mouseDown(with event: NSEvent) {
        (focusTarget ?? self).makeKeyAndOrderFront(nil)
    }
}

/// The instructions and buttons. Its own small window, so it is sized to fit.
private struct PlacementHUD: View {
    @ObservedObject var session: PlacementSession

    var body: some View {
        VStack(spacing: 10) {
            HStack(spacing: 10) {
                Image(systemName: "macwindow.on.rectangle")
                    .font(.title2)
                    .foregroundStyle(Color.accentColor)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Place the set window")
                        .font(.headline)
                    Text("Drag it where you want the set to appear, and resize it. The window is empty on purpose.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            Text(session.readout)
                .font(.callout.monospacedDigit())
                .foregroundStyle(session.found ? Color.primary : .secondary)
            HStack(spacing: 12) {
                Button("Cancel", action: session.onCancel)
                    .keyboardShortcut(.cancelAction)
                Button("Save Placement", action: session.onSave)
                    .keyboardShortcut(.defaultAction)
                    .disabled(!session.found)
            }
            .controlSize(.large)
            Text("Return saves · Esc cancels")
                .font(.caption2)
                .foregroundStyle(.tertiary)
        }
        .padding(20)
        .frame(width: 420)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14))
        .padding(12)
    }
}
