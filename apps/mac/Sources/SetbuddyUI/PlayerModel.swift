import Foundation
import SwiftUI
import SetbuddyAV
import SetbuddyCore

/// The Rust core's handle. Aliased because this executable module is itself
/// named `Setbuddy`, so the bare name would resolve to the module.
typealias Core = SetbuddyCore.Setbuddy

/// Receives snapshots pushed from the Rust ticker thread and forwards them to
/// the main actor.
///
/// Separate from `PlayerModel` on purpose: the observer is called from a Rust
/// thread, so it cannot be main-actor isolated, and folding the two together
/// would mean lying about isolation somewhere.
final class SnapshotBridge: PlayerObserver, @unchecked Sendable {
    private let forward: @Sendable (PlayerSnapshot) -> Void

    init(forward: @escaping @Sendable (PlayerSnapshot) -> Void) {
        self.forward = forward
    }

    func onSnapshot(snapshot: PlayerSnapshot) {
        forward(snapshot)
    }
}

@MainActor
public final class PlayerModel: ObservableObject {
    @Published public private(set) var snapshot: PlayerSnapshot?
    @Published public private(set) var engineAvailable: Bool
    @Published public private(set) var lastError: String?
    @Published public private(set) var results: [Track] = []
    @Published public private(set) var recents: [Track] = []
    @Published public private(set) var queue: [Track] = []
    @Published public private(set) var folders: [String] = []
    @Published public private(set) var isScanning = false

    /// A folder being walked and probed on its way onto the stage. Held so the
    /// queue tab can say something is coming rather than looking empty.
    @Published public private(set) var isStaging = false
    @Published var searchQuery: String = "" {
        didSet { runSearch() }
    }

    /// While the scrubber is being dragged we ignore pushed positions, or the
    /// thumb fights the user's finger.
    @Published var isScrubbing = false
    @Published var scrubPosition: Double = 0

    /// Where a just-issued seek is heading, held until the engine's reported
    /// position catches up.
    ///
    /// Without this the thumb springs back on release: clearing `isScrubbing`
    /// hands the display straight back to the last pushed snapshot, which still
    /// carries the pre-seek position for a tick. The deadline is a safety valve
    /// so a seek that never lands cannot freeze the display.
    @Published private var pendingSeek: PendingSeek?

    /// Artwork for the current track: embedded cover art, or a frame from the
    /// video. Loaded off the main thread, nil until it arrives.
    @Published public private(set) var artwork: NSImage?
    private var artworkTrackId: Int64?

    /// Artwork for queue rows, by track id. Filled by `requestArtwork` as rows
    /// appear; a miss is recorded too, so a track with no art is asked once.
    @Published public private(set) var rowArtwork: [Int64: NSImage?] = [:]
    private var artworkInFlight: Set<Int64> = []

    /// What `ffprobe` said about the track the details panel last asked
    /// about, and which track that was — so the panel can tell "still
    /// loading" from "loaded, and there was nothing".
    @Published public private(set) var details: [MetadataEntry] = []
    @Published public private(set) var detailsTrackId: Int64?

    /// One lane for row artwork. First requests shell out to ffmpeg, and a
    /// freshly staged folder asks for every row at once — serial keeps that
    /// from fanning out into a process per track.
    private let artworkLane = DispatchQueue(label: "setbuddy.artwork", qos: .utility)

    /// Where and how large the pop-out is when it next appears, in the core's
    /// settings form: `"40%"` of the screen's width, `"fullscreen"`, a pixel
    /// width like `"1280"`, or width and top-left corner like `"1280+100+50"`.
    @Published public private(set) var videoWindowLayout: String = "40%"

    /// Listening mode: the expanded player. Owned here rather than in the view
    /// because with the panel as the video surface, expanding *is* summoning
    /// the set — there is no disc to press in there — so the state has a
    /// consequence beyond layout.
    @Published public private(set) var isExpanded = false

    /// A summon asked for but not yet reflected in a snapshot. Without it the
    /// 4 Hz ticker would ask again before the first request had landed.
    private var videoRequest: VideoRequest?

    /// The engine policy in force: `"auto"`, or an engine id that is always
    /// used. Read back from the core so settings show what is really set.
    @Published public private(set) var enginePolicy: String = "auto"

    /// What each registered engine can do, in preference order. Settings name
    /// engines from this, never from an id spelled out in a view.
    @Published public private(set) var engines: [EngineCapabilities] = []

    /// Where the picture goes when the set is summoned: the engine's own
    /// floating window, or the panel's own backdrop. Only an engine inside
    /// this process can draw into the panel, so the choice only bites while
    /// AVFoundation is the engine playing — see `panelVideoShowing`.
    @Published public private(set) var videoSurface: VideoSurface = .window

    /// The process owning the placement window while one is up. Nil otherwise.
    @Published public private(set) var placementPid: UInt32?
    private var placementOverlay: PlacementOverlay?

    /// Held here rather than read back from the engine: mpv reports its own
    /// volume, but an adopted session may have been left at any level, and the
    /// slider should reflect what this app last asked for.
    @Published var volume: Double = 100 {
        didSet { setVolume(volume) }
    }

    private var core: Core?
    private var bridge: SnapshotBridge?
    /// Held, not just registered: the panel surface is a Swift-to-Swift
    /// arrangement with this engine that the core knows nothing about.
    private let av = AVFoundationEngine()
    private let nowPlaying = NowPlayingBridge()

    public init() {
        engineAvailable = mpvAvailable()
        guard engineAvailable else { return }
        do {
            // AVFoundation first: registration order is preference order, so
            // it takes the containers it decodes natively and mpv keeps the
            // rest (webm, mkv, opus, flac, and anything unusual).
            let core = try Core.withEngines(engines: [av])
            self.core = core

            let bridge = SnapshotBridge { [weak self] snapshot in
                Task { @MainActor in self?.apply(snapshot) }
            }
            core.addObserver(observer: bridge)
            // 4 Hz: fast enough for a smooth scrubber, slow enough to stay idle.
            core.startTicker(intervalMs: 250)
            self.bridge = bridge

            nowPlaying.connect(
                togglePlayPause: { [weak self] in self?.togglePlayPause() },
                next: { [weak self] in self?.next() },
                previous: { [weak self] in self?.previous() },
                seek: { [weak self] position in self?.seek(to: position) }
            )
            reloadLibrary()
            videoWindowLayout = (try? core.videoWindowLayout()) ?? videoWindowLayout
            videoSurface = VideoSurface.parse((try? core.videoSurface()) ?? "window")
            av.setVideoSurface(videoSurface)
            enginePolicy = core.enginePolicy()
            engines = core.engines()
        } catch {
            lastError = describe(error)
        }
    }

    // MARK: - State

    public var isPlaying: Bool {
        guard let snapshot else { return false }
        return !snapshot.paused && !snapshot.idle
    }

    public var hasTrack: Bool { snapshot?.track != nil }

    /// Position to draw: the drag value while scrubbing, the seek target until
    /// the engine confirms it, and otherwise whatever the engine reports.
    var displayPosition: Double {
        if isScrubbing { return scrubPosition }
        if let pendingSeek { return pendingSeek.target }
        return snapshot?.positionSecs ?? 0
    }

    var duration: Double { snapshot?.durationSecs ?? 0 }

    private func apply(_ snapshot: PlayerSnapshot) {
        self.snapshot = snapshot

        // A seek is complete once the engine reports a position near the target
        // — or once we have waited long enough that something clearly went
        // wrong, in which case the engine's own position is the better answer.
        if let pendingSeek {
            let reported = snapshot.positionSecs ?? 0
            if abs(reported - pendingSeek.target) < 1.0 || Date() > pendingSeek.deadline {
                self.pendingSeek = nil
            }
        }

        // A request is done once the engine agrees, or once waiting for it has
        // clearly failed — after which the engine's own state is the truth.
        if let videoRequest, snapshot.videoVisible == videoRequest.want || Date() > videoRequest.deadline {
            self.videoRequest = nil
        }
        syncPanelVideo()

        refreshArtwork(for: snapshot)
        nowPlaying.update(with: snapshot)
    }

    /// Load artwork when the track changes, never on every tick.
    private func refreshArtwork(for snapshot: PlayerSnapshot) {
        guard let track = snapshot.track else {
            artworkTrackId = nil
            artwork = nil
            nowPlaying.setArtwork(nil)
            return
        }
        guard artworkTrackId != track.id else { return }

        artworkTrackId = track.id
        artwork = nil
        nowPlaying.setArtwork(nil)
        guard let core else { return }

        let id = track.id
        Task.detached(priority: .utility) {
            // Extraction shells out to ffmpeg, so it must not run on the main
            // thread. The core caches the result, so this is once per file.
            let path = (try? core.artworkPath(trackId: id)) ?? nil
            let image = path.flatMap { NSImage(contentsOfFile: $0) }
            await MainActor.run {
                // The track may have changed while the frame was being grabbed.
                guard self.artworkTrackId == id else { return }
                self.artwork = image
                self.nowPlaying.setArtwork(image)
            }
        }
    }

    /// Fetch a row's artwork if it is not already known or on its way.
    func requestArtwork(for track: Track) {
        let id = track.id
        guard let core, rowArtwork[id] == nil, !artworkInFlight.contains(id) else { return }
        artworkInFlight.insert(id)
        artworkLane.async {
            let path = (try? core.artworkPath(trackId: id)) ?? nil
            let image = path.flatMap { NSImage(contentsOfFile: $0) }
            Task { @MainActor in
                self.artworkInFlight.remove(id)
                self.rowArtwork[id] = .some(image)
            }
        }
    }

    /// Fetch the full metadata for a track. Shells out to ffprobe, so it runs
    /// off the main thread; a second request for the same track is free.
    func loadDetails(for track: Track) {
        guard let core, detailsTrackId != track.id else { return }
        let id = track.id
        Task.detached(priority: .userInitiated) {
            let entries = (try? core.trackDetails(trackId: id)) ?? []
            await MainActor.run {
                self.details = entries
                self.detailsTrackId = id
            }
        }
    }

    // MARK: - Transport

    public func togglePlayPause() {
        perform { core in _ = try core.togglePaused() }
    }

    public func next() {
        perform { core in _ = try core.next() }
        reloadQueue()
    }

    public func previous() {
        perform { core in _ = try core.previous() }
        reloadQueue()
    }

    public func seek(to seconds: Double) {
        beginSeek(to: seconds)
        perform { core in try core.seekAbsolute(seconds: seconds) }
    }

    public func skip(_ delta: Double) {
        guard let core else { return }
        do {
            // The core clamps to the file, so the returned value — not the
            // requested delta — is where playback is actually heading.
            let target = try core.seekRelative(deltaSecs: delta)
            beginSeek(to: target)
            lastError = nil
        } catch {
            lastError = describe(error)
        }
    }

    /// Show `target` until the engine confirms it.
    private func beginSeek(to target: Double) {
        pendingSeek = PendingSeek(target: target, deadline: Date().addingTimeInterval(2))
    }

    func setVolume(_ percent: Double) {
        perform { core in try core.setVolume(percent: percent) }
    }

    /// The pop-out: shows or hides the floating video window. Audio is
    /// unaffected either way.
    public func toggleVideo() {
        perform { core in _ = try core.toggleVideo() }
    }

    /// Choose where and how large the pop-out is. Size and position land on
    /// its next appearance; fullscreen applies to a window already showing.
    public func setVideoWindowLayout(_ spec: String) {
        guard let core else { return }
        do {
            try core.setVideoWindowLayout(spec: spec)
            videoWindowLayout = try core.videoWindowLayout()
            lastError = nil
        } catch {
            lastError = describe(error)
        }
    }

    /// Whether the panel could show video at all — that is, whether an engine
    /// that can draw into this process is registered. False on a build or a
    /// machine where only mpv is available.
    public var panelVideoSupported: Bool {
        engineIds.contains(av.capabilities().id)
    }

    /// Whether the set *would* play inside the panel for what is playing now:
    /// the panel is where it was asked to go, the file has a picture, and the
    /// engine holding it is the one that can draw here. A webm on mpv pops out
    /// to its own window however this is set — mpv has no surface to hand over.
    public var panelVideoPossible: Bool {
        videoSurface == .panel
            && snapshot?.hasVideo == true
            && snapshot?.engineId == av.capabilities().id
    }

    /// Whether the panel is drawing video right now.
    public var panelVideoShowing: Bool {
        panelVideoPossible && snapshot?.videoVisible == true
    }

    /// What a key does in the expanded player.
    ///
    /// The mapping lives here rather than in the view so it can be tested
    /// without a window, and so every surface that grows a keyboard answers
    /// the same keys. Everything goes through the core, which is what keeps
    /// the menu bar's idea of the set and the engine's own state one thing.
    public enum PlayerKey: Equatable {
        case playPause
        case skipBack
        case skipForward
        case next
        case previous
        case restart
        case volumeUp
        case volumeDown
        case collapse

        public static func from(_ key: KeyEquivalent, modifiers: EventModifiers = []) -> PlayerKey? {
            switch key {
            case .space: return .playPause
            case .leftArrow: return modifiers.contains(.command) ? .previous : .skipBack
            case .rightArrow: return modifiers.contains(.command) ? .next : .skipForward
            case .upArrow: return .volumeUp
            case .downArrow: return .volumeDown
            case .escape: return .collapse
            case KeyEquivalent("k"): return .playPause
            case KeyEquivalent("j"): return .skipBack
            case KeyEquivalent("l"): return .skipForward
            case KeyEquivalent("r"): return .restart
            case KeyEquivalent("n"): return .next
            case KeyEquivalent("p"): return .previous
            default: return nil
            }
        }
    }

    /// Steps match the buttons the keyboard replaces, so the two agree.
    public func perform(_ key: PlayerKey) {
        switch key {
        case .playPause: togglePlayPause()
        case .skipBack: skip(-30)
        case .skipForward: skip(30)
        case .next: next()
        case .previous: previous()
        case .restart: seek(to: 0)
        case .volumeUp: volume = min(volume + 5, 100)
        case .volumeDown: volume = max(volume - 5, 0)
        case .collapse: setExpanded(false)
        }
    }

    /// Expand or collapse the player. With the panel as the surface this also
    /// summons the set or puts it away.
    public func setExpanded(_ expanded: Bool) {
        guard isExpanded != expanded else { return }
        isExpanded = expanded
        syncPanelVideo()
    }

    /// Keep the picture in step with the expanded player: summoned while it is
    /// open, away while it is not. Runs on every snapshot too, so a track that
    /// changes under an open player brings its own video up.
    private func syncPanelVideo() {
        guard panelVideoPossible, videoRequest == nil else { return }
        guard (snapshot?.videoVisible == true) != isExpanded else { return }
        videoRequest = VideoRequest(want: isExpanded, deadline: Date().addingTimeInterval(2))
        toggleVideo()
    }

    /// The engine's own view, for the panel to host. Stable across redraws.
    public func panelVideoView() -> NSView {
        av.panelVideoView()
    }

    /// Choose between the floating window and the panel's backdrop. Persisted
    /// like the layout is, and applied to the engine at once.
    public func setVideoSurface(_ surface: VideoSurface) {
        guard let core else { return }
        do {
            try core.setVideoSurface(surface: surface.rawValue)
            videoSurface = surface
            av.setVideoSurface(surface)
            lastError = nil
        } catch {
            lastError = describe(error)
        }
    }

    /// Put up an empty video window at the current layout for the user to
    /// drag and resize. `saveWindowPlacement` reads it back; either that or
    /// `cancelWindowPlacement` takes it down.
    public func beginWindowPlacement() {
        guard let core, placementPid == nil else { return }
        do {
            guard let pid = try core.beginWindowPlacement() else {
                lastError = "No engine can show a window to place."
                return
            }
            placementPid = pid
            lastError = nil
            placementOverlay = PlacementOverlay(
                pid: pid_t(pid),
                onSave: { [weak self] in self?.saveWindowPlacement() },
                onCancel: { [weak self] in self?.cancelWindowPlacement() }
            )
        } catch {
            lastError = describe(error)
        }
    }

    /// Read the placement window's size, position and screen into the layout.
    public func saveWindowPlacement() {
        guard let pid = placementPid else { return }
        guard let spec = WindowPlacement.layoutSpec(forWindowOf: pid_t(pid)) else {
            lastError = "The placement window could not be found on screen."
            return
        }
        setVideoWindowLayout(spec)
        cancelWindowPlacement()
    }

    public func cancelWindowPlacement() {
        guard placementPid != nil else { return }
        placementPid = nil
        placementOverlay?.close()
        placementOverlay = nil
        perform { core in try core.endWindowPlacement() }
    }

    func setRepeat(_ mode: RepeatMode) {
        perform { core in try core.setRepeat(mode: mode) }
    }

    /// Queue positions playback is confined to; empty when it walks the whole
    /// queue.
    var loopPositions: Set<Int> {
        Set((snapshot?.loopPositions ?? []).map(Int.init))
    }

    /// Scramble the tracks at `positions` among themselves — every row when
    /// `positions` is empty. Visible and persisted: the order shown is the
    /// order that plays.
    func scramble(_ positions: [Int]) {
        perform { core in try core.queueScramble(positions: positions.map(UInt32.init)) }
        reloadQueue()
    }

    /// Loop playback over `positions` until `clearLoop`. Replaces repeat.
    func setLoop(_ positions: [Int]) {
        guard !positions.isEmpty else { return clearLoop() }
        perform { core in try core.setLoop(positions: positions.map(UInt32.init)) }
    }

    func clearLoop() {
        perform { core in try core.setLoop(positions: []) }
    }

    // MARK: - Library

    public func play(_ track: Track) {
        perform { core in _ = try core.playTrack(trackId: track.id) }
        reloadQueue()
    }

    /// Play from the top, ignoring the saved position.
    public func restart(_ track: Track) {
        perform { core in _ = try core.restartTrack(trackId: track.id) }
        reloadQueue()
    }

    /// Index in `queue` of the track that is playing.
    var queueIndex: Int? { snapshot?.queueIndex.map(Int.init) }

    /// Put a file, or every track under a folder, on the stage — the top of the
    /// queue — without starting playback.
    ///
    /// Off the main thread: staging a folder scans and probes it, which for a
    /// directory of long sets is not instant.
    public func stage(_ url: URL) {
        guard let core, !isStaging else { return }
        isStaging = true
        Task.detached(priority: .userInitiated) {
            let outcome = Result { try core.stagePath(path: url.path) }
            await MainActor.run {
                self.isStaging = false
                switch outcome {
                case .success:
                    // Folder staging indexes new files, so the whole library
                    // view is refreshed, not just the queue.
                    self.reloadLibrary()
                case .failure(let error):
                    self.lastError = describe(error)
                }
            }
        }
    }

    /// Play from the top of the stage.
    public func playStage() {
        guard let first = queue.first else { return }
        play(first)
    }

    /// Reorder the queue. Indices are into `queue`.
    func moveQueueItem(from: Int, to: Int) {
        guard from != to, queue.indices.contains(from), queue.indices.contains(to) else { return }
        perform { core in _ = try core.queueMove(from: UInt32(from), to: UInt32(to)) }
        reloadQueue()
    }

    func enqueue(_ track: Track) {
        perform { core in try core.queueAddTrack(trackId: track.id) }
        reloadQueue()
    }

    func clearQueue() {
        perform { core in try core.queueClear() }
        reloadQueue()
    }

    public func openFile(at url: URL) {
        perform { core in _ = try core.playPath(path: url.path) }
        reloadLibrary()
    }

    func addFolder(_ url: URL) {
        perform { core in try core.addFolder(path: url.path) }
        reloadFolders()
        scan()
    }

    public func removeFolder(_ path: String) {
        perform { core in _ = try core.removeFolder(path: path) }
        reloadFolders()
    }

    /// Rescan watched folders off the main thread — this walks the disk.
    /// Engines available, in preference order: AVFoundation, then mpv.
    public var engineIds: [String] { engines.map(\.id) }

    /// `"auto"` picks the first engine that can open each file; an engine id
    /// forces that engine and reports unsupported files rather than falling back.
    public func setEnginePolicy(_ policy: String) {
        perform { core in try core.setEnginePolicy(policy: policy) }
        enginePolicy = core?.enginePolicy() ?? policy
    }

    /// Engines that run in their own process. They are the ones that can put
    /// up a window for the user to place by hand, because the host reads that
    /// window back by owning pid.
    public var outOfProcessEngines: [EngineCapabilities] {
        engines.filter { $0.id != av.capabilities().id }
    }

    /// Whether the current policy can still reach an engine of its own — a
    /// forced in-process engine can never open a window for placement.
    public var windowPlacementPossible: Bool {
        enginePolicy == "auto" || outOfProcessEngines.contains { $0.id == enginePolicy }
    }

    /// Whether the video window settings can affect anything: either the set
    /// itself is set to open in a window, or a file that only an out-of-process
    /// engine can play would still open one.
    public var videoWindowSettingsApply: Bool {
        videoSurface == .window || windowPlacementPossible
    }

    public func removeWatchedFolder(_ path: String) {
        removeFolder(path)
    }

    public func scan() {
        guard let core, !isScanning else { return }
        isScanning = true
        Task.detached(priority: .utility) {
            let outcome = Result { try core.scan() }
            await MainActor.run {
                self.isScanning = false
                switch outcome {
                case .success:
                    self.reloadLibrary()
                case .failure(let error):
                    self.lastError = describe(error)
                }
            }
        }
    }

    public func reloadLibrary() {
        reloadFolders()
        reloadRecents()
        reloadQueue()
        runSearch()
    }

    private func reloadFolders() {
        folders = fetch { try $0.watchedFolders() }
    }

    private func reloadRecents() {
        recents = fetch { try $0.recents(limit: 12) }
    }

    public func reloadQueue() {
        queue = fetch { try $0.queueTracks() }
    }

    private func runSearch() {
        let query = searchQuery.trimmingCharacters(in: .whitespaces)
        guard !query.isEmpty else {
            results = []
            return
        }
        results = fetch { try $0.search(query: query, limit: 25) }
    }

    /// Reads that are safe to come back empty — a failed library query should
    /// leave the list blank, not raise a banner over the transport controls.
    private func fetch<T>(_ body: (Core) throws -> [T]) -> [T] {
        guard let core else { return [] }
        return (try? body(core)) ?? []
    }

    // MARK: - Lifecycle

    /// Save state and stop the engines, leaving the process running.
    ///
    /// Split from `quit` so tests can shut a model down without taking the test
    /// runner with it.
    public func shutdown() {
        cancelWindowPlacement()
        try? core?.quit()
        nowPlaying.clear()
    }

    public func quit() {
        shutdown()
        NSApplication.shared.terminate(nil)
    }

    func dismissError() {
        lastError = nil
    }

    private func perform(_ body: (Core) throws -> Void) {
        guard let core else { return }
        do {
            try body(core)
            lastError = nil
        } catch {
            lastError = describe(error)
        }
    }
}

/// A summon that has been issued but not yet confirmed by the engine.
private struct VideoRequest {
    let want: Bool
    let deadline: Date
}

/// A seek that has been issued but not yet confirmed by the engine.
private struct PendingSeek {
    let target: Double
    let deadline: Date
}

/// Prefer the typed message the core provides over Swift's generic wrapping.
func describe(_ error: Error) -> String {
    if let error = error as? SetbuddyError {
        return error.localizedDescription
    }
    return error.localizedDescription
}
