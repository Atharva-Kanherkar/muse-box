import Foundation

public enum PlaybackState: String, Sendable {
    case idle
    case playing
    case paused
}

/// What is on right now, in the render document's terms: position is a
/// stamped sample, and progress is interpolated locally from it
/// (`progress_ms + (now - server_ts)`), never streamed.
public struct NowPlaying: Equatable, Sendable {
    /// `spotify:track:…`, `spotify:episode:…`, `spotify:local:…`.
    public var trackID: String
    public var title: String
    public var artist: String
    public var album: String
    public var durationMs: Int
    public var artworkURL: URL?
    public var state: PlaybackState
    /// Position at `stamp`.
    public var positionMs: Double
    public var stamp: Date

    public init(
        trackID: String, title: String, artist: String, album: String,
        durationMs: Int, artworkURL: URL?, state: PlaybackState,
        positionMs: Double, stamp: Date
    ) {
        self.trackID = trackID
        self.title = title
        self.artist = artist
        self.album = album
        self.durationMs = durationMs
        self.artworkURL = artworkURL
        self.state = state
        self.positionMs = positionMs
        self.stamp = stamp
    }

    public func progressMs(at now: Date) -> Double {
        let limit = Double(max(durationMs, 0))
        guard state == .playing else { return min(max(positionMs, 0), limit) }
        let elapsed = max(now.timeIntervalSince(stamp), 0) * 1000
        return min(max(positionMs, 0) + elapsed, limit)
    }

    /// The bare base-62 ID for `spotify:track:<id>`, else nil.
    public var spotifyTrackID: String? {
        let prefix = "spotify:track:"
        guard trackID.hasPrefix(prefix) else { return nil }
        let id = String(trackID.dropFirst(prefix.count))
        return id.isEmpty ? nil : id
    }

    /// Songs get lyrics; ads, episodes and local files do not.
    public var isSong: Bool { spotifyTrackID != nil }

    /// Same track, same state, and the clock we already have still agrees
    /// with the new sample. Re-stamping on every poll would make the progress
    /// needle jitter, so a sample only replaces the stamp on real drift (a seek).
    public func agrees(with sample: NowPlaying, toleranceMs: Double = 1_500) -> Bool {
        guard trackID == sample.trackID, state == sample.state else { return false }
        return abs(progressMs(at: sample.stamp) - sample.positionMs) <= toleranceMs
    }
}

/// `m:ss`, the times under the title.
public func formatDuration(_ milliseconds: Double) -> String {
    guard milliseconds.isFinite, milliseconds >= 0 else { return "0:00" }
    let total = Int(milliseconds / 1000)
    return "\(total / 60):" + String(format: "%02d", total % 60)
}

/// Bitka's line picker from `Mascot.tsx`: a 32-bit string hash, so the same
/// track always earns the same compliment.
public func stableLine(for seed: String, from pool: [String]) -> String {
    guard !pool.isEmpty else { return "" }
    var hash: Int32 = 0
    for unit in seed.utf16 { hash = hash &* 31 &+ Int32(unit) }
    return pool[Int(hash.magnitude % UInt32(pool.count))]
}

public func stableHash(_ seed: String) -> UInt32 {
    var hash: Int32 = 0
    for unit in seed.utf16 { hash = hash &* 31 &+ Int32(unit) }
    return hash.magnitude
}
