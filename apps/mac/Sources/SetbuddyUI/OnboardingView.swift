import SwiftUI

/// Shown when mpv is not installed.
///
/// Setwave requires mpv rather than bundling it, so this is the whole setup
/// story: one command, copyable, with no further ceremony.
struct OnboardingView: View {
    @State private var copied = false
    private let command = "brew install mpv"

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label("mpv is not installed", systemImage: "exclamationmark.triangle")
                .font(.headline)

            Text("Setwave plays through mpv, which handles the .webm and .mkv "
                 + "files that downloaded sets arrive in. Install it, then reopen Setwave.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            HStack {
                Text(command)
                    .font(.system(.body, design: .monospaced))
                    .textSelection(.enabled)
                Spacer()
                Button(copied ? "Copied" : "Copy") {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(command, forType: .string)
                    copied = true
                }
                .buttonStyle(.borderless)
            }
            .padding(8)
            .background(.quaternary, in: RoundedRectangle(cornerRadius: 6))

            HStack {
                Button("Recheck") {
                    // Relaunching is the honest way to re-run the check: the
                    // core is built once, at startup.
                    restart()
                }
                Spacer()
                Button("Quit") { NSApplication.shared.terminate(nil) }
            }
        }
        .padding(16)
    }

    private func restart() {
        let url = Bundle.main.bundleURL
        let configuration = NSWorkspace.OpenConfiguration()
        configuration.createsNewApplicationInstance = true
        NSWorkspace.shared.openApplication(at: url, configuration: configuration) { _, _ in
            DispatchQueue.main.async { NSApplication.shared.terminate(nil) }
        }
    }
}
