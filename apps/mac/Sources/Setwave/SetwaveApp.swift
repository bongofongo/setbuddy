import SwiftUI
import SetwaveUI

@main
struct SetwaveApp: App {
    @StateObject private var model = PlayerModel()

    var body: some Scene {
        MenuBarExtra {
            MenuBarView().environmentObject(model)
        } label: {
            // The icon carries the state: a set playing from the menu bar with
            // no window is the normal case, so it has to be readable at a glance.
            Image(systemName: menuBarSymbol)
        }
        .menuBarExtraStyle(.window)
    }

    private var menuBarSymbol: String {
        guard model.engineAvailable else { return "waveform.badge.exclamationmark" }
        guard model.hasTrack else { return "waveform" }
        return model.isPlaying ? "waveform.circle.fill" : "waveform.circle"
    }
}
