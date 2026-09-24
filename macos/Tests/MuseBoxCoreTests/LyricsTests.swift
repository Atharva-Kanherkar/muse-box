import Testing
@testable import MuseBoxCore

@Suite struct LyricsTests {
    @Test func timestampsParseInEveryShapeLrclibEmits() {
        #expect(LRC.timestamp("01:02.34") == 62_340)
        #expect(LRC.timestamp("01:02.345") == 62_345)
        #expect(LRC.timestamp("01:02.3") == 62_300)
        #expect(LRC.timestamp("01:02") == 62_000)
        #expect(LRC.timestamp("1:02:50") == 62_500)
        #expect(LRC.timestamp("ar:Artist") == nil)
    }

    @Test func aRepeatedChorusLandsAtEveryTimestamp() {
        let lines = LRC.parse("[00:10.00][00:30.00]chorus\n[00:20.00]verse")
        #expect(lines.map(\.atMs) == [10_000, 20_000, 30_000])
        #expect(lines.map(\.text) == ["chorus", "verse", "chorus"])
    }

    @Test func anInstrumentalGapKeepsItsPlace() {
        let lines = LRC.parse("[00:01.00]one\n[00:05.00]\n[00:09.00]two")
        #expect(lines.count == 3)
        #expect(lines[1].text.isEmpty)
    }

    @Test func untimedJunkYieldsNothing() {
        #expect(LRC.parse("[ar:Someone]\nno stamps here").isEmpty)
    }

    @Test func theClosestSyncedReleaseWins() {
        let far = LrclibRecord(syncedLyrics: "[00:01.00]far", duration: 300)
        let close = LrclibRecord(syncedLyrics: "[00:01.00]close", duration: 201)
        let plain = LrclibRecord(plainLyrics: "plain", duration: 200)
        let best = LRC.pickBest([plain, far, close], durationMs: 200_000)
        #expect(best?.syncedLyrics == "[00:01.00]close")
    }

    @Test func somethingUnsyncedIsBetterThanNothing() {
        let plain = LrclibRecord(plainLyrics: "a\n\nb", duration: nil)
        let lyrics = LRC.pickBest([plain], durationMs: 1).flatMap(LRC.lyrics(from:))
        #expect(lyrics?.synced == false)
        #expect(lyrics?.lines.map(\.text) == ["a", "b"])
    }

    @Test func theCursorFollowsTheSongAndSkipsGaps() {
        let lyrics = Lyrics(synced: true, lines: [
            LyricLine(atMs: 1_000, text: "one"),
            LyricLine(atMs: 2_000, text: ""),
            LyricLine(atMs: 3_000, text: "three"),
        ])
        #expect(lyrics.cursor(atMs: 500) == (-1, 0))
        #expect(lyrics.cursor(atMs: 1_500) == (0, 2))
        #expect(lyrics.cursor(atMs: 3_500) == (2, 3))
    }
}
