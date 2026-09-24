import AppKit
import CoreText
import MuseBoxCore
import SwiftUI

/// The web client's design tokens (`web/src/styles.css`), one to one.
enum Ink {
    static let ground = Color(red: 0x0B / 255, green: 0x0A / 255, blue: 0x09 / 255)
    static let edge = Color(red: 240 / 255, green: 232 / 255, blue: 220 / 255).opacity(0.1)
    static let edgeStrong = Color(red: 240 / 255, green: 232 / 255, blue: 220 / 255).opacity(0.2)
    static let ink = Color(red: 0xF2 / 255, green: 0xED / 255, blue: 0xE4 / 255)
    static let muted = Color(red: 0xA4 / 255, green: 0x9E / 255, blue: 0x96 / 255)
    static let faint = Color(red: 0x6D / 255, green: 0x68 / 255, blue: 0x62 / 255)
    static let alert = Color(red: 0xE8 / 255, green: 0x62 / 255, blue: 0x3C / 255)
    static let live = Color(red: 0x7F / 255, green: 0xD4 / 255, blue: 0xA3 / 255)
}

extension RGB {
    var color: Color { Color(.sRGB, red: r, green: g, blue: b, opacity: 1) }
    var nsColor: NSColor { NSColor(srgbRed: r, green: g, blue: b, alpha: 1) }
}

/// IBM Plex Mono for the interface, Instrument Serif for titles, VT323 for
/// the CRT lyrics: the same three faces as the web client, bundled (OFL).
enum Typeface {
    static func register() {
        var directories: [URL] = []
        if let bundled = Bundle.main.resourceURL?.appendingPathComponent("Fonts") {
            directories.append(bundled)
        }
        // `swift run` has no bundle; find the fonts next to the sources.
        directories.append(
            URL(fileURLWithPath: #filePath)
                .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
                .appendingPathComponent("Resources/Fonts")
        )
        for directory in directories {
            let fonts = (try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil))?
                .filter { $0.pathExtension == "ttf" } ?? []
            guard !fonts.isEmpty else { continue }
            CTFontManagerRegisterFontURLs(fonts as CFArray, .process, true, nil)
            return
        }
    }
}

extension Font {
    static func display(_ size: CGFloat) -> Font { .custom("InstrumentSerif-Regular", size: size) }
    static func retro(_ size: CGFloat) -> Font { .custom("VT323-Regular", size: size) }

    static func mono(_ size: CGFloat, _ weight: Weight = .regular) -> Font {
        switch weight {
        case .semibold, .bold, .heavy, .black: .custom("IBMPlexMono-SemiBold", size: size)
        default: .custom("IBMPlexMono-Regular", size: size)
        }
    }
}

extension View {
    /// The rail/label style: tiny, uppercase, tracked out.
    func label(_ size: CGFloat = 11, tracking: CGFloat = 0.16, weight: Font.Weight = .regular) -> some View {
        font(.mono(size, weight)).tracking(size * tracking).textCase(.uppercase)
    }
}
