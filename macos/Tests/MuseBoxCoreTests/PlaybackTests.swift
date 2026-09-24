import Foundation
import Testing
@testable import MuseBoxCore

@Suite struct PlaybackTests {
    private func track(_ state: PlaybackState, at position: Double, stamp: Date) -> NowPlaying {
        NowPlaying(
            trackID: "spotify:track:5FVd6KXrgO9B3JPmC8OPst", title: "Do I Wanna Know?",
            artist: "Arctic Monkeys", album: "AM", durationMs: 272_000, artworkURL: nil,
            state: state, positionMs: position, stamp: stamp
        )
    }

    /// `rendered_progress = progress_ms + (now - server_ts)` while playing.
    @Test func progressIsInterpolatedWhilePlaying() {
        let stamp = Date(timeIntervalSince1970: 1_000)
        let playing = track(.playing, at: 84_000, stamp: stamp)
        #expect(playing.progressMs(at: stamp.addingTimeInterval(2.5)) == 86_500)
        #expect(playing.progressMs(at: stamp.addingTimeInterval(-5)) == 84_000)
        #expect(playing.progressMs(at: stamp.addingTimeInterval(1_000)) == 272_000)
    }

    @Test func progressHoldsWhilePaused() {
        let stamp = Date(timeIntervalSince1970: 1_000)
        let paused = track(.paused, at: 84_000, stamp: stamp)
        #expect(paused.progressMs(at: stamp.addingTimeInterval(60)) == 84_000)
    }

    /// Polls that agree with the running clock must not re-stamp it (jitter);
    /// a seek must.
    @Test func onlyRealDriftReplacesTheClock() {
        let stamp = Date(timeIntervalSince1970: 1_000)
        let running = track(.playing, at: 10_000, stamp: stamp)
        #expect(running.agrees(with: track(.playing, at: 12_300, stamp: stamp.addingTimeInterval(2))))
        #expect(!running.agrees(with: track(.playing, at: 90_000, stamp: stamp.addingTimeInterval(2))))
        #expect(!running.agrees(with: track(.paused, at: 12_000, stamp: stamp.addingTimeInterval(2))))
    }

    @Test func durationsFormatLikeTheWebClient() {
        #expect(formatDuration(85_000) == "1:25")
        #expect(formatDuration(233_000) == "3:53")
        #expect(formatDuration(-1) == "0:00")
    }

    @Test func onlySongsHaveABareTrackID() {
        #expect(track(.playing, at: 0, stamp: .now).spotifyTrackID == "5FVd6KXrgO9B3JPmC8OPst")
        var episode = track(.playing, at: 0, stamp: .now)
        episode.trackID = "spotify:episode:abc"
        #expect(episode.spotifyTrackID == nil)
        #expect(!episode.isSong)
    }

    @Test func linesAreStablePerSeed() {
        let pool = ["a", "b", "c"]
        #expect(stableLine(for: "spotify:track:x", from: pool) == stableLine(for: "spotify:track:x", from: pool))
    }
}
