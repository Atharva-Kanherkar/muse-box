import Foundation
import MuseBoxCore

/// LRCLIB, straight from the Mac: free, keyless, human-timed. The same two
/// lookups as `src/lyrics.rs` (exact match, then a search), and misses are
/// remembered so a track on repeat does not re-ask.
actor LyricsService {
    private var cache: [String: Lyrics?] = [:]
    private var order: [String] = []
    private let session: URLSession = {
        let configuration = URLSessionConfiguration.default
        configuration.timeoutIntervalForRequest = 10
        configuration.httpAdditionalHeaders = [
            // LRCLIB asks callers to identify themselves.
            "User-Agent": "muse-box-mac/\(Bundle.main.shortVersion) (https://github.com/Atharva-Kanherkar/muse-box)",
        ]
        return URLSession(configuration: configuration)
    }()

    func lyrics(for track: NowPlaying) async -> Lyrics? {
        guard track.isSong, !track.title.isEmpty else { return nil }
        if let hit = cache[track.trackID] { return hit }
        do {
            let found = try await fetch(track)
            remember(track.trackID, found)
            return found
        } catch {
            // Offline or LRCLIB down: try again next time, do not cache a miss.
            return nil
        }
    }

    private func fetch(_ track: NowPlaying) async throws -> Lyrics? {
        var exact = URLComponents(string: "https://lrclib.net/api/get")
        exact?.queryItems = [
            URLQueryItem(name: "track_name", value: track.title),
            URLQueryItem(name: "artist_name", value: track.artist),
            URLQueryItem(name: "album_name", value: track.album),
            URLQueryItem(name: "duration", value: String(track.durationMs / 1000)),
        ]
        if let url = exact?.url {
            let (data, response) = try await session.data(from: url)
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            if status == 200, let record = try? JSONDecoder().decode(LrclibRecord.self, from: data),
               let lyrics = LRC.lyrics(from: record) {
                return lyrics
            }
            // 404 is the normal answer for a miss; anything else is trouble.
            if status != 404, status != 200 { throw URLError(.badServerResponse) }
        }

        var search = URLComponents(string: "https://lrclib.net/api/search")
        search?.queryItems = [
            URLQueryItem(name: "track_name", value: track.title),
            URLQueryItem(name: "artist_name", value: track.artist),
        ]
        guard let url = search?.url else { return nil }
        let (data, response) = try await session.data(from: url)
        guard (response as? HTTPURLResponse)?.statusCode == 200 else { return nil }
        let results = (try? JSONDecoder().decode([LrclibRecord].self, from: data)) ?? []
        return LRC.pickBest(results, durationMs: track.durationMs).flatMap(LRC.lyrics(from:))
    }

    private func remember(_ id: String, _ lyrics: Lyrics?) {
        cache[id] = .some(lyrics)
        order.removeAll { $0 == id }
        order.append(id)
        while order.count > 256 { cache[order.removeFirst()] = nil }
    }
}

extension Bundle {
    var shortVersion: String {
        object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "dev"
    }
}
