import MuseBoxCore
import SwiftUI

/// Bitka, the pixel cat who lives with the box (`web/src/components/Mascot.tsx`):
/// same sprite, same moods. On the Mac she can actually hear the song, so she
/// bobs on the real beat.
struct Bitka: View {
    enum Mood { case vibing, dozing, thinking, petted }

    var mood: Mood
    var accent: Color
    var beatIndex: Int
    var time: Double

    static let cells = CGSize(width: 30, height: 26)

    var body: some View {
        GeometryReader { geometry in
            let cell = geometry.size.width / Self.cells.width
            ZStack(alignment: .topLeading) {
                Image(decorative: Self.body, scale: 1)
                    .resizable()
                    .interpolation(.none)
                Image(decorative: Self.eyes(for: mood), scale: 1)
                    .resizable()
                    .interpolation(.none)
                    .renderingMode(.template)
                    .foregroundStyle(accent)
                    .shadow(color: accent.opacity(0.8), radius: cell * 0.8)
                    .opacity(mood == .thinking ? (Int(time / 0.45) % 2 == 0 ? 1 : 0.25) : 1)
            }
            .shadow(color: .black.opacity(0.5), radius: 11, y: 14)
            .offset(y: offset(height: geometry.size.height))
            .rotationEffect(.degrees(rotation), anchor: .bottom)
            .scaleEffect(x: 1, y: squash, anchor: .bottom)
        }
        .aspectRatio(Self.cells.width / Self.cells.height, contentMode: .fit)
    }

    /// Vibing: a hard two-step bob, one step per beat, like a sprite.
    private func offset(height: CGFloat) -> CGFloat {
        switch mood {
        case .vibing: beatIndex % 2 == 1 ? -0.04 * height : 0
        case .dozing: 2 * (1 - cos(2 * .pi * time / 5)) / 2
        default: 0
        }
    }

    private var squash: CGFloat {
        mood == .dozing ? 1 - 0.015 * (1 - cos(2 * .pi * time / 5)) / 2 : 1
    }

    /// Petted: a happy wiggle.
    private var rotation: Double {
        mood == .petted ? (Int(time / 0.16) % 2 == 0 ? -4 : 4) : 0
    }

    // MARK: sprite

    // . empty · L light pink · P pink · D deep pink · K dark · W cream · C coffee · I ice
    private static let sprite = [
        "......P...KKKKKKKK...P........",
        ".....PLP.KKKKKKKKKK.PDP.......",
        "......LLLLLLLLLLLLLLLL........",
        ".....PPPPPPPPPPPPPPPPPP.......",
        "....PPPPPPPPPPPPPPPPPPPD......",
        "..KKPPPPKKKKKKKKKKKKPPPDKK....",
        "..KKPPPKKKKKKKKKKKKKKPPDKK....",
        "..WKPPPKKKKKKKKKKKKKKPPDKW....",
        "..KKPPPKKKKKKKKKKKKKKPPDKK....",
        "..KKPPPKKKKKKKKKKKKKKPPDKK....",
        "..KKPPPPKKKKKKKKKKKKPPPDKKP...",
        "....PPDDPPPPPPPPPPPPDDPD..P...",
        "....PPPPPPPPPPPPPPPPPPPD.PP...",
        ".....PPPPPPPPPPPPPPPPPP.WWWW..",
        ".........PPPPPPPPPP.....W..W..",
        ".....PPPPPPWPPPPPPPDPPP.WICW..",
        ".....PPPPPPPWPPPPPPD.PPPWCCW..",
        ".....LPPPPPWPPPPPPPD....WCIW..",
        "...D...DPPPPPPPPPPPD....WICW..",
        "...D..D.PPPPPPPPPPPD....WWWW..",
        "....DD...PPPD..PPPD...........",
        ".........PPPD..PPPD...........",
        ".........PPPD..PPPD...........",
        ".........PPPD..PPPD...........",
        "..............................",
        "..............................",
    ]

    private static let palette: [Character: UInt32] = [
        "L": 0xF6D3E0, "P": 0xE8B4C8, "D": 0xC98AA6, "K": 0x14100F,
        "W": 0xF2EDE4, "C": 0x3A2A20, "I": 0xD7ECF5,
    ]

    private static let body: CGImage = render { x, y in
        let row = Array(sprite[y])
        return palette[row[x]]
    }

    private static let happyEyes: Set<[Int]> = [[9, 8], [10, 7], [11, 8], [16, 8], [17, 7], [18, 8]]
    private static let flatEyes: Set<[Int]> = [[9, 8], [10, 8], [11, 8], [16, 8], [17, 8], [18, 8]]
    private static let dotEyes: Set<[Int]> = [[10, 8], [17, 8]]

    private static let happy = eyes(happyEyes)
    private static let flat = eyes(flatEyes)
    private static let dots = eyes(dotEyes)

    static func eyes(for mood: Mood) -> CGImage {
        switch mood {
        case .vibing, .petted: happy
        case .dozing: flat
        case .thinking: dots
        }
    }

    private static func eyes(_ cells: Set<[Int]>) -> CGImage {
        render { x, y in cells.contains([x, y]) ? 0xFFFFFF : nil }
    }

    private static func render(_ color: (Int, Int) -> UInt32?) -> CGImage {
        let width = Int(cells.width), height = Int(cells.height)
        var pixels = [UInt8](repeating: 0, count: width * height * 4)
        for y in 0..<height {
            for x in 0..<width {
                guard let value = color(x, y) else { continue }
                let index = (y * width + x) * 4
                pixels[index] = UInt8(value >> 16 & 0xFF)
                pixels[index + 1] = UInt8(value >> 8 & 0xFF)
                pixels[index + 2] = UInt8(value & 0xFF)
                pixels[index + 3] = 255
            }
        }
        let provider = CGDataProvider(data: Data(pixels) as CFData)!
        return CGImage(
            width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: width * 4,
            space: CGColorSpace(name: CGColorSpace.sRGB)!,
            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
            provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent
        )!
    }
}

/// A pixel heart, for when she is petted.
struct PixelHeart: View {
    var body: some View {
        Canvas { context, size in
            let cell = size.width / 7
            let rows: [(Int, Int, Int)] = [(1, 0, 2), (4, 0, 2), (0, 1, 7), (0, 2, 7), (1, 3, 5), (2, 4, 3), (3, 5, 1)]
            for (x, y, width) in rows {
                context.fill(
                    Path(CGRect(x: CGFloat(x) * cell, y: CGFloat(y) * cell, width: CGFloat(width) * cell, height: cell)),
                    with: .color(Color(red: 0xE8 / 255, green: 0x63 / 255, blue: 0x7F / 255))
                )
            }
        }
        .aspectRatio(7 / 6, contentMode: .fit)
    }
}

/// Bitka's lines, straight from the web client.
enum BitkaLines {
    static let compliments = [
        "oh, THIS one.",
        "good taste tonight~",
        "your library never misses.",
        "this one? respect.",
        "you get it.",
    ]
    static let pets = ["mrrp!", "purrrr.", ":3", "careful, the coffee.", "hehe."]
}
