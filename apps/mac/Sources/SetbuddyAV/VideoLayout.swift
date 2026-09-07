import AppKit

/// The pop-out's size and place, in the same grammar the store holds and the
/// Rust `VideoWindowLayout` parses: `"40%"`, `"fill"`, `"fullscreen"`,
/// `"1280"`, `"1280+100+50"`, `"1280+100+50/1"`.
///
/// Parsed here rather than crossing the boundary as a typed value because the
/// setting *is* a string — one grammar, parsed at each edge. Keep this in step
/// with `VideoWindowLayout::parse`; its round-trip test is the specification.
enum VideoLayout: Equatable {
    /// A fraction of the screen's width, in (0, 1].
    case screenFraction(Double)
    /// As large as fits the screen, still a window.
    case fill
    case fullscreen
    /// A width in physical pixels, optionally placed, optionally on a screen.
    case custom(width: Int, position: Position?, screen: Int?)

    /// Physical pixels from the top-left of a screen's usable area — the same
    /// numbers `WindowPlacement.layoutSpec` reads back out.
    struct Position: Equatable {
        let x: Int
        let y: Int
    }

    static func parse(_ spec: String) -> VideoLayout? {
        let spec = spec.trimmingCharacters(in: .whitespaces)
        if spec.lowercased() == "fill" { return .fill }

        var head = spec
        var screen: Int?
        if let slash = spec.firstIndex(of: "/") {
            head = spec[spec.startIndex..<slash].trimmingCharacters(in: .whitespaces)
            let tail = spec[spec.index(after: slash)...].trimmingCharacters(in: .whitespaces)
            guard let index = Int(tail), index >= 0 else { return nil }
            screen = index
        }
        if head.lowercased() == "fullscreen" { return .fullscreen }
        if head.hasSuffix("%") {
            let number = head.dropLast().trimmingCharacters(in: .whitespaces)
            guard let percent = Double(number), percent > 0, percent <= 100 else { return nil }
            return .screenFraction(percent / 100)
        }

        let parts = head.split(separator: "+", omittingEmptySubsequences: false)
            .map { $0.trimmingCharacters(in: .whitespaces) }
        guard let width = Int(parts[0]), width > 0 else { return nil }
        switch parts.count {
        case 1:
            return .custom(width: width, position: nil, screen: screen)
        case 3:
            guard let x = Int(parts[1]), let y = Int(parts[2]) else { return nil }
            return .custom(width: width, position: Position(x: x, y: y), screen: screen)
        default:
            // A position needs both axes, and nothing follows it.
            return nil
        }
    }

    /// The window frame this layout asks for, in AppKit points from the
    /// bottom-left of the main screen. Nil when there is no screen at all.
    ///
    /// Height is never specified: the window follows the video's aspect, so a
    /// width is the whole size.
    func frame(aspect: Double) -> NSRect? {
        guard let main = NSScreen.screens.first else { return nil }
        let screens = NSScreen.screens
        var screen = NSScreen.main ?? main
        if case .custom(_, _, let index) = self, let index, screens.indices.contains(index) {
            screen = screens[index]
        }
        let visible = screen.visibleFrame

        switch self {
        case .fullscreen:
            return screen.frame
        case .screenFraction(let fraction):
            return Self.centred(width: visible.width * fraction, aspect: aspect, in: visible)
        case .fill:
            // A box the size of the screen, not a width: sizing by width alone
            // pushes a tall video off the bottom.
            return Self.centred(
                width: min(visible.width, visible.height * aspect),
                aspect: aspect,
                in: visible
            )
        case .custom(let widthPixels, let position, _):
            let scale = screen.backingScaleFactor
            let width = Double(widthPixels) / scale
            let height = width / aspect
            guard let position else {
                return Self.centred(width: width, aspect: aspect, in: visible)
            }
            // The exact inverse of `WindowPlacement.layoutSpec`: it reports
            // physical pixels measured from the top-left of the screen's usable
            // area, in the window server's top-left-down space. Undo both.
            let cgFrame = NSRect(
                x: screen.frame.minX,
                y: main.frame.height - screen.frame.maxY,
                width: screen.frame.width,
                height: screen.frame.height
            )
            let menuBar = screen.frame.maxY - screen.visibleFrame.maxY
            let cgX = cgFrame.minX + Double(position.x) / scale
            let cgY = cgFrame.minY + menuBar + Double(position.y) / scale
            return NSRect(
                x: cgX,
                y: main.frame.height - (cgY + height),
                width: width,
                height: height
            )
        }
    }

    /// Middle of the screen, at the video's aspect, never smaller than a
    /// window worth having.
    private static func centred(width: Double, aspect: Double, in visible: NSRect) -> NSRect {
        let width = max(width, 240)
        let height = width / aspect
        return NSRect(
            x: visible.midX - width / 2,
            y: visible.midY - height / 2,
            width: width,
            height: height
        )
    }
}
