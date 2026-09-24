import Foundation

/// The box's idle face, ported from `src/idle.rs`: a seven-segment clock over
/// a Bayer field that drifts once a minute. With nothing playing, this is the
/// cover.
public enum IdleClock {
    public struct Layout: Equatable, Sendable {
        public var x: Int
        public var y: Int
        public var digitWidth: Int
        public var digitHeight: Int
        public var thickness: Int
        public var gap: Int
    }

    /// Luma frame (one byte per pixel) for the minute containing `date`,
    /// digits shown in `timeZone`.
    public static func frame(at date: Date, timeZone: TimeZone = .current, width: Int, height: Int) -> [UInt8] {
        let minute = Int64((date.timeIntervalSince1970 / 60).rounded(.down))
        var frame = patternField(minute: minute, width: width, height: height)
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = timeZone
        let parts = calendar.dateComponents([.hour, .minute], from: date)
        drawClock(&frame, width: width, height: height, hour: parts.hour ?? 0, minute: parts.minute ?? 0)
        return frame
    }

    static func patternField(minute: Int64, width: Int, height: Int) -> [UInt8] {
        let bayer: [[UInt16]] = [
            [0, 32, 8, 40, 2, 34, 10, 42],
            [48, 16, 56, 24, 50, 18, 58, 26],
            [12, 44, 4, 36, 14, 46, 6, 38],
            [60, 28, 52, 20, 62, 30, 54, 22],
            [3, 35, 11, 43, 1, 33, 9, 41],
            [51, 19, 59, 27, 49, 17, 57, 25],
            [15, 47, 7, 39, 13, 45, 5, 37],
            [63, 31, 55, 23, 61, 29, 53, 21],
        ]
        let minute = UInt64(max(minute, 0))
        let phaseX = Int(minute % 8)
        let phaseY = Int((minute / 3) % 8)
        var frame = [UInt8](repeating: 0, count: width * height)
        for y in 0..<height {
            for x in 0..<width {
                let ordered = bayer[(y + phaseY) % 8][(x + phaseX) % 8]
                let drift = UInt16((UInt64(x) * 3 + UInt64(y) * 5 + minute) % 23)
                let value = 178 + (ordered * 51) / 63 + drift
                frame[y * width + x] = UInt8(min(value, 238))
            }
        }
        return frame
    }

    /// Largest layout that fits, or nil when the frame is too small for one.
    public static func layout(width: Int, height: Int) -> Layout? {
        let margin = max(min(width, height) / 32, 2)
        let maxHeight = max(height - margin * 2, 1)
        var digitHeight = max(maxHeight * 3 / 4, 1)
        while true {
            let thickness = min(max(digitHeight / 10, 2), digitHeight)
            let digitWidth = max(digitHeight * 2 / 5, thickness * 2)
            let gap = max(thickness / 2, 2)
            let total = digitWidth * 4 + thickness + gap * 6
            if total <= max(width - margin * 2, 0) {
                return Layout(
                    x: max(width - total, 0) / 2,
                    y: max(height - digitHeight, 0) / 2,
                    digitWidth: digitWidth,
                    digitHeight: digitHeight,
                    thickness: thickness,
                    gap: gap
                )
            }
            if digitHeight == 1 { return nil }
            digitHeight -= 1
        }
    }

    private static let segments: [UInt8] = [
        0b111_1110, 0b011_0000, 0b110_1101, 0b111_1001, 0b011_0011,
        0b101_1011, 0b101_1111, 0b111_0000, 0b111_1111, 0b111_1011,
    ]

    private static func drawClock(_ frame: inout [UInt8], width: Int, height: Int, hour: Int, minute: Int) {
        guard let layout = layout(width: width, height: height) else { return }
        var x = layout.x
        for (index, digit) in [hour / 10, hour % 10, minute / 10, minute % 10].enumerated() {
            if index == 2 {
                let upper = layout.y + layout.digitHeight / 3 - layout.thickness / 2
                let lower = layout.y + layout.digitHeight * 2 / 3 - layout.thickness / 2
                fill(&frame, width, height, x, upper, layout.thickness, layout.thickness)
                fill(&frame, width, height, x, lower, layout.thickness, layout.thickness)
                x += layout.thickness + layout.gap * 2
            }
            drawDigit(&frame, width, height, x, layout.y, digit, layout)
            x += layout.digitWidth + layout.gap
        }
    }

    private static func drawDigit(
        _ frame: inout [UInt8], _ width: Int, _ height: Int,
        _ x: Int, _ y: Int, _ digit: Int, _ layout: Layout
    ) {
        let mask = segments[digit]
        let half = layout.digitHeight / 2
        let across = max(layout.digitWidth - layout.thickness * 2, 0)
        let down = max(half - layout.thickness, 0)
        let rects = [
            (x + layout.thickness, y, across, layout.thickness),
            (x + layout.digitWidth - layout.thickness, y + layout.thickness, layout.thickness, down),
            (x + layout.digitWidth - layout.thickness, y + half, layout.thickness, down),
            (x + layout.thickness, y + layout.digitHeight - layout.thickness, across, layout.thickness),
            (x, y + half, layout.thickness, down),
            (x, y + layout.thickness, layout.thickness, down),
            (x + layout.thickness, y + half - layout.thickness / 2, across, layout.thickness),
        ]
        for (index, rect) in rects.enumerated() where mask & (1 << (6 - index)) != 0 {
            fill(&frame, width, height, rect.0, rect.1, rect.2, rect.3)
        }
    }

    private static func fill(
        _ frame: inout [UInt8], _ width: Int, _ height: Int,
        _ x: Int, _ y: Int, _ w: Int, _ h: Int
    ) {
        let top = max(y, 0), bottom = min(y + h, height)
        let left = max(x, 0), right = min(x + w, width)
        guard top < bottom, left < right else { return }
        for py in top..<bottom {
            for px in left..<right {
                frame[py * width + px] = 20
            }
        }
    }
}
