// Draws the app icon: Bitka on the muse-box night room, dot screen and all.
//   swift scripts/make-icon.swift build/AppIcon.iconset && iconutil -c icns build/AppIcon.iconset
import AppKit
import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

let output = URL(fileURLWithPath: CommandLine.arguments.dropFirst().first ?? "AppIcon.iconset")
try? FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)

let sRGB = CGColorSpace(name: CGColorSpace.sRGB)!

func color(_ hex: UInt32, _ alpha: CGFloat = 1) -> CGColor {
    CGColor(
        srgbRed: CGFloat(hex >> 16 & 0xFF) / 255, green: CGFloat(hex >> 8 & 0xFF) / 255,
        blue: CGFloat(hex & 0xFF) / 255, alpha: alpha
    )
}

func radial(_ context: CGContext, at center: CGPoint, radius: CGFloat, _ hex: UInt32, _ alpha: CGFloat) {
    let gradient = CGGradient(colorsSpace: sRGB, colors: [
        color(hex, alpha), color(hex, alpha * 0.55), color(hex, 0),
    ] as CFArray, locations: [0, 0.4, 1])!
    context.drawRadialGradient(gradient, startCenter: center, startRadius: 0, endCenter: center, endRadius: radius, options: [])
}

let sprite = [
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
]
let palette: [Character: UInt32] = [
    "L": 0xF6D3E0, "P": 0xE8B4C8, "D": 0xC98AA6, "K": 0x14100F,
    "W": 0xF2EDE4, "C": 0x3A2A20, "I": 0xD7ECF5,
]
let eyes: [(Int, Int)] = [(9, 8), (10, 7), (11, 8), (16, 8), (17, 7), (18, 8)]
let amber: UInt32 = 0xF08A4B

func master() -> CGImage {
    let size = 1024
    let context = CGContext(
        data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: size * 4,
        space: sRGB, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    )!
    let tile = CGRect(x: 100, y: 100, width: 824, height: 824)
    let shape = CGPath(roundedRect: tile, cornerWidth: 186, cornerHeight: 186, transform: nil)

    // Drop shadow under the tile.
    context.saveGState()
    context.setShadow(offset: CGSize(width: 0, height: -12), blur: 28, color: color(0x000000, 0.45))
    context.addPath(shape)
    context.setFillColor(color(0x0B0A09))
    context.fillPath()
    context.restoreGState()

    context.saveGState()
    context.addPath(shape)
    context.clip()
    context.setFillColor(color(0x0B0A09))
    context.fill(tile)

    // The room's light: a plum wash, an amber glow from below, a halo behind her.
    radial(context, at: CGPoint(x: 280, y: 300), radius: 640, 0x5B2E7A, 0.85)
    radial(context, at: CGPoint(x: 820, y: 860), radius: 480, 0x8E3D6B, 0.55)
    radial(context, at: CGPoint(x: 512, y: 60), radius: 560, amber, 0.55)
    radial(context, at: CGPoint(x: 512, y: 470), radius: 360, amber, 0.22)

    // Dot screen.
    context.setFillColor(color(0xF0E8DC, 0.07))
    for y in stride(from: 104, to: 924, by: 12) {
        for x in stride(from: 104, to: 924, by: 12) {
            context.fillEllipse(in: CGRect(x: x, y: y, width: 3, height: 3))
        }
    }

    // Bitka, 20 px cells, standing on the needle.
    let cell = 20
    let origin = CGPoint(x: 512 - CGFloat(sprite[0].count * cell) / 2 + 30, y: 222)
    func fill(_ x: Int, _ y: Int, _ hex: UInt32) {
        let rect = CGRect(
            x: origin.x + CGFloat(x * cell),
            y: origin.y + CGFloat((sprite.count - 1 - y) * cell),
            width: CGFloat(cell), height: CGFloat(cell)
        )
        context.setFillColor(color(hex))
        context.fill(rect)
    }
    context.saveGState()
    context.setShadow(offset: CGSize(width: 0, height: -18), blur: 30, color: color(0x000000, 0.55))
    for (y, row) in sprite.enumerated() {
        for (x, character) in row.enumerated() {
            if let hex = palette[character] { fill(x, y, hex) }
        }
    }
    context.restoreGState()
    context.saveGState()
    context.setShadow(offset: .zero, blur: 22, color: color(amber, 0.95))
    for (x, y) in eyes { fill(x, y, amber) }
    context.restoreGState()

    // The progress needle, glowing.
    context.saveGState()
    context.setShadow(offset: .zero, blur: 16, color: color(amber, 0.9))
    context.setFillColor(color(amber))
    context.fill(CGRect(x: 196, y: 196, width: 390, height: 8))
    context.setFillColor(color(0xFFFFFF, 0.14))
    context.fill(CGRect(x: 586, y: 196, width: 242, height: 8))
    context.restoreGState()

    // Vignette and a lit rim.
    let vignette = CGGradient(colorsSpace: sRGB, colors: [color(0x000000, 0), color(0x000000, 0.45)] as CFArray, locations: [0.55, 1])!
    context.drawRadialGradient(vignette, startCenter: CGPoint(x: 512, y: 560), startRadius: 0, endCenter: CGPoint(x: 512, y: 560), endRadius: 640, options: [])
    context.restoreGState()
    context.addPath(CGPath(roundedRect: tile.insetBy(dx: 1.5, dy: 1.5), cornerWidth: 185, cornerHeight: 185, transform: nil))
    context.setStrokeColor(color(0xFFFFFF, 0.12))
    context.setLineWidth(3)
    context.strokePath()
    return context.makeImage()!
}

func resized(_ image: CGImage, to size: Int) -> CGImage {
    let context = CGContext(
        data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: size * 4,
        space: sRGB, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    )!
    context.interpolationQuality = .high
    context.draw(image, in: CGRect(x: 0, y: 0, width: size, height: size))
    return context.makeImage()!
}

func write(_ image: CGImage, _ name: String) {
    let url = output.appendingPathComponent(name)
    let destination = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil)!
    CGImageDestinationAddImage(destination, image, nil)
    CGImageDestinationFinalize(destination)
}

let icon = master()
for points in [16, 32, 128, 256, 512] {
    write(resized(icon, to: points), "icon_\(points)x\(points).png")
    write(resized(icon, to: points * 2), "icon_\(points)x\(points)@2x.png")
}
print(output.path)
