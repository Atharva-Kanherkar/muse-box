import Foundation

/// The two 1-bit renderers from `src/image.rs`, so the Mac can show the exact
/// face the shelf panel would: Bayer for texture, Atkinson for detail.
public enum DitherMode: String, CaseIterable, Sendable {
    case bayer
    case atkinson
}

public enum Dither {
    /// Integer Rec. 601 luma, rounded the way the backend rounds it.
    public static func luminance(_ red: UInt8, _ green: UInt8, _ blue: UInt8) -> UInt8 {
        let red = 299 * UInt32(red), green = 587 * UInt32(green), blue = 114 * UInt32(blue)
        return UInt8((red + green + blue + 500) / 1000)
    }

    /// Luma for packed RGB (three bytes per pixel).
    public static func luminance(rgb pixels: [UInt8]) -> [UInt8] {
        stride(from: 0, to: pixels.count - 2, by: 3).map {
            luminance(pixels[$0], pixels[$0 + 1], pixels[$0 + 2])
        }
    }

    /// `true` means ink.
    public static func ink(_ luma: [UInt8], width: Int, height: Int, mode: DitherMode) -> [Bool] {
        switch mode {
        case .bayer: bayer(luma, width: width, height: height)
        case .atkinson: atkinson(luma, width: width, height: height)
        }
    }

    public static func bayer(_ luma: [UInt8], width: Int, height: Int) -> [Bool] {
        let matrix: [[UInt16]] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]]
        var ink = [Bool](repeating: false, count: width * height)
        for y in 0..<height {
            for x in 0..<width {
                let threshold = matrix[y % 4][x % 4] * 16 + 8
                ink[y * width + x] = UInt16(luma[y * width + x]) < threshold
            }
        }
        return ink
    }

    public static func atkinson(_ luma: [UInt8], width: Int, height: Int) -> [Bool] {
        var values = luma.map(Float.init)
        var ink = [Bool](repeating: false, count: width * height)
        let neighbors = [(1, 0), (2, 0), (-1, 1), (0, 1), (1, 1), (0, 2)]
        for y in 0..<height {
            for x in 0..<width {
                let index = y * width + x
                let value = values[index]
                let isInk = value < 128
                ink[index] = isInk
                let error = (value - (isInk ? 0 : 255)) / 8
                for (dx, dy) in neighbors {
                    let nx = x + dx, ny = y + dy
                    guard nx >= 0, nx < width, ny < height else { continue }
                    let neighbor = ny * width + nx
                    values[neighbor] = (values[neighbor] + error).clamped(to: 0...255)
                }
            }
        }
        return ink
    }

    /// Row-major, MSB-first, rows padded to a byte, 1 = ink: the `art.bits` layout.
    public static func pack(_ ink: [Bool], width: Int, height: Int) -> [UInt8] {
        let rowBytes = (width + 7) / 8
        var packed = [UInt8](repeating: 0, count: rowBytes * height)
        for y in 0..<height {
            for x in 0..<width where ink[y * width + x] {
                packed[y * rowBytes + x / 8] |= 1 << (7 - UInt8(x % 8))
            }
        }
        return packed
    }

    /// Paints ink as `ink` and paper as `paper`, RGBA8, ready for a CGImage.
    public static func paint(_ ink: [Bool], ink inkColor: RGB, paper: RGB) -> [UInt8] {
        let on = inkColor.bytes, off = paper.bytes
        var pixels = [UInt8](repeating: 255, count: ink.count * 4)
        for (index, isInk) in ink.enumerated() {
            let source = isInk ? on : off
            pixels[index * 4] = source[0]
            pixels[index * 4 + 1] = source[1]
            pixels[index * 4 + 2] = source[2]
        }
        return pixels
    }
}
