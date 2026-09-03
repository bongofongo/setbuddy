import SwiftUI
import SetwaveCore

/// Watched folders, the pop-out's size, and engine choice.
///
/// Presented as a popover from the footer rather than a separate window: a menu
/// bar app that opens a window to change one setting has lost the plot.
struct SettingsPanel: View {
    @EnvironmentObject private var model: PlayerModel
    @Environment(\.dismiss) private var dismiss
    @State private var policy: String = "auto"

    /// The layout picker's presets. The tag is the core's settings form, so a
    /// saved value that matches one selects it and any other shows as custom.
    private static let layoutPresets: [(label: String, spec: String)] = [
        ("Small — a quarter of the screen", "25%"),
        ("Medium — 40% of the screen", "40%"),
        ("Large — 60% of the screen", "60%"),
        ("Fill — edge to edge, still a window", "fill"),
        ("Fullscreen", "fullscreen"),
    ]
    private static let customTag = "custom"

    @State private var layoutChoice: String = "40%"
    @State private var customWidth: String = "1280"
    @State private var customX: String = "0"
    @State private var customY: String = "0"
    @State private var customScreen: Int = 0

    private var placing: Bool { model.placementPid != nil }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            folders
            Divider()
            videoWindow
            Divider()
            engine
        }
        .padding(16)
        .frame(width: 340)
        .onAppear {
            policy = currentPolicy
            loadLayoutChoice()
        }
    }

    private var videoWindow: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Video window")
                .font(.caption.weight(.semibold))

            Picker("", selection: $layoutChoice) {
                ForEach(Self.layoutPresets, id: \.spec) { preset in
                    Text(preset.label).tag(preset.spec)
                }
                Text("Custom size and position…").tag(Self.customTag)
            }
            .labelsHidden()
            .pickerStyle(.menu)
            .controlSize(.small)
            .onChange(of: layoutChoice) { _, choice in
                if choice != Self.customTag {
                    model.setVideoWindowLayout(choice)
                }
            }

            if layoutChoice == Self.customTag {
                customFields
            }

            Text(hint)
                .font(.caption2)
                .foregroundStyle(.tertiary)
                .fixedSize(horizontal: false, vertical: true)
        }
        // Placement runs in its own overlay, which is why this popover closing
        // does not cancel it: it closes the moment the user clicks the window
        // they are placing.
        .onChange(of: model.videoWindowLayout) { _, _ in loadLayoutChoice() }
    }

    private var customFields: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button {
                model.beginWindowPlacement()
            } label: {
                Label("Place the window visually…", systemImage: "macwindow.on.rectangle")
            }
            .controlSize(.small)
            .disabled(placing)

            Grid(alignment: .leading, horizontalSpacing: 8, verticalSpacing: 6) {
                    GridRow {
                        Text("Width").font(.caption).foregroundStyle(.secondary)
                        TextField("1280", text: $customWidth)
                            .frame(width: 70)
                        Text("px").font(.caption).foregroundStyle(.tertiary)
                    }
                    GridRow {
                        Text("Position").font(.caption).foregroundStyle(.secondary)
                        HStack(spacing: 4) {
                            TextField("x", text: $customX).frame(width: 52)
                            TextField("y", text: $customY).frame(width: 52)
                        }
                        Text("px from top-left").font(.caption).foregroundStyle(.tertiary)
                    }
                    if screenNames.count > 1 {
                        GridRow {
                            Text("Screen").font(.caption).foregroundStyle(.secondary)
                            Picker("", selection: $customScreen) {
                                ForEach(Array(screenNames.enumerated()), id: \.offset) { index, name in
                                    Text(name).tag(index)
                                }
                            }
                            .labelsHidden()
                            .gridCellColumns(2)
                        }
                    }
                }
                .textFieldStyle(.roundedBorder)
                .controlSize(.small)
                .onSubmit(applyCustom)

            HStack {
                Spacer()
                Button("Apply", action: applyCustom)
                    .controlSize(.small)
            }
        }
    }

    private var screenNames: [String] { WindowPlacement.screenNames }

    private var hint: String {
        switch layoutChoice {
        case "fullscreen":
            return "Takes the whole screen, staying above everything on it."
        case "fill":
            return "As large as fits the screen without leaving it — edge to edge in width or height, whichever the video reaches first."
        default:
            return "Applies the next time the set is summoned. Height follows the video. Sizes are in screen pixels."
        }
    }

    /// Width alone when the position fields are blank, so a custom size can
    /// still leave placement to the system.
    private func applyCustom() {
        let x = customX.trimmingCharacters(in: .whitespaces)
        let y = customY.trimmingCharacters(in: .whitespaces)
        let width = customWidth.trimmingCharacters(in: .whitespaces)
        var spec = x.isEmpty && y.isEmpty ? width : "\(width)+\(x)+\(y)"
        if screenNames.count > 1 {
            spec += "/\(customScreen)"
        }
        model.setVideoWindowLayout(spec)
    }

    /// Pick the preset matching the saved layout, or fall through to custom
    /// with the saved width and position already filled in.
    private func loadLayoutChoice() {
        let saved = model.videoWindowLayout
        if Self.layoutPresets.contains(where: { $0.spec == saved }) {
            layoutChoice = saved
            return
        }
        layoutChoice = Self.customTag
        let (geometry, screen) = saved.split(separator: "/", maxSplits: 1)
            .map(String.init)
            .reduce(into: ("", "")) { pair, part in
                if pair.0.isEmpty { pair.0 = part } else { pair.1 = part }
            }
        let parts = geometry.split(separator: "+").map(String.init)
        customWidth = parts.first ?? geometry
        if parts.count == 3 {
            customX = parts[1]
            customY = parts[2]
        } else {
            customX = ""
            customY = ""
        }
        customScreen = Int(screen) ?? 0
    }

    private var folders: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Watched folders")
                .font(.caption.weight(.semibold))

            if model.folders.isEmpty {
                Text("None yet. Add the folder your sets download into.")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            } else {
                ForEach(model.folders, id: \.self) { folder in
                    HStack(spacing: 6) {
                        Image(systemName: "folder")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Text((folder as NSString).abbreviatingWithTildeInPath)
                            .font(.caption)
                            .lineLimit(1)
                            .truncationMode(.head)
                            .help(folder)
                        Spacer()
                        Button {
                            model.removeWatchedFolder(folder)
                        } label: {
                            Image(systemName: "minus.circle")
                        }
                        .buttonStyle(.plain)
                        .foregroundStyle(.secondary)
                        .help("Stop watching this folder")
                    }
                }
            }

            HStack {
                Button("Add Folder…") { addFolder() }
                Button(model.isScanning ? "Scanning…" : "Rescan") { model.scan() }
                    .disabled(model.folders.isEmpty || model.isScanning)
            }
            .controlSize(.small)
        }
    }

    private var engine: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Playback engine")
                .font(.caption.weight(.semibold))

            Picker("", selection: $policy) {
                Text("Automatic").tag("auto")
                ForEach(model.engineIds, id: \.self) { id in
                    Text(id).tag(id)
                }
            }
            .labelsHidden()
            .pickerStyle(.menu)
            .controlSize(.small)
            .onChange(of: policy) { _, new in
                model.setEnginePolicy(new)
            }

            Text(policy == "auto"
                 ? "Picks the first engine that can open each file."
                 : "Always uses \(policy). Files it cannot open will report an error rather than falling back.")
                .font(.caption2)
                .foregroundStyle(.tertiary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var currentPolicy: String {
        // The snapshot reports the engine that is actually playing, which is the
        // best available hint when nothing has been forced.
        model.snapshot?.engineId.map { _ in "auto" } ?? "auto"
    }

    private func addFolder() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.message = "Choose a folder to watch"
        if panel.runModal() == .OK, let url = panel.url {
            model.addFolder(url)
        }
    }
}
