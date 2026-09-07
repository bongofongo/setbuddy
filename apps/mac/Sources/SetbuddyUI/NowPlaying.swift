import AppKit
import MediaPlayer
import SetwaveCore

/// Owns the system Now Playing widget and the media keys.
///
/// Setwave can own these only because the engine gives them up: mpv is spawned
/// with `--media-controls=no` and `--input-media-keys=no`, so it neither claims
/// the Control Center widget nor eats F7/F8/F9. Without that, both processes
/// would answer every key press and each one would toggle twice.
final class NowPlayingBridge {
    private var isConnected = false
    private var artwork: MPMediaItemArtwork?

    /// Cache the artwork so each tick's info update can carry it without
    /// rebuilding an `MPMediaItemArtwork` every time.
    func setArtwork(_ image: NSImage?) {
        guard let image else {
            artwork = nil
            return
        }
        artwork = MPMediaItemArtwork(boundsSize: image.size) { _ in image }
    }

    func connect(
        togglePlayPause: @escaping () -> Void,
        next: @escaping () -> Void,
        previous: @escaping () -> Void,
        seek: @escaping (Double) -> Void
    ) {
        guard !isConnected else { return }
        isConnected = true

        let center = MPRemoteCommandCenter.shared()

        center.togglePlayPauseCommand.isEnabled = true
        center.togglePlayPauseCommand.addTarget { _ in
            togglePlayPause()
            return .success
        }
        center.playCommand.isEnabled = true
        center.playCommand.addTarget { _ in
            togglePlayPause()
            return .success
        }
        center.pauseCommand.isEnabled = true
        center.pauseCommand.addTarget { _ in
            togglePlayPause()
            return .success
        }
        center.nextTrackCommand.isEnabled = true
        center.nextTrackCommand.addTarget { _ in
            next()
            return .success
        }
        center.previousTrackCommand.isEnabled = true
        center.previousTrackCommand.addTarget { _ in
            previous()
            return .success
        }
        // Lets the Control Center scrubber drive playback, not just report it.
        center.changePlaybackPositionCommand.isEnabled = true
        center.changePlaybackPositionCommand.addTarget { event in
            guard let event = event as? MPChangePlaybackPositionCommandEvent else {
                return .commandFailed
            }
            seek(event.positionTime)
            return .success
        }
    }

    func update(with snapshot: PlayerSnapshot) {
        let center = MPNowPlayingInfoCenter.default()
        guard let track = snapshot.track else {
            center.nowPlayingInfo = nil
            center.playbackState = .stopped
            return
        }

        var info: [String: Any] = [
            MPMediaItemPropertyTitle: track.title ?? track.displayLabel,
            MPNowPlayingInfoPropertyMediaType: NSNumber(
                value: (track.hasVideo ? MPNowPlayingInfoMediaType.video
                                       : MPNowPlayingInfoMediaType.audio).rawValue
            ),
            // A rate of 0 is what tells the widget to stop animating.
            MPNowPlayingInfoPropertyPlaybackRate: snapshot.paused ? 0.0 : 1.0,
        ]
        if let artist = track.artist {
            info[MPMediaItemPropertyArtist] = artist
        }
        if let album = track.album {
            info[MPMediaItemPropertyAlbumTitle] = album
        }
        if let duration = snapshot.durationSecs, duration > 0 {
            info[MPMediaItemPropertyPlaybackDuration] = duration
        }
        if let position = snapshot.positionSecs {
            info[MPNowPlayingInfoPropertyElapsedPlaybackTime] = position
        }
        if let artwork {
            info[MPMediaItemPropertyArtwork] = artwork
        }

        center.nowPlayingInfo = info
        center.playbackState = snapshot.idle ? .stopped : (snapshot.paused ? .paused : .playing)
    }

    func clear() {
        artwork = nil
        let center = MPNowPlayingInfoCenter.default()
        center.nowPlayingInfo = nil
        center.playbackState = .stopped
    }
}
