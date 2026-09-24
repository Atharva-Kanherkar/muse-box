import Foundation
import Testing
@testable import MuseBoxCore

@Suite struct DitherTests {
    @Test func packedOutputIsCeilWidthOverEightTimesHeight() {
        let ink = [Bool](repeating: true, count: 13 * 5)
        let packed = Dither.pack(ink, width: 13, height: 5)
        #expect(packed.count == 2 * 5)
        #expect(packed[0] == 0xFF)
        #expect(packed[1] == 0b1111_1000)
    }

    @Test func blackIsInkAndWhiteIsPaperInBothModes() {
        for mode in DitherMode.allCases {
            #expect(Dither.ink([UInt8](repeating: 0, count: 16), width: 4, height: 4, mode: mode).allSatisfy { $0 })
            #expect(Dither.ink([UInt8](repeating: 255, count: 16), width: 4, height: 4, mode: mode).allSatisfy { !$0 })
        }
    }

    @Test func midGreyBayerIsAnEvenScreen() {
        let ink = Dither.bayer([UInt8](repeating: 128, count: 64), width: 8, height: 8)
        #expect(ink.filter { $0 }.count == 32)
    }

    @Test func theIdleClockFitsAndDrawsDigits() throws {
        let layout = try #require(IdleClock.layout(width: 400, height: 400))
        #expect(layout.digitHeight * 5 >= 400 * 2)
        let at = Date(timeIntervalSince1970: 1_787_132_460) // 09:41 UTC
        let frame = IdleClock.frame(at: at, timeZone: try #require(TimeZone(identifier: "UTC")), width: 400, height: 400)
        #expect(frame.count == 400 * 400)
        #expect(frame.contains(20))
        #expect(IdleClock.layout(width: 4, height: 4) == nil)
    }
}
