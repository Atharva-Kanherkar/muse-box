import Foundation
import Testing
@testable import MuseBoxCore

@Suite struct SpotifyBroadcastTests {
    private let stamp = Date(timeIntervalSince1970: 1_000)

    /// The shape Spotify posts on a play.
    @Test func aPlayingBroadcastIsATrack() throws {
        let info: [AnyHashable: Any] = [
            "Player State": "Playing",
            "Track ID": "spotify:track:5FVd6KXrgO9B3JPmC8OPst",
            "Name": "Do I Wanna Know?",
            "Artist": "Arctic Monkeys",
            "Album": "AM",
            "Duration": NSNumber(value: 272_000),
            "Playback Position": NSNumber(value: 84.25),
            "Has Artwork": NSNumber(value: true),
        ]
        guard case .track(let track) = SpotifyBroadcast.reading(info, at: stamp) else {
            Issue.record("expected a track")
            return
        }
        #expect(track.state == .playing)
        #expect(track.spotifyTrackID == "5FVd6KXrgO9B3JPmC8OPst")
        #expect(track.durationMs == 272_000)
        #expect(track.positionMs == 84_250)
        #expect(track.progressMs(at: stamp.addingTimeInterval(1)) == 85_250)
        #expect(track.artworkURL == nil)
    }

    @Test func pausedHoldsItsPlace() {
        let info: [AnyHashable: Any] = [
            "Player State": "Paused", "Track ID": "spotify:track:x",
            "Duration": NSNumber(value: 200_000), "Playback Position": NSNumber(value: 10),
        ]
        guard case .track(let track) = SpotifyBroadcast.reading(info, at: stamp) else {
            Issue.record("expected a track")
            return
        }
        #expect(track.state == .paused)
        #expect(track.progressMs(at: stamp.addingTimeInterval(30)) == 10_000)
    }

    @Test func stoppedAndJunkAreNotTracks() {
        #expect(SpotifyBroadcast.reading(["Player State": "Stopped"], at: stamp) == .stopped)
        #expect(SpotifyBroadcast.reading(["Player State": "Playing"], at: stamp) == .stopped)
        #expect(SpotifyBroadcast.reading(["Something": "else"], at: stamp) == .ignored)
        #expect(SpotifyBroadcast.reading(["Player State": "Buffering"], at: stamp) == .ignored)
    }

    @Test func scriptDurationsAreMilliseconds() {
        #expect(SpotifyBroadcast.milliseconds(scriptDuration: 272_000) == 272_000)
        #expect(SpotifyBroadcast.milliseconds(scriptDuration: 272) == 272_000)
        #expect(SpotifyBroadcast.milliseconds(scriptDuration: 0) == 0)
    }
}
