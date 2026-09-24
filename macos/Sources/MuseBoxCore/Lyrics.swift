import Foundation

/// One lyric line and where it falls in the track.
public struct LyricLine: Hashable, Sendable {
    public var atMs: Int
    public var text: String

    public init(atMs: Int, text: String) {
        self.atMs = atMs
        self.text = text
    }
}

/// Time-synced lyrics from LRCLIB, mirroring `src/lyrics.rs`. `synced` false
/// means show the words whole; never pretend to follow along.
public struct Lyrics: Hashable, Sendable {
    public var synced: Bool
    public var lines: [LyricLine]

    public init(synced: Bool, lines: [LyricLine]) {
        self.synced = synced
        self.lines = lines
    }

    /// The line being sung at `progressMs` (-1 before the first) and the next
    /// line someone will actually hear, skipping instrumental gaps.
    public func cursor(atMs progressMs: Double) -> (current: Int, next: Int) {
        guard synced else { return (-1, -1) }
        var current = -1
        for (index, line) in lines.enumerated() {
            if Double(line.atMs) <= progressMs { current = index } else { break }
        }
        var next = current + 1
        while next < lines.count, lines[next].text.trimmingCharacters(in: .whitespaces).isEmpty {
            next += 1
        }
        return (current, next)
    }
}

/// An LRCLIB `/api/get` or `/api/search` record.
public struct LrclibRecord: Decodable, Sendable {
    public var plainLyrics: String?
    public var syncedLyrics: String?
    /// Seconds. Only present on search results.
    public var duration: Double?

    public init(plainLyrics: String? = nil, syncedLyrics: String? = nil, duration: Double? = nil) {
        self.plainLyrics = plainLyrics
        self.syncedLyrics = syncedLyrics
        self.duration = duration
    }
}

public enum LRC {
    /// Releases within this much of the track's length are the same recording.
    public static let durationToleranceMs = 8_000

    /// Prefer a synced result whose length matches, then any synced one, then
    /// anything at all. A mismatched master drifts audibly.
    public static func pickBest(_ results: [LrclibRecord], durationMs: Int) -> LrclibRecord? {
        func close(_ record: LrclibRecord) -> Bool {
            guard let seconds = record.duration else { return false }
            return abs(Int(seconds * 1000) - durationMs) <= durationToleranceMs
        }
        return results.first { $0.syncedLyrics != nil && close($0) }
            ?? results.first { $0.syncedLyrics != nil }
            ?? results.first
    }

    public static func lyrics(from record: LrclibRecord) -> Lyrics? {
        if let synced = record.syncedLyrics {
            let lines = parse(synced)
            if !lines.isEmpty { return Lyrics(synced: true, lines: lines) }
        }
        if let plain = record.plainLyrics {
            let lines = plain
                .split(whereSeparator: \.isNewline)
                .map { $0.trimmingCharacters(in: .whitespaces) }
                .filter { !$0.isEmpty }
                .map { LyricLine(atMs: 0, text: $0) }
            if !lines.isEmpty { return Lyrics(synced: false, lines: lines) }
        }
        return nil
    }

    /// `[mm:ss.xx] text`, several stamps allowed per line for a repeated
    /// chorus, blank lines kept as instrumental gaps.
    public static func parse(_ source: String) -> [LyricLine] {
        var lines: [LyricLine] = []
        for raw in source.split(omittingEmptySubsequences: false, whereSeparator: \.isNewline) {
            var rest = Substring(raw)
            var stamps: [Int] = []
            while rest.hasPrefix("[") {
                guard let close = rest.firstIndex(of: "]") else { break }
                if let at = timestamp(rest[rest.index(after: rest.startIndex)..<close]) {
                    stamps.append(at)
                }
                rest = rest[rest.index(after: close)...]
            }
            let text = rest.trimmingCharacters(in: .whitespaces)
            lines += stamps.map { LyricLine(atMs: $0, text: text) }
        }
        // Stable, so a chorus keeps its written order at equal stamps.
        return lines.enumerated()
            .sorted { $0.element.atMs != $1.element.atMs ? $0.element.atMs < $1.element.atMs : $0.offset < $1.offset }
            .map(\.element)
    }

    /// `mm:ss.xx`, `mm:ss.xxx` or `mm:ss`. Tags like `[ar:Artist]` are nil.
    public static func timestamp(_ stamp: Substring) -> Int? {
        guard let colon = stamp.firstIndex(of: ":"),
              let minutes = Int(stamp[..<colon].trimmingCharacters(in: .whitespaces)), minutes >= 0
        else { return nil }
        let rest = stamp[stamp.index(after: colon)...]
        let secondsPart: Substring
        let fraction: Substring
        if let split = rest.firstIndex(where: { $0 == "." || $0 == ":" }) {
            secondsPart = rest[..<split]
            fraction = rest[rest.index(after: split)...]
        } else {
            secondsPart = rest
            fraction = ""
        }
        guard let seconds = Int(secondsPart.trimmingCharacters(in: .whitespaces)), seconds >= 0 else { return nil }
        let digits = fraction.trimmingCharacters(in: .whitespaces)
        let millis: Int
        switch digits.count {
        case 0: millis = 0
        case 1: guard let value = Int(digits) else { return nil }; millis = value * 100
        case 2: guard let value = Int(digits) else { return nil }; millis = value * 10
        default: guard let value = Int(digits.prefix(3)) else { return nil }; millis = value
        }
        return minutes * 60_000 + seconds * 1_000 + millis
    }
}
