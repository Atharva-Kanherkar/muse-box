import Foundation

/// The Spotify desktop app's own broadcast. It posts this distributed
/// notification on every play, pause, skip and seek, to anyone listening,
/// with no permission involved: this is how muse-box follows Spotify without
/// the Web API, a login, or a developer app.
public enum SpotifyBroadcast {
    public static let name = Notification.Name("com.spotify.client.PlaybackStateChanged")

    public enum Reading: Equatable {
        case track(NowPlaying)
        case stopped
        /// Not something we understand; change nothing.
        case ignored
    }

    /// `userInfo` carries "Player State" (Playing/Paused/Stopped), "Track ID",
    /// "Name", "Artist", "Album", "Duration" (ms) and "Playback Position" (s).
    /// No cover URL: that comes from AppleScript or Spotify's oEmbed.
    public static func reading(_ info: [AnyHashable: Any], at stamp: Date) -> Reading {
        guard let state = info["Player State"] as? String else { return .ignored }
        switch state {
        case "Stopped":
            return .stopped
        case "Playing", "Paused":
            guard let id = info["Track ID"] as? String, !id.isEmpty else { return .stopped }
            return .track(NowPlaying(
                trackID: id,
                title: info["Name"] as? String ?? "",
                artist: info["Artist"] as? String ?? "",
                album: info["Album"] as? String ?? "",
                durationMs: (info["Duration"] as? NSNumber)?.intValue ?? 0,
                artworkURL: nil,
                state: state == "Playing" ? .playing : .paused,
                positionMs: ((info["Playback Position"] as? NSNumber)?.doubleValue ?? 0) * 1000,
                stamp: stamp
            ))
        default:
            return .ignored
        }
    }

    /// AppleScript's `duration of current track`: the dictionary says seconds,
    /// Spotify answers in milliseconds. Take either.
    public static func milliseconds(scriptDuration raw: Int) -> Int {
        raw > 0 && raw < 5_000 ? raw * 1000 : max(raw, 0)
    }
}
