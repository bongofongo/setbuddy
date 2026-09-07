import SwiftUI
import SetbuddyCore

/// Everything known about a track, with its artwork: the indexed fields, the
/// file on disk, and — fetched on open — what `ffprobe` says about the
/// container, its tags and its streams.
struct TrackDetailsView: View {
    @EnvironmentObject private var model: PlayerModel
    let track: Track

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                header
                section("Track", trackRows)
                section("File", fileRows)
                probed
            }
            .padding(16)
        }
        .frame(width: 380)
        .frame(maxHeight: 560)
        .onAppear { model.loadDetails(for: track) }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 12) {
            Group {
                if let image = model.artwork, model.snapshot?.track?.id == track.id {
                    Image(nsImage: image)
                        .resizable()
                        .aspectRatio(contentMode: .fill)
                } else if let image = model.rowArtwork[track.id] ?? nil {
                    Image(nsImage: image)
                        .resizable()
                        .aspectRatio(contentMode: .fill)
                } else {
                    ZStack {
                        Rectangle().fill(.quaternary)
                        Image(systemName: track.hasVideo ? "film" : "music.note")
                            .font(.title)
                            .foregroundStyle(.tertiary)
                    }
                }
            }
            .frame(width: track.hasVideo ? 160 : 110, height: track.hasVideo ? 90 : 110)
            .clipShape(RoundedRectangle(cornerRadius: 8))
            .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(.separator, lineWidth: 0.5))
            .onAppear { model.requestArtwork(for: track) }

            VStack(alignment: .leading, spacing: 3) {
                Text(track.title ?? track.displayLabel)
                    .font(.headline)
                    .lineLimit(3)
                if let artist = track.artist {
                    Text(artist).font(.subheadline).foregroundStyle(.secondary)
                }
                if let album = track.album {
                    Text(album).font(.caption).foregroundStyle(.tertiary)
                }
                Spacer(minLength: 0)
                HStack(spacing: 6) {
                    Image(systemName: track.hasVideo ? "film" : "waveform")
                    Text(track.hasVideo ? "Video" : "Audio")
                    if let duration = track.durationSecs {
                        Text("·")
                        Text(formatDuration(seconds: duration))
                    }
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            }
        }
    }

    private var trackRows: [(String, String)] {
        var rows: [(String, String)] = [
            ("Title", track.title ?? "—"),
            ("Artist", track.artist ?? "—"),
            ("Album", track.album ?? "—"),
        ]
        if let duration = track.durationSecs {
            rows.append(("Duration", formatDuration(seconds: duration)))
        }
        if let resume = track.resumeSecs {
            rows.append(("Resumes at", formatDuration(seconds: resume)))
        }
        rows.append(("Last played", track.lastPlayedAt.map(date) ?? "Never"))
        if model.snapshot?.track?.id == track.id, let engine = model.snapshot?.engineId {
            rows.append(("Engine", engine))
        }
        rows.append(("Library id", String(track.id)))
        return rows
    }

    private var fileRows: [(String, String)] {
        [
            ("Path", (track.path as NSString).abbreviatingWithTildeInPath),
            ("Size", ByteCountFormatter.string(fromByteCount: track.sizeBytes, countStyle: .file)),
            ("Modified", date(track.mtime)),
            ("Indexed", date(track.addedAt)),
        ]
    }

    /// The ffprobe section: a spinner while it runs, its rows once it has, and
    /// a note when there is nothing to show.
    @ViewBuilder private var probed: some View {
        if model.detailsTrackId != track.id {
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text("Reading the file…").font(.caption).foregroundStyle(.secondary)
            }
        } else if model.details.isEmpty {
            Text("No further metadata. ffprobe is either not installed or could not read this file.")
                .font(.caption)
                .foregroundStyle(.tertiary)
                .fixedSize(horizontal: false, vertical: true)
        } else {
            section("Format & streams", model.details.map { ($0.label, $0.value) })
        }
    }

    private func section(_ title: String, _ rows: [(String, String)]) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title.uppercased())
                .font(.caption2.weight(.semibold))
                .foregroundStyle(.tertiary)
            Grid(alignment: .topLeading, horizontalSpacing: 10, verticalSpacing: 4) {
                ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                    GridRow {
                        Text(row.0)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .gridColumnAlignment(.trailing)
                        Text(row.1)
                            .font(.caption)
                            .textSelection(.enabled)
                            .fixedSize(horizontal: false, vertical: true)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }
        }
    }

    private func date(_ unix: Int64) -> String {
        Date(timeIntervalSince1970: TimeInterval(unix))
            .formatted(date: .abbreviated, time: .shortened)
    }
}
