import AVFoundation
import AVKit
import AppKit
import SetbuddyCore

/// Playback through AVFoundation, satisfying the same contract mpv does.
///
/// Written in Swift and handed to the Rust core as a foreign engine, which is
/// what `tests/swift/main.swift` has been proving was possible since v1. The
/// core cannot tell it from mpv: it asks capabilities, gets a file it can
/// decode natively, and drives it through `PlaybackEngine`.
///
/// Two rules shape everything here:
///
/// * **`snapshot()` never touches AVFoundation.** Every value it returns is
///   read from a cached record under a lock, published from the periodic time
///   observer and from the calls themselves. AVFoundation would answer only on
///   the main thread, and the ticker asks from a Rust one.
/// * **Nothing blocks the caller.** Each method updates that cache and hands
///   the AV work to the main queue, always asynchronously so calls keep the
///   order they were made in. The cache is updated optimistically, so a
///   snapshot taken between the call and its main-thread landing already
///   reflects what was asked for.
public final class AVFoundationEngine: PlaybackEngine, @unchecked Sendable {
    /// What AVFoundation decodes natively. webm, mkv, opus and flac are
    /// deliberately absent — they are mpv's, and claiming them here would route
    /// a set to a player that cannot open it.
    private static let containers = ["mp3", "m4a", "mp4", "mov", "aac", "wav", "aiff"]

    private static let idleState = EngineSnapshot(
        positionSecs: nil,
        durationSecs: nil,
        paused: false,
        idle: true,
        eof: false,
        hasVideo: false,
        videoVisible: false,
        path: nil
    )

    private let lock = NSLock()
    private var state = AVFoundationEngine.idleState

    // Everything below is main-queue-only state; it is only ever read or
    // written inside `onMain`.
    private var player: AVPlayer?
    private var playerView: AVPlayerView?
    private var window: NSWindow?
    private var timeObserver: Any?
    private var endObserver: NSObjectProtocol?
    private var closeObserver: NSObjectProtocol?
    private var statusObservation: NSKeyValueObservation?
    private var rateObservation: NSKeyValueObservation?
    private var volume: Double = 100
    private var speed: Double = 1
    private var ontop = false
    private var layout: VideoLayout = .screenFraction(0.4)
    private var surface: VideoSurface = .window
    private var panelView: PanelVideoView?
    /// Whether the app's panel view is on screen right now. Distinct from
    /// `videoVisible`: the set stays summoned while the menu bar panel is
    /// closed, it just has nowhere to draw, so decoding stops and audio does
    /// not.
    private var panelOnScreen = false
    /// Bumped on every load so an asset inspection that finishes late cannot
    /// describe the previous file.
    private var generation = 0
    private var didShutdown = false

    public init() {}

    // MARK: - PlaybackEngine

    public func capabilities() -> EngineCapabilities {
        EngineCapabilities(
            id: "avfoundation",
            displayName: "AVFoundation",
            containers: Self.containers,
            video: true,
            ontopWindow: true,
            // The one thing mpv cannot offer: system Picture-in-Picture.
            nativePip: true
        )
    }

    public func load(path: String, startAt: Double?) throws {
        guard FileManager.default.fileExists(atPath: path) else {
            throw SetbuddyError.Playback(message: "\(path) is no longer on disk")
        }
        mutate {
            $0.path = path
            $0.positionSecs = startAt ?? 0
            $0.durationSecs = nil
            $0.paused = false
            $0.idle = false
            $0.eof = false
            $0.hasVideo = false
        }

        let url = URL(fileURLWithPath: path)
        onMain { [self] in
            let player = ensurePlayer()
            generation += 1
            let generation = self.generation
            clearItemObservers()

            let item = AVPlayerItem(url: url)
            endObserver = NotificationCenter.default.addObserver(
                forName: .AVPlayerItemDidPlayToEndTime,
                object: item,
                queue: .main
            ) { [weak self] _ in
                // The core watches `eof` to advance the queue; holding the last
                // frame rather than tearing down is what makes that observable.
                self?.mutate { $0.eof = true; $0.paused = true }
            }
            statusObservation = item.observe(\.status, options: [.new]) { [weak self] item, _ in
                guard let self else { return }
                switch item.status {
                case .failed:
                    self.failed(item.error)
                case .readyToPlay:
                    // The item's tracks only exist now, and they are what
                    // video decoding is switched on and off through.
                    self.onMain { self.updateVideoDecoding() }
                default:
                    break
                }
            }

            player.replaceCurrentItem(with: item)
            if let startAt, startAt > 0 {
                player.seek(to: Self.time(startAt), toleranceBefore: .zero, toleranceAfter: .zero)
            }
            player.playImmediately(atRate: Float(speed))
            window?.title = url.lastPathComponent

            // Whether there is a picture to pop out is not known until the
            // asset has been read, and reading it is I/O — so ask off the main
            // thread and publish the answer when it arrives.
            Task { @MainActor [weak self] in
                let tracks = try? await AVURLAsset(url: url).loadTracks(withMediaType: .video)
                guard let self, self.generation == generation else { return }
                self.mutate { $0.hasVideo = !(tracks ?? []).isEmpty }
            }
        }
    }

    public func setPaused(paused: Bool) throws {
        mutate { $0.paused = paused }
        onMain { [self] in
            guard let player else { return }
            if paused {
                player.pause()
            } else {
                player.playImmediately(atRate: Float(speed))
            }
        }
    }

    public func seekAbsolute(seconds: Double) throws {
        let target = max(seconds, 0)
        // Optimistic, so a scrubber released here does not spring back to the
        // last observed position while the seek lands.
        mutate { $0.positionSecs = target; $0.eof = false }
        onMain { [self] in
            player?.seek(to: Self.time(target), toleranceBefore: .zero, toleranceAfter: .zero)
        }
    }

    public func setVolume(percent: Double) throws {
        let percent = min(max(percent, 0), 100)
        onMain { [self] in
            volume = percent
            player?.volume = Float(percent / 100)
        }
    }

    public func setSpeed(rate: Double) throws {
        let rate = max(rate, 0.01)
        onMain { [self] in
            speed = rate
            // Setting a rate on a paused player would start it playing.
            if let player, player.timeControlStatus != .paused {
                player.rate = Float(rate)
            }
        }
    }

    /// The pop-out. Audio is untouched either way: the window only carries a
    /// view onto the player, so ordering it out is not a playback change.
    public func setVideoVisible(visible: Bool) throws {
        mutate { $0.videoVisible = visible }
        onMain { [self] in applyPresentation() }
    }

    public func setVideoOntop(ontop: Bool) throws {
        onMain { [self] in
            self.ontop = ontop
            window?.level = ontop ? .floating : .normal
        }
    }

    public func setVideoWindowLayout(spec: String) throws {
        guard let layout = VideoLayout.parse(spec) else {
            throw SetbuddyError.Playback(message: "\"\(spec)\" is not a window layout")
        }
        onMain { [self] in
            self.layout = layout
            // Size and position land on the next pop-out — resizing a window
            // the user is watching is not what "choose a size" means — but
            // fullscreen is applied at once, matching the mpv engine.
            if case .fullscreen = layout, let window, window.isVisible {
                applyLayout(to: window)
            }
        }
    }

    public func stop() throws {
        mutate { $0 = Self.idleState }
        onMain { [self] in
            clearItemObservers()
            player?.replaceCurrentItem(with: nil)
            window?.orderOut(nil)
            panelView?.attach(nil)
        }
    }

    public func snapshot() -> EngineSnapshot {
        lock.lock(); defer { lock.unlock() }
        return state
    }

    // MARK: - The app's own surface
    //
    // Not part of `PlaybackEngine`, and deliberately so: this is what an engine
    // running inside the app's process can offer that one in a subprocess never
    // can. The app holds this engine directly, so it can ask; the core, the
    // contract, and mpv are all untouched by it.

    /// Where the picture goes when video is switched on. Takes effect at once
    /// if the set is already out.
    ///
    /// Main thread — the app changes this from a settings control.
    public func setVideoSurface(_ surface: VideoSurface) {
        dispatchPrecondition(condition: .onQueue(.main))
        guard surface != self.surface else { return }
        self.surface = surface
        applyPresentation()
    }

    /// The view for the app to host. The same view every time: it owns the
    /// player layer, and rebuilding that would tear the picture down.
    ///
    /// Main thread.
    public func panelVideoView() -> NSView {
        dispatchPrecondition(condition: .onQueue(.main))
        if let panelView { return panelView }
        // Not just decoding: the app builds this view lazily, only rendering
        // it once a snapshot says the set is out, so on the first summon after
        // launch the view arrives *after* the player was handed over and there
        // was no surface to hand it to. Re-deciding the presentation when the
        // surface appears is what puts the picture in it — without this the
        // first summon came up black and every one after it worked, because by
        // then the view already existed.
        let view = PanelVideoView { [weak self] onScreen in
            guard let self else { return }
            self.panelOnScreen = onScreen
            self.applyPresentation()
        }
        panelView = view
        return view
    }

    /// Idempotent teardown. There is no subprocess to orphan — the player dies
    /// with the app — so this is about releasing the window and the observers.
    public func shutdown() {
        mutate { $0 = Self.idleState }
        onMain { [self] in
            guard !didShutdown else { return }
            didShutdown = true
            clearItemObservers()
            if let timeObserver, let player {
                player.removeTimeObserver(timeObserver)
            }
            timeObserver = nil
            rateObservation?.invalidate()
            rateObservation = nil
            player?.replaceCurrentItem(with: nil)
            player = nil
            playerView?.player = nil
            playerView = nil
            if let closeObserver {
                NotificationCenter.default.removeObserver(closeObserver)
            }
            closeObserver = nil
            window?.orderOut(nil)
            window?.contentView = nil
            window = nil
            panelView?.attach(nil)
            panelView = nil
        }
    }

    // MARK: - Main-queue work

    private func onMain(_ body: @escaping () -> Void) {
        // Always async, never "inline if already on main": a call made from the
        // main thread must not overtake one already queued from the ticker.
        DispatchQueue.main.async(execute: body)
    }

    private func mutate(_ change: (inout EngineSnapshot) -> Void) {
        lock.lock(); defer { lock.unlock() }
        change(&state)
    }

    private func ensurePlayer() -> AVPlayer {
        if let player { return player }
        let player = AVPlayer()
        // Hold at the end rather than advancing: the queue is the core's.
        player.actionAtItemEnd = .pause
        player.volume = Float(volume / 100)
        // Position and duration are published from here and nowhere else.
        timeObserver = player.addPeriodicTimeObserver(
            forInterval: CMTime(seconds: 0.1, preferredTimescale: 600),
            queue: .main
        ) { [weak self, weak player] time in
            self?.publish(time, of: player)
        }
        // Playback state as AVFoundation actually has it, not only as it was
        // asked for. A stall, an interruption, or the window surface's own
        // controls all change it behind our back, and with the on-screen
        // transport hidden the menu bar's idea of "playing" is the only one
        // the user has left.
        rateObservation = player.observe(\.timeControlStatus, options: [.new]) { [weak self] player, _ in
            self?.mutate { $0.paused = player.timeControlStatus == .paused }
        }
        self.player = player
        return player
    }

    private func publish(_ time: CMTime, of player: AVPlayer?) {
        let duration = player?.currentItem?.duration
        mutate {
            if time.isNumeric {
                $0.positionSecs = max(time.seconds, 0)
            }
            if let duration, duration.isNumeric, duration.seconds.isFinite {
                $0.durationSecs = duration.seconds
            }
        }
    }

    /// A file AVFoundation turned out not to be able to open. The contract has
    /// no channel for an error found after `load` returned, so the engine goes
    /// idle — the core then reports nothing playing rather than a frozen
    /// position — and says why in the log.
    private func failed(_ error: Error?) {
        NSLog("Setbuddy AVFoundation: %@", error?.localizedDescription ?? "the file failed to open")
        mutate {
            // The window, if it is up, is still up.
            let videoVisible = $0.videoVisible
            $0 = Self.idleState
            $0.videoVisible = videoVisible
        }
    }

    private func clearItemObservers() {
        if let endObserver {
            NotificationCenter.default.removeObserver(endObserver)
        }
        endObserver = nil
        statusObservation?.invalidate()
        statusObservation = nil
    }

    private func ensureWindow() -> NSWindow {
        if let window { return window }
        let view = AVPlayerView()
        view.player = ensurePlayer()
        view.controlsStyle = .floating
        // The capability that justifies this engine's existence.
        view.allowsPictureInPicturePlayback = true
        // Setbuddy owns the Now Playing widget; two writers fight over it.
        view.updatesNowPlayingInfoCenter = false

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 960, height: 540),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.contentView = view
        window.isReleasedWhenClosed = false
        window.collectionBehavior = [.fullScreenPrimary]
        window.level = ontop ? .floating : .normal
        window.title = snapshot().path.map { ($0 as NSString).lastPathComponent } ?? "Setbuddy"
        // Closing the window is the pop-out going away, not playback stopping.
        closeObserver = NotificationCenter.default.addObserver(
            forName: NSWindow.willCloseNotification,
            object: window,
            queue: .main
        ) { [weak self] _ in
            self?.mutate { $0.videoVisible = false }
        }
        self.window = window
        playerView = view
        return window
    }

    /// Put the picture where the current surface says it goes, or take it away.
    ///
    /// One place decides this, because the two surfaces are exclusive: an
    /// `AVPlayer` drives one layer at a time, so handing it to the panel means
    /// taking it back off the window's player view, and the other way round.
    private func applyPresentation() {
        let visible = snapshot().videoVisible
        switch (surface, visible) {
        case (.window, true):
            panelView?.attach(nil)
            let window = ensureWindow()
            playerView?.player = player
            applyLayout(to: window)
            window.orderFrontRegardless()
        case (.panel, true):
            window?.orderOut(nil)
            playerView?.player = nil
            panelView?.attach(player)
        case (_, false):
            window?.orderOut(nil)
            panelView?.attach(nil)
        }
        updateVideoDecoding()
    }

    /// Stop decoding pictures nobody is looking at, without touching audio.
    ///
    /// This is the AVFoundation counterpart of mpv's `vid` property, which the
    /// M0 spike measured as audio-continuous. It matters more here: the panel
    /// surface is inside a menu bar popover that closes constantly, and a set
    /// left summoned would otherwise decode video into a view that is not on
    /// screen for as long as it plays.
    private func updateVideoDecoding() {
        let showing = snapshot().videoVisible && (surface == .window || panelOnScreen)
        guard let item = player?.currentItem else { return }
        for track in item.tracks where track.assetTrack?.mediaType == .video {
            track.isEnabled = showing
        }
    }

    private func applyLayout(to window: NSWindow) {
        let isFullScreen = window.styleMask.contains(.fullScreen)
        if case .fullscreen = layout {
            if !isFullScreen { window.toggleFullScreen(nil) }
            return
        }
        if isFullScreen { window.toggleFullScreen(nil) }
        guard let frame = layout.frame(aspect: videoAspect()) else { return }
        window.setFrame(frame, display: true)
    }

    /// The loaded video's aspect, or 16:9 before the first frame is decoded —
    /// the window follows the picture, so an audio file gets a sensible box.
    private func videoAspect() -> Double {
        let size = player?.currentItem?.presentationSize ?? .zero
        guard size.width > 0, size.height > 0 else { return 16.0 / 9.0 }
        return size.width / size.height
    }

    private static func time(_ seconds: Double) -> CMTime {
        CMTime(seconds: seconds, preferredTimescale: 600)
    }
}

/// Where the picture goes when video is switched on.
///
/// Only an engine living inside the app's own process can offer `.panel` —
/// mpv's window belongs to another process and cannot be drawn into a SwiftUI
/// view — which is why this is an AVFoundation-only setting rather than part
/// of the engine contract.
public enum VideoSurface: String {
    /// The engine's own floating window, placed by the layout setting.
    case window
    /// A view the app hosts itself, behind its own UI.
    case panel

    public static func parse(_ spec: String) -> VideoSurface {
        VideoSurface(rawValue: spec.trimmingCharacters(in: .whitespaces).lowercased()) ?? .window
    }
}

/// The view the app hosts to show video inside its own UI.
///
/// Engine-owned and handed out as-is: SwiftUI rebuilds its representables
/// freely, and building a fresh `AVPlayerLayer` per rebuild would tear the
/// picture down every time the panel redraws. It reports whether it is on
/// screen, which is what lets the engine stop decoding pictures nobody is
/// looking at once the menu bar panel closes.
public final class PanelVideoView: NSView {
    private let playerLayer = AVPlayerLayer()
    private let onScreenChanged: (Bool) -> Void

    init(onScreenChanged: @escaping (Bool) -> Void) {
        self.onScreenChanged = onScreenChanged
        super.init(frame: .zero)
        playerLayer.videoGravity = .resizeAspectFill
        playerLayer.backgroundColor = NSColor.black.cgColor
        // Layer-hosting, not layer-backed: the player layer *is* the backing
        // layer, so it follows the view's bounds with no sizing code.
        layer = playerLayer
        wantsLayer = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used from a nib") }

    func attach(_ player: AVPlayer?) {
        playerLayer.player = player
    }

    /// Whether the picture has actually been handed to this view, and whether
    /// a frame of it has arrived. Both exist for the regression test: the
    /// first summon after launch used to leave the layer with no player.
    var isAttached: Bool { playerLayer.player != nil }
    var isReadyForDisplay: Bool { playerLayer.isReadyForDisplay }

    public override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        onScreenChanged(window != nil)
    }
}
