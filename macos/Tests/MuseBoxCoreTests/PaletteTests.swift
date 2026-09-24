import Testing
@testable import MuseBoxCore

@Suite struct PaletteTests {
    /// Two solid regions: the bigger one is the background, the other the accent.
    @Test func theHeavierClusterIsTheBackground() {
        var pixels: [UInt8] = []
        for _ in 0..<700 { pixels += [30, 20, 80] }   // deep indigo, 70%
        for _ in 0..<300 { pixels += [232, 102, 58] } // amber, 30%
        let palette = PaletteExtractor.extract(rgb: pixels)
        #expect(palette.background.bytes == [30, 20, 80])
        let accent = palette.accent.hsl
        #expect(accent.h > 10 && accent.h < 25)
    }

    /// Dark covers must not hand the room a near-black accent.
    @Test func theAccentIsClampedForDisplay() {
        let clamped = PaletteExtractor.clampAccent([8, 6, 10])
        let hsl = RGB(bytes: clamped[0], clamped[1], clamped[2]).hsl
        #expect(hsl.l >= 0.355 && hsl.l <= 0.745)
        #expect(hsl.s >= 0.41)
    }

    @Test func anEmptyCoverStillYieldsTwoColours() {
        let palette = PaletteExtractor.extract(rgb: [])
        #expect(palette.background.bytes == [0, 0, 0])
        #expect(palette.accent.hsl.l >= 0.35)
    }

    @Test func oklabRoundTripsAndMixesLikeCSS() {
        let amber = RGB(hex: "#e8663a")!
        let back = RGB(oklab: amber.oklab)
        #expect(back.bytes == amber.bytes)
        let black = RGB(bytes: 0, 0, 0)
        #expect(amber.mixed(with: black, share: 1).bytes == amber.bytes)
        #expect(amber.mixed(with: black, share: 0).bytes == black.bytes)
        #expect(RGB(hex: "#1A1A1A")?.hex == "#1a1a1a")
    }
}
