import SwiftUI
import SetbuddyAV
import SetbuddyCore

public struct MenuBarView: View {
    @EnvironmentObject private var model: PlayerModel

    /// Which list is showing. Owned here rather than in the panel because
    /// staging something has to be able to switch to the queue.
    @State private var tab: BrowserTab = .recent


    public init() {}

    public var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if !model.engineAvailable {
                OnboardingView()
            } else if model.isExpanded {
                ListeningView { withAnimation(collapse) { model.setExpanded(false) } }
            } else {
                NowPlayingHeader(onExpand: { withAnimation(expand) { model.setExpanded(true) } })
                TransportBar()
                Divider()
                BrowserPanel(tab: $tab)
                Divider()
                FooterBar(onAdd: stage)
            }
        }
        .frame(width: 380)
        .overlay(alignment: .top) { ErrorBanner() }
        // A track that ends while expanded leaves nothing to look at, so the
        // panel falls back to the list rather than a blank square.
        .onChange(of: model.hasTrack) { _, hasTrack in
            if !hasTrack { withAnimation(collapse) { model.setExpanded(false) } }
        }
    }

    /// Adding music always lands on the stage and always shows it: the queue is
    /// where the newly added tracks can be arranged before anything plays.
    private func stage(_ url: URL) {
        model.stage(url)
        withAnimation(.easeOut(duration: 0.2)) { tab = .queue }
    }

    private var expand: Animation { .spring(response: 0.42, dampingFraction: 0.82) }
    private var collapse: Animation { .spring(response: 0.34, dampingFraction: 0.9) }
}

// MARK: - Now playing

private struct NowPlayingHeader: View {
    @EnvironmentObject private var model: PlayerModel
    let onExpand: () -> Void
    @State private var showDetails = false
    @State private var hoveringTitle = false

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            ArtworkThumbnail(onExpand: onExpand)
            VStack(alignment: .leading, spacing: 6) {
                if let track = model.snapshot?.track {
                    VStack(alignment: .leading, spacing: 1) {
                        // The title is the way into the details panel. It
                        // underlines on hover, the way a link does, because
                        // nothing else about a headline says "click me".
                        Text(track.title ?? track.displayLabel)
                            .font(.headline)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .underline(hoveringTitle, color: .secondary)
                            .contentShape(Rectangle())
                            .onHover { hoveringTitle = $0 }
                            .onTapGesture { showDetails = true }
                            .help("Show everything known about this track")
                            .popover(isPresented: $showDetails, arrowEdge: .bottom) {
                                TrackDetailsView(track: track).environmentObject(model)
                            }
                        if let artist = track.artist {
                            Text(artist)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                        }
                    }
                    scrubber
                } else {
                    Text("Nothing playing")
                        .font(.headline)
                        .foregroundStyle(.secondary)
                    Text("Open a file, or pick something from your library below.")
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .padding(.horizontal, 14)
        .padding(.top, 12)
        .padding(.bottom, 8)
    }

    private var scrubber: some View {
        VStack(spacing: 2) {
            Slider(
                value: Binding(
                    get: { model.displayPosition },
                    set: { model.scrubPosition = $0 }
                ),
                in: 0...max(model.duration, 1),
                onEditingChanged: { editing in
                    if editing {
                        model.scrubPosition = model.displayPosition
                        model.isScrubbing = true
                    } else {
                        // Order matters: `seek` records where playback is
                        // heading, and only then is the drag state released.
                        // Clearing it first would hand the display back to the
                        // last pushed snapshot for one frame, which is exactly
                        // the backwards jump this avoids.
                        model.seek(to: model.scrubPosition)
                        model.isScrubbing = false
                    }
                }
            )
            .controlSize(.small)
            .disabled(model.duration <= 0)

            HStack {
                Text(formatDuration(seconds: model.displayPosition))
                Spacer()
                Text(formatDuration(seconds: model.duration))
            }
            .font(.caption2.monospacedDigit())
            .foregroundStyle(.secondary)
        }
    }
}

/// Cover art, or a frame from the video. Falls back to an icon while the
/// thumbnail is being extracted, or when there is none to be had.
///
/// Clicking it expands into listening mode, which is why it carries a hover
/// hint: without one there is nothing to say the square is a control.
private struct ArtworkThumbnail: View {
    @EnvironmentObject private var model: PlayerModel
    let onExpand: () -> Void
    @State private var hovering = false
    @State private var discAngle: Double = 0

    var body: some View {
        Group {
            if let image = model.artwork {
                Image(nsImage: image)
                    .resizable()
                    .aspectRatio(contentMode: .fill)
            } else {
                ZStack {
                    Rectangle().fill(.quaternary)
                    Image(systemName: placeholderSymbol)
                        .font(.title3)
                        .foregroundStyle(.tertiary)
                }
            }
        }
        .frame(width: 52, height: 52)
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .overlay {
            if model.hasTrack && hovering {
                ZStack {
                    RoundedRectangle(cornerRadius: 6).fill(.black.opacity(0.45))
                    // The record, turning: the same glyph the expanded player
                    // uses for the set, so the way in and the thing behind it
                    // are visibly the same object.
                    Image(systemName: "opticaldisc.fill")
                        .font(.title2.weight(.light))
                        .foregroundStyle(.white.opacity(0.95))
                        .rotationEffect(.degrees(discAngle))
                }
                .transition(.opacity)
            }
        }
        .overlay(
            RoundedRectangle(cornerRadius: 6)
                .strokeBorder(.separator, lineWidth: 0.5)
        )
        .contentShape(RoundedRectangle(cornerRadius: 6))
        .onTapGesture { if model.hasTrack { onExpand() } }
        .onHover { hovering in
            self.hovering = hovering
            // Turning only under the pointer: at rest the header is still.
            if hovering {
                withAnimation(.linear(duration: 4).repeatForever(autoreverses: false)) {
                    discAngle = 360
                }
            } else {
                withAnimation(.easeOut(duration: 0.3)) { discAngle = 0 }
            }
        }
        .help(model.hasTrack ? (model.panelVideoPossible ? "Play the set" : "Listening mode") : "")
        .animation(.easeOut(duration: 0.18), value: model.artwork)
        .animation(.easeOut(duration: 0.12), value: hovering)
    }

    private var placeholderSymbol: String {
        guard let track = model.snapshot?.track else { return "waveform" }
        return track.hasVideo ? "film" : "music.note"
    }
}

// MARK: - Buttons

/// The panel's buttons are bare glyphs, so hovering one has to say it is a
/// button: a soft pad appears behind it, darkens on press, and a disabled one
/// is dimmed rather than left looking pressable.
struct HoverButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        HoverButtonBody(configuration: configuration)
    }
}

private struct HoverButtonBody: View {
    let configuration: ButtonStyle.Configuration
    @Environment(\.isEnabled) private var isEnabled
    @State private var hovering = false

    var body: some View {
        configuration.label
            .padding(.horizontal, 5)
            .padding(.vertical, 4)
            .background(
                RoundedRectangle(cornerRadius: 6)
                    .fill(Color.primary.opacity(padOpacity))
            )
            .contentShape(RoundedRectangle(cornerRadius: 6))
            .opacity(isEnabled ? 1 : 0.35)
            .scaleEffect(configuration.isPressed ? 0.94 : 1)
            .onHover { hovering = $0 }
            .animation(.easeOut(duration: 0.12), value: hovering)
            .animation(.easeOut(duration: 0.08), value: configuration.isPressed)
    }

    private var padOpacity: Double {
        guard isEnabled else { return 0 }
        if configuration.isPressed { return 0.16 }
        return hovering ? 0.09 : 0
    }
}

// MARK: - Transport

private struct TransportBar: View {
    @EnvironmentObject private var model: PlayerModel

    var body: some View {
        HStack(spacing: 4) {
            Button { model.previous() } label: {
                Image(systemName: "backward.fill")
            }
            .help("Previous track")

            Button { model.skip(-30) } label: {
                Image(systemName: "gobackward.30")
            }
            .help("Back 30 seconds")

            Button { model.togglePlayPause() } label: {
                Image(systemName: model.isPlaying ? "pause.fill" : "play.fill")
                    .font(.title2)
            }
            .help(model.isPlaying ? "Pause" : "Play")
            .disabled(!model.hasTrack)

            Button { model.skip(30) } label: {
                Image(systemName: "goforward.30")
            }
            .help("Forward 30 seconds")

            Button { model.next() } label: {
                Image(systemName: "forward.fill")
            }
            .help("Next track")

            Spacer()

            volumeControl
        }
        .buttonStyle(HoverButtonStyle())
        .padding(.horizontal, 9)
        .padding(.bottom, 10)
    }

    private var volumeControl: some View {
        HStack(spacing: 4) {
            Image(systemName: "speaker.wave.2")
                .font(.caption)
                .foregroundStyle(.secondary)
            Slider(value: $model.volume, in: 0...100)
                .controlSize(.mini)
                .frame(width: 70)
        }
    }
}

// MARK: - Library

/// The two lists the panel can show. Recent is the default; the queue is what
/// staging switches to.
private enum BrowserTab: Hashable {
    case recent
    case queue

    var title: String {
        switch self {
        case .recent: "Recent"
        case .queue: "Queue"
        }
    }
}

private struct BrowserPanel: View {
    @EnvironmentObject private var model: PlayerModel
    @Binding var tab: BrowserTab

    /// Search replaces the tab bar rather than sitting under it: at 380pt wide
    /// there is room for the tabs or a text field, not both.
    @State private var searching = false
    @FocusState private var searchFocused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if searching {
                searchField
            } else {
                tabBar
            }
            content
        }
        .padding(.top, 10)
    }

    private var tabBar: some View {
        HStack(spacing: 6) {
            tabButton(.recent)
            tabButton(.queue)
            Spacer()
            Button {
                searching = true
                searchFocused = true
            } label: {
                Image(systemName: "magnifyingglass")
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(HoverButtonStyle())
            .help("Search your library")
        }
        .padding(.leading, 14)
        .padding(.trailing, 9)
    }

    private func tabButton(_ target: BrowserTab) -> some View {
        TabButton(
            title: target.title,
            count: target == .queue ? model.queue.count : 0,
            selected: tab == target
        ) { tab = target }
    }

    private var searchField: some View {
        HStack(spacing: 6) {
            Image(systemName: "magnifyingglass")
                .foregroundStyle(.secondary)
            TextField("Search your library", text: $model.searchQuery)
                .textFieldStyle(.plain)
                .focused($searchFocused)
            Button {
                model.searchQuery = ""
                searching = false
            } label: {
                Image(systemName: "xmark.circle.fill")
            }
            .buttonStyle(HoverButtonStyle())
            .foregroundStyle(.tertiary)
            .help("Close search")
        }
        .padding(.leading, 14)
        .padding(.trailing, 9)
    }

    @ViewBuilder private var content: some View {
        if searching {
            VStack(alignment: .leading, spacing: 6) {
                if model.searchQuery.trimmingCharacters(in: .whitespaces).isEmpty {
                    hint("Type to search across every indexed track.")
                } else {
                    sectionLabel("Results")
                    trackList(model.results, empty: "Nothing matched.")
                }
            }
        } else {
            switch tab {
            case .recent:
                trackList(model.recents, empty: recentEmptyMessage)
            case .queue:
                QueuePanel()
            }
        }
    }

    /// The empty hint sits outside the scroll view so an empty list measures
    /// as no rows at all, which is what hides the resize handle.
    @ViewBuilder private func trackList(_ tracks: [Track], empty: String) -> some View {
        if tracks.isEmpty {
            hint(empty)
        } else {
            FittedScrollView {
                ForEach(tracks, id: \.id) { track in
                    TrackRow(track: track)
                }
            }
        }
    }

    private var recentEmptyMessage: String {
        model.folders.isEmpty ? "Add a folder below to build your library." : "Nothing played yet."
    }

    private func hint(_ text: String) -> some View {
        Text(text)
            .font(.caption)
            .foregroundStyle(.tertiary)
            .padding(.horizontal, 14)
            .padding(.vertical, 8)
            .fixedSize(horizontal: false, vertical: true)
    }

    private func sectionLabel(_ text: String) -> some View {
        Text(text.uppercased())
            .font(.caption2.weight(.semibold))
            .foregroundStyle(.tertiary)
            .padding(.horizontal, 14)
    }
}

/// One tab in the browser's bar. Carries its own capsule rather than the shared
/// hover pad, so the selected tab and a hovered one read as the same shape at
/// two strengths.
private struct TabButton: View {
    let title: String
    let count: Int
    let selected: Bool
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 4) {
                Text(title)
                if count > 0 {
                    Text("\(count)")
                        .font(.caption2.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
            }
            .font(.caption.weight(.semibold))
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .background(Capsule().fill(Color.primary.opacity(fillOpacity)))
            .foregroundStyle(selected ? Color.primary : .secondary)
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .animation(.easeOut(duration: 0.12), value: hovering)
    }

    private var fillOpacity: Double {
        if selected { return 0.1 }
        return hovering ? 0.06 : 0
    }
}

/// The stage: what will play, in the order it will play, plus the controls that
/// change that order.
///
/// Rows select like Finder rows — click, ⌘-click, ⇧-click, double-click to
/// play — and the order controls act on the selection when there is one and
/// on the whole queue when there is not. Shuffle and repeat live here rather
/// than in the footer because they are answers to "in what order does this
/// queue play": meaningless until there is a queue, which is why they are not
/// drawn at all for an empty one.
private struct QueuePanel: View {
    @EnvironmentObject private var model: PlayerModel

    /// Selected rows, by position. Positions rather than ids because the same
    /// track can be queued twice and the user chose rows.
    @State private var selection: Set<Int> = []
    /// The row a ⇧-click ranges from: the last plain click.
    @State private var anchor: Int?

    private var isEmpty: Bool { model.queue.isEmpty }
    private var looping: Bool { !model.loopPositions.isEmpty }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if isEmpty {
                Text(model.isStaging
                     ? "Staging…"
                     : "Nothing staged. Add a file or folder below, then arrange it here.")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 8)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                orderBar
                FittedScrollView {
                    // Keyed by position, not track id: the same track can sit
                    // in the queue twice, and duplicate ids make ForEach drop
                    // rows on the floor.
                    ForEach(Array(model.queue.enumerated()), id: \.offset) { index, track in
                        QueueRow(
                            track: track,
                            index: index,
                            count: model.queue.count,
                            selected: selection.contains(index),
                            looped: model.loopPositions.contains(index),
                            onClick: { select(index, modifiers: $0) },
                            onDoubleClick: { model.play(track) },
                            onMove: { move(from: index, to: $0) }
                        )
                    }
                }
            }
        }
        // Rows shifted under the selection — a stage landed on top, or the
        // queue was cleared — so the positions it holds no longer mean what
        // the user picked.
        .onChange(of: model.queue.count) { _, _ in
            selection.removeAll()
            anchor = nil
        }
    }

    private var orderBar: some View {
        HStack(spacing: 2) {
            Button { play() } label: {
                Image(systemName: "play.fill")
            }
            .help(selection.isEmpty ? "Play from the top of the stage" : "Play the first selected row")

            Button { model.scramble(selection.sorted()) } label: {
                Image(systemName: "shuffle")
            }
            .help(selection.isEmpty ? "Shuffle the queue" : "Shuffle the \(selection.count) selected rows among themselves")

            Button { toggleRepeat() } label: {
                Image(systemName: repeatIcon)
                    .foregroundStyle(repeatActive ? Color.accentColor : .secondary)
            }
            .help(repeatHelp)

            Spacer()

            if model.isStaging {
                ProgressView().controlSize(.mini)
            }

            if !selection.isEmpty {
                Button {
                    selection.removeAll()
                    anchor = nil
                } label: {
                    HStack(spacing: 3) {
                        Text("\(selection.count) selected")
                        Image(systemName: "xmark.circle.fill")
                    }
                }
                .font(.caption)
                .foregroundStyle(.secondary)
                .help("Deselect")
            }

            Button("Clear") { model.clearQueue() }
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .buttonStyle(HoverButtonStyle())
        .font(.callout)
        .padding(.horizontal, 9)
        .padding(.vertical, 6)
    }

    // MARK: Selection

    /// Finder rules: ⌘ toggles, ⇧ ranges from the anchor, a plain click
    /// selects just this row.
    private func select(_ index: Int, modifiers flags: NSEvent.ModifierFlags) {
        if flags.contains(.command) {
            if selection.contains(index) {
                selection.remove(index)
            } else {
                selection.insert(index)
            }
            anchor = index
        } else if flags.contains(.shift), let anchor {
            selection.formUnion(min(anchor, index)...max(anchor, index))
        } else {
            selection = [index]
            anchor = index
        }
    }

    private func move(from: Int, to: Int) {
        model.moveQueueItem(from: from, to: to)
        // Keep the selection on the same rows through the move.
        selection = Set(selection.map { positionAfterMove($0, from: from, to: to) })
        anchor = anchor.map { positionAfterMove($0, from: from, to: to) }
    }

    /// Mirrors the core's bookkeeping for one move, so the selection follows
    /// its rows the way the current track and the loop do.
    private func positionAfterMove(_ position: Int, from: Int, to: Int) -> Int {
        if position == from { return to }
        var moved = position
        if moved > from { moved -= 1 }
        if moved >= to { moved += 1 }
        return moved
    }

    // MARK: Order functions

    private func play() {
        if let first = selection.min(), model.queue.indices.contains(first) {
            model.play(model.queue[first])
        } else {
            model.playStage()
        }
    }

    /// Looping rows and repeating the queue share the button: with a loop up,
    /// the press stops it; with rows selected, it loops them; otherwise it
    /// cycles repeat as it always has.
    private func toggleRepeat() {
        if looping {
            model.clearLoop()
        } else if !selection.isEmpty {
            model.setLoop(selection.sorted())
        } else {
            let current = model.snapshot?.repeat ?? .off
            let next: RepeatMode = switch current {
            case .off: .all
            case .all: .one
            case .one: .off
            }
            model.setRepeat(next)
        }
    }

    private var repeatActive: Bool {
        looping || model.snapshot?.repeat != RepeatMode.off
    }

    private var repeatIcon: String {
        model.snapshot?.repeat == RepeatMode.one ? "repeat.1" : "repeat"
    }

    private var repeatHelp: String {
        if looping { return "Stop looping the \(model.loopPositions.count) looped rows" }
        if !selection.isEmpty { return "Loop the \(selection.count) selected rows" }
        return "Repeat"
    }
}

private struct QueueRow: View {
    @EnvironmentObject private var model: PlayerModel
    let track: Track
    let index: Int
    let count: Int
    let selected: Bool
    let looped: Bool
    let onClick: (NSEvent.ModifierFlags) -> Void
    let onDoubleClick: () -> Void
    let onMove: (Int) -> Void
    @State private var hovering = false

    private var isPlaying: Bool { model.queueIndex == index }

    var body: some View {
        HStack(spacing: 8) {
            // A loop is a bracket down the left edge: the looped rows read as
            // one run even when they are not adjacent.
            RoundedRectangle(cornerRadius: 1)
                .fill(looped ? Color.accentColor : .clear)
                .frame(width: 3)
                .padding(.vertical, 2)

            Group {
                if isPlaying {
                    Image(systemName: "speaker.wave.2.fill").font(.caption2)
                } else {
                    Text("\(index + 1)").font(.caption2.monospacedDigit())
                }
            }
            .foregroundStyle(isPlaying ? AnyShapeStyle(Color.accentColor) : AnyShapeStyle(.tertiary))
            .frame(width: 16, alignment: .trailing)

            RowArtwork(track: track)

            Text(track.displayLabel)
                .font(.callout)
                .lineLimit(1)
                .truncationMode(.middle)
                .foregroundStyle(isPlaying || selected ? Color.primary : .secondary)

            Spacer()

            if hovering {
                // Up and down rather than drag-and-drop: one click per position
                // is precise in a list this short, and needs no drop targets.
                Button { onMove(index - 1) } label: {
                    Image(systemName: "arrow.up")
                }
                .buttonStyle(HoverButtonStyle())
                .disabled(index == 0)
                .help("Move up")

                Button { onMove(index + 1) } label: {
                    Image(systemName: "arrow.down")
                }
                .buttonStyle(HoverButtonStyle())
                .disabled(index == count - 1)
                .help("Move down")
            } else if let duration = track.durationSecs {
                Text(formatDuration(seconds: duration))
                    .font(.caption2.monospacedDigit())
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(.leading, 6)
        .padding(.trailing, 14)
        .padding(.vertical, 4)
        .background(rowBackground)
        .contentShape(Rectangle())
        // One handler, reading the click count off the event, rather than a
        // stacked double-tap gesture: that makes the single tap wait out the
        // double-click interval before it fires, which reads as lag on select.
        // Here the first click selects at once and the second plays.
        .onTapGesture {
            let event = NSApp.currentEvent
            if (event?.clickCount ?? 1) >= 2 {
                onDoubleClick()
            } else {
                onClick(event?.modifierFlags ?? [])
            }
        }
        .onHover { hovering = $0 }
    }

    private var rowBackground: Color {
        if selected { return Color.accentColor.opacity(0.18) }
        if hovering { return Color.primary.opacity(0.06) }
        return .clear
    }
}

/// A row's cover or frame. Video gets a 16:9 frame and audio a square, which
/// is what makes video rows sit a little taller.
private struct RowArtwork: View {
    @EnvironmentObject private var model: PlayerModel
    let track: Track

    private var size: CGSize {
        track.hasVideo ? CGSize(width: 78, height: 44) : CGSize(width: 36, height: 36)
    }

    var body: some View {
        Group {
            if let image = model.rowArtwork[track.id] ?? nil {
                Image(nsImage: image)
                    .resizable()
                    .aspectRatio(contentMode: .fill)
            } else {
                ZStack {
                    Rectangle().fill(.quaternary)
                    Image(systemName: track.hasVideo ? "film" : "music.note")
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                }
            }
        }
        .frame(width: size.width, height: size.height)
        .clipShape(RoundedRectangle(cornerRadius: 4))
        .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(.separator, lineWidth: 0.5))
        .onAppear { model.requestArtwork(for: track) }
    }
}

private struct TrackRow: View {
    @EnvironmentObject private var model: PlayerModel
    let track: Track
    @State private var hovering = false

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: track.hasVideo ? "film" : "music.note")
                .font(.caption)
                .foregroundStyle(.secondary)
                .frame(width: 14)

            VStack(alignment: .leading, spacing: 1) {
                Text(track.displayLabel)
                    .font(.callout)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if let resume = track.resumeSecs {
                    // Only shown when the saved position would actually be used.
                    Text("Resume \(formatDuration(seconds: resume))")
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                }
            }

            Spacer()

            if let duration = track.durationSecs {
                Text(formatDuration(seconds: duration))
                    .font(.caption2.monospacedDigit())
                    .foregroundStyle(.tertiary)
            }

            if hovering {
                Button { model.enqueue(track) } label: {
                    Image(systemName: "text.append")
                }
                .buttonStyle(HoverButtonStyle())
                .help("Add to queue")
                if track.resumeSecs != nil {
                    Button { model.restart(track) } label: {
                        Image(systemName: "arrow.counterclockwise")
                    }
                    .buttonStyle(HoverButtonStyle())
                    .help("Play from the start")
                }
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 5)
        .background(hovering ? Color.primary.opacity(0.06) : .clear)
        .contentShape(Rectangle())
        .onTapGesture { model.play(track) }
        .onHover { hovering = $0 }
    }
}

// MARK: - Lists

/// A list exactly as tall as its rows, up to a fixed ceiling, and scrolling
/// past it.
///
/// Not a measured scroll view. A `ScrollView` has no height of its own, and a
/// zero-height one never lays out its content, so measuring the rows to size
/// it reports zero and stays there. `ViewThatFits` decides in layout instead:
/// rows that fit under the ceiling are placed as they are, and rows that do
/// not go into a scroll view pinned to it. The rows are built twice, which is
/// why the stack is not lazy and why these lists stay short.
///
/// `fixedSize` is load-bearing. `frame(maxHeight:)` is flexible — under the
/// panel's proposal it grows to the ceiling however few rows there are — and
/// `fixedSize` is what makes it settle at the rows' own height instead.
private struct FittedScrollView<Content: View>: View {
    @ViewBuilder let content: Content

    /// The most a list may take before it scrolls. Past roughly this the panel
    /// starts running off a short screen. Computed because a generic type
    /// cannot hold a static stored property.
    private static var cap: Double { 300 }

    var body: some View {
        ViewThatFits(in: .vertical) {
            rows
            ScrollView { rows }
                .frame(height: Self.cap)
        }
        .frame(maxHeight: Self.cap)
        .fixedSize(horizontal: false, vertical: true)
    }

    private var rows: some View {
        VStack(alignment: .leading, spacing: 0) {
            content
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

// MARK: - Footer

private struct FooterBar: View {
    @EnvironmentObject private var model: PlayerModel
    let onAdd: (URL) -> Void
    @State private var showSettings = false

    var body: some View {
        HStack(spacing: 2) {
            Button { chooseMusic() } label: {
                Image(systemName: "plus.rectangle.on.folder")
            }
            .help("Stage a file or a folder")

            Button { showSettings.toggle() } label: {
                Image(systemName: "gearshape")
            }
            .help("Watched folders and engine")
            .popover(isPresented: $showSettings, arrowEdge: .bottom) {
                SettingsPanel().environmentObject(model)
            }

            Spacer()

            Button { model.quit() } label: {
                Image(systemName: "power")
            }
            .help("Quit Setbuddy")
        }
        .buttonStyle(HoverButtonStyle())
        .font(.callout)
        .padding(.horizontal, 9)
        .padding(.vertical, 9)
    }

    /// One picker for both, because staging treats them the same: a folder is
    /// just more tracks arriving at once.
    private func chooseMusic() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.prompt = "Stage"
        panel.message = "Choose a set, a track, or a folder to stage"
        if panel.runModal() == .OK, let url = panel.url {
            onAdd(url)
        }
    }
}

private struct ErrorBanner: View {
    @EnvironmentObject private var model: PlayerModel

    var body: some View {
        if let message = model.lastError {
            HStack(spacing: 8) {
                Image(systemName: "exclamationmark.triangle.fill")
                Text(message)
                    .font(.caption)
                    .lineLimit(2)
                Spacer()
                Button { model.dismissError() } label: {
                    Image(systemName: "xmark")
                }
                .buttonStyle(.plain)
            }
            .padding(8)
            .background(.red.opacity(0.85), in: RoundedRectangle(cornerRadius: 6))
            .foregroundStyle(.white)
            .padding(8)
            .transition(.move(edge: .top).combined(with: .opacity))
        }
    }
}

// MARK: - Listening mode

/// The artwork, full bleed, with the transport floating over it.
///
/// A view state only: playback, the queue and the video window are untouched by
/// entering or leaving it.
private struct ListeningView: View {
    @EnvironmentObject private var model: PlayerModel
    let onBack: () -> Void

    /// Whether this view is holding the keyboard. Only then are the on-screen
    /// controls redundant, so only then are they taken away — a set playing in
    /// a panel that cannot answer keys must still be pressable.
    @FocusState private var focused: Bool
    @State private var showHint = false

    private static let height: CGFloat = 420

    /// The set is playing here and the keys reach it: nothing on screen is
    /// carrying its weight any more.
    private var keyboardDriven: Bool { model.panelVideoShowing && focused }

    var body: some View {
        ZStack {
            backdrop
            // Art is arbitrary, so the chrome carries its own contrast rather
            // than trusting the image underneath it to be dark.
            LinearGradient(
                colors: [.black.opacity(0.55), .black.opacity(0.15), .black.opacity(0.75)],
                startPoint: .top,
                endPoint: .bottom
            )

            VStack(spacing: 0) {
                header
                Spacer(minLength: 0)
                // Nothing to press when the set plays here: expanding the
                // player is what summoned it. The disc stays for the window
                // surface, and for files only mpv can open.
                if model.snapshot?.hasVideo == true && !model.panelVideoPossible {
                    SetSummonButton()
                }
                Spacer(minLength: 0)
                if keyboardDriven {
                    hint
                } else {
                    transport
                }
            }
            .padding(14)
        }
        .frame(width: 380, height: Self.height)
        .clipped()
        // The whole view takes the keyboard, not a control inside it: there is
        // nothing to tab between, and a focus ring over a video is noise.
        .focusable()
        .focusEffectDisabled()
        .focused($focused)
        .onKeyPress { press in
            guard let key = PlayerModel.PlayerKey.from(press.key, modifiers: press.modifiers) else {
                return .ignored
            }
            model.perform(key)
            return .handled
        }
        .onAppear { focused = true }
        .onChange(of: keyboardDriven) { _, driven in
            guard driven else { return }
            // Say what the keys are once, then get out of the way.
            showHint = true
            Task { @MainActor in
                try? await Task.sleep(for: .seconds(3))
                withAnimation(.easeOut(duration: 0.6)) { showHint = false }
            }
        }
        .animation(.easeOut(duration: 0.2), value: keyboardDriven)
        .transition(.opacity.combined(with: .scale(scale: 0.94)))
    }

    private var backdrop: some View {
        Group {
            if model.panelVideoShowing {
                // The set itself, playing where the artwork would be. Filled
                // rather than fitted: this is a backdrop for the transport to
                // sit on, and the panel is portrait while a set is not.
                PanelVideo(view: model.panelVideoView())
            } else if let image = model.artwork {
                Image(nsImage: image)
                    .resizable()
                    .aspectRatio(contentMode: .fill)
            } else {
                LinearGradient(
                    colors: [Color.accentColor.opacity(0.35), .black],
                    startPoint: .topLeading,
                    endPoint: .bottomTrailing
                )
                .overlay {
                    Image(systemName: "waveform")
                        .font(.system(size: 64))
                        .foregroundStyle(.white.opacity(0.25))
                }
            }
        }
        .frame(width: 380, height: Self.height)
    }

    /// The pop-out glyph from the transport bar, mirrored: the same rectangle
    /// pair collapsing inward reads as "put this back in the panel".
    private var header: some View {
        HStack {
            Button(action: onBack) {
                Image(systemName: "arrow.down.right.and.arrow.up.left")
                    .font(.callout.weight(.semibold))
                    .padding(7)
                    .background(.black.opacity(0.35), in: Circle())
            }
            .buttonStyle(.plain)
            .help("Back to the player")
            Spacer()
        }
    }

    /// What replaces the transport while the keys are live. Fades out on its
    /// own: the controls are gone, so the first thing to say is how to drive
    /// it without them.
    private var hint: some View {
        Text("Space  play  ·  ← →  30s  ·  ↑ ↓  volume  ·  Esc  back")
            .font(.caption2.monospacedDigit())
            .foregroundStyle(.white.opacity(0.85))
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .background(.black.opacity(0.35), in: Capsule())
            .opacity(showHint ? 1 : 0)
            .shadow(color: .black.opacity(0.5), radius: 6, y: 1)
    }

    private var transport: some View {
        HStack(spacing: 22) {
            Button { model.seek(to: 0) } label: {
                Image(systemName: "arrow.counterclockwise")
            }
            .help("Restart from the beginning")

            Button { model.skip(-30) } label: {
                Image(systemName: "gobackward.30")
            }
            .help("Back 30 seconds")

            Button { model.togglePlayPause() } label: {
                Image(systemName: model.isPlaying ? "pause.circle.fill" : "play.circle.fill")
                    .font(.system(size: 44))
            }
            .help(model.isPlaying ? "Pause" : "Play")
            .disabled(!model.hasTrack)

            Button { model.skip(30) } label: {
                Image(systemName: "goforward.30")
            }
            .help("Forward 30 seconds")

            Button { model.next() } label: {
                Image(systemName: "forward.fill")
            }
            .help("Next track")
        }
        .font(.title3)
        .buttonStyle(.plain)
        .foregroundStyle(.white)
        .shadow(color: .black.opacity(0.5), radius: 6, y: 1)
    }
}

/// Pops the video window out from the middle of the artwork.
///
/// Deliberately theatrical: rings race outward on summon and collapse inward on
/// dismissal, so the two directions are told apart by motion alone. The rings
/// fire on the click rather than off `videoVisible`, because the snapshot that
/// carries the new state is up to a tick behind the press.
private struct SetSummonButton: View {
    @EnvironmentObject private var model: PlayerModel
    @State private var pulses: [Pulse] = []
    @State private var charge: Double = 0
    @State private var spin = false
    @State private var discAngle: Double = 0
    @State private var hovering = false

    private var active: Bool { model.snapshot?.videoVisible ?? false }

    /// Says where the set is about to appear, because that now depends on a
    /// setting and on which engine holds the file.
    private var helpText: String {
        if active { return model.panelVideoShowing ? "Send the set away" : "Dismiss the set" }
        return model.videoSurface == .panel && model.panelVideoSupported
            ? "Play the set here"
            : "Summon the set"
    }

    var body: some View {
        Button {
            summon(outward: !active)
            model.toggleVideo()
        } label: {
            core
        }
        .buttonStyle(.plain)
        .help(helpText)
    }

    private var core: some View {
        ZStack {
            ForEach(pulses) { pulse in
                SummonPulse(outward: pulse.outward, delay: pulse.delay)
            }

            // Barely there at rest: the artwork behind it should still read.
            Circle()
                .fill(.ultraThinMaterial)
                .opacity(0.3)
                .overlay(Circle().strokeBorder(.white.opacity(0.2), lineWidth: 1))

            // The aura only turns while the set is out, which is what makes the
            // summoned state feel held rather than merely toggled.
            Circle()
                .strokeBorder(
                    AngularGradient(
                        colors: [.clear, Color.accentColor, .white, Color.accentColor, .clear],
                        center: .center
                    ),
                    lineWidth: 2.5
                )
                .rotationEffect(.degrees(spin ? 360 : 0))
                .opacity(active ? 0.75 : 0)
                .scaleEffect(1.18)

            // The record turns only while the set is out, so the summoned
            // state is legible from motion without a label under the button.
            Image(systemName: active ? "opticaldisc.fill" : "opticaldisc")
                .font(.system(size: 30, weight: .light))
                .foregroundStyle(.white.opacity(active ? 0.95 : 0.8))
                .rotationEffect(.degrees(discAngle))
                // Additive, so a nudge on hover reads even while the record is
                // already turning under it.
                .rotationEffect(.degrees(hovering ? 14 : 0))
                .contentTransition(.symbolEffect(.replace))
        }
        .frame(width: 76, height: 76)
        .scaleEffect((1 + charge * 0.16) * (hovering ? 1.08 : 1))
        .shadow(color: Color.accentColor.opacity(active ? 0.6 : 0.18), radius: 16 + charge * 14)
        .contentShape(Circle())
        .onHover { hovering = $0 }
        .animation(.spring(response: 0.3, dampingFraction: 0.5), value: charge)
        .animation(.easeOut(duration: 0.25), value: active)
        .animation(.spring(response: 0.3, dampingFraction: 0.65), value: hovering)
        .onAppear {
            withAnimation(.linear(duration: 6).repeatForever(autoreverses: false)) { spin = true }
            if active { startTurning() }
        }
        // Driven from the snapshot rather than the press: the record should be
        // turning exactly while the window is up, however that came about.
        .onChange(of: active) { _, on in
            if on {
                startTurning()
            } else {
                withAnimation(.easeOut(duration: 0.45)) { discAngle = 0 }
            }
        }
    }

    private func startTurning() {
        withAnimation(.linear(duration: 8).repeatForever(autoreverses: false)) {
            discAngle = 360
        }
    }

    private func summon(outward: Bool) {
        let wave = (0..<3).map { Pulse(outward: outward, delay: Double($0) * 0.12) }
        pulses.append(contentsOf: wave)
        charge = 1

        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(160))
            charge = 0
            // Rings are only ever added, so retiring this wave by id keeps a
            // rapid second press from cutting the first one short.
            try? await Task.sleep(for: .milliseconds(1_100))
            let spent = Set(wave.map(\.id))
            pulses.removeAll { spent.contains($0.id) }
        }
    }

    private struct Pulse: Identifiable {
        let id = UUID()
        let outward: Bool
        let delay: Double
    }
}

/// The engine's video view, hosted in the panel.
///
/// The view is made by the engine and handed over as-is — SwiftUI rebuilds
/// this struct freely, and rebuilding the player layer with it would tear the
/// picture down on every redraw.
private struct PanelVideo: NSViewRepresentable {
    let view: NSView

    func makeNSView(context: Context) -> NSView { view }
    func updateNSView(_ nsView: NSView, context: Context) {}
}

/// One ring of the summon. Animates itself on appear and is discarded by the
/// button once it has run.
private struct SummonPulse: View {
    let outward: Bool
    let delay: Double
    @State private var progress: Double = 0

    var body: some View {
        Circle()
            .strokeBorder(Color.accentColor.opacity(0.9), lineWidth: 2.5 - progress * 2)
            .scaleEffect(outward ? 0.55 + progress * 1.85 : 2.4 - progress * 1.85)
            // Deconstruction ends tight and hard rather than fading to nothing,
            // so the collapse lands on the button instead of drifting off it.
            .opacity(outward ? 1 - progress : 1 - pow(progress, 5))
            .blur(radius: outward ? progress * 2 : (1 - progress) * 3)
            .onAppear {
                withAnimation(.easeOut(duration: 0.8).delay(delay)) { progress = 1 }
            }
    }
}
