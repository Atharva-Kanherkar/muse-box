import AppKit
import ImageIO
import MuseBoxCore
import SwiftUI
import UniformTypeIdentifiers

/// `muse-box --snapshot <dir>` renders the interface to PNGs with a staged
/// track: no Spotify, no audio, no permissions. It is how the screenshots in
/// the README are made, and a quick way to eyeball a design change.
@MainActor
enum Stills {
    static func render(to directory: URL) {
        _ = NSApplication.shared
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let model = AppModel.shared
        let cover = CoverArt.make(from: Fixture.sleeve(), dither: .bayer)
        let stamp = Date()
        let lyrics = Lyrics(synced: true, lines: [
            LyricLine(atMs: 70_000, text: "headlights on the water"),
            LyricLine(atMs: 80_000, text: "and we don't need to say a word"),
            LyricLine(atMs: 90_000, text: "just the hum of the engine, low"),
        ])
        model.stage(Fixture.track(at: stamp), cover: cover, lyrics: lyrics, link: .connected, hearing: .listening)

        var frame = AmbientFrame()
        frame.palette = cover.palette
        frame.energy = 0.62
        frame.beats = 9.4
        frame.pulse = 0.85
        frame.breath = 0.7
        frame.hit = 0.3
        frame.beatIndex = 12
        frame.period = 60 / 118
        frame.playing = 1
        frame.hearing = true
        frame.tempo = 118
        frame.level = 0.7
        frame.bars = [0.9, 0.75, 0.6, 0.72, 0.5, 0.38, 0.3, 0.18]
        frame.time = 2.2

        let at = stamp
        save(PlayerView().environmentObject(model), frame: frame, date: at, size: CGSize(width: 1400, height: 860), to: directory, name: "player")
        save(Room(frame: frame, style: .scene(0.7), size: CGSize(width: 1600, height: 1000)), frame: frame, date: at, size: CGSize(width: 1600, height: 1000), to: directory, name: "room-scene")

        let wallpaper = ProcessInfo.processInfo.environment["MUSEBOX_WALLPAPER"]
            .flatMap { NSImage(contentsOfFile: $0) }
            .flatMap { $0.cgImage(forProposedRect: nil, context: nil, hints: nil) } ?? hills()
        save(
            ZStack {
                Image(decorative: wallpaper, scale: 1).resizable().aspectRatio(contentMode: .fill)
                Room(frame: frame, style: .tint(0.7), size: CGSize(width: 1600, height: 1000))
            },
            frame: frame, date: at, size: CGSize(width: 1600, height: 1000), to: directory, name: "room-tint"
        )
        save(
            Image(decorative: wallpaper, scale: 1).resizable().aspectRatio(contentMode: .fill),
            frame: frame, date: at, size: CGSize(width: 1600, height: 1000), to: directory, name: "room-off"
        )
        save(PlayerView().environmentObject(model), frame: frame, date: at, size: CGSize(width: 980, height: 680), to: directory, name: "player-compact")
        save(MenuBarStill().environmentObject(model), frame: frame, date: at, size: CGSize(width: 332, height: 492), to: directory, name: "menubar")
        model.panelFace = true
        save(PlayerView().environmentObject(model), frame: frame, date: at, size: CGSize(width: 1400, height: 860), to: directory, name: "player-1bit")
        model.panelFace = false

        var idle = frame
        idle.playing = 0
        idle.hearing = false
        idle.tempo = nil
        idle.palette = .quiet
        model.stage(nil, cover: nil, lyrics: nil, link: .connected, hearing: .off)
        save(PlayerView().environmentObject(model), frame: idle, date: at, size: CGSize(width: 1180, height: 760), to: directory, name: "idle")
    }

    /// A desktop's worth of room light, as the room windows draw it.
    private struct Room: View {
        var frame: AmbientFrame
        var style: AmbientStyle
        var size: CGSize

        var body: some View {
            if let image = AmbientLayer.image(frame, size: size, scale: 1, style: style) {
                Image(decorative: image, scale: 1)
            }
        }
    }

    private static func save<V: View>(_ view: V, frame: AmbientFrame, date: Date, size: CGSize, to directory: URL, name: String) {
        let renderer = ImageRenderer(content: view
            .environment(\.snapshotting, true)
            .environment(\.stillFrame, frame)
            .environment(\.stillDate, date)
            .frame(width: size.width, height: size.height))
        renderer.scale = Double(ProcessInfo.processInfo.environment["MUSEBOX_STILL_SCALE"] ?? "") ?? 2
        guard let image = renderer.cgImage else {
            FileHandle.standardError.write(Data("could not render \(name)\n".utf8))
            return
        }
        // JPEG: these are soft gradients, and a PNG of one is megabytes.
        let url = directory.appendingPathComponent("\(name).jpg")
        guard let destination = CGImageDestinationCreateWithURL(url as CFURL, UTType.jpeg.identifier as CFString, 1, nil) else { return }
        CGImageDestinationAddImage(destination, image, [kCGImageDestinationLossyCompressionQuality: 0.86] as CFDictionary)
        CGImageDestinationFinalize(destination)
        FileHandle.standardOutput.write(Data("\(url.path)\n".utf8))
    }

    /// A plain daylight wallpaper, to show what Tint does to someone's desktop.
    private static func hills() -> CGImage {
        let width = 1600, height = 1000
        let context = CGContext(
            data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
            space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        )!
        let sky = CGGradient(colorsSpace: CGColorSpace(name: CGColorSpace.sRGB), colors: [
            CGColor(srgbRed: 0.98, green: 0.84, blue: 0.72, alpha: 1),
            CGColor(srgbRed: 0.62, green: 0.76, blue: 0.9, alpha: 1),
        ] as CFArray, locations: [0, 1])!
        context.drawLinearGradient(sky, start: .zero, end: CGPoint(x: 0, y: height), options: [])
        for (index, shade) in [0.42, 0.3, 0.2].enumerated() {
            context.setFillColor(CGColor(srgbRed: shade * 0.8, green: shade, blue: shade * 1.1, alpha: 1))
            let path = CGMutablePath()
            let base = CGFloat(160 + index * -60)
            path.move(to: CGPoint(x: 0, y: 0))
            for x in stride(from: 0, through: width, by: 20) {
                let wave = sin(Double(x) / Double(220 + index * 90) + Double(index)) * Double(60 + index * 20)
                path.addLine(to: CGPoint(x: CGFloat(x), y: base + CGFloat(wave) + CGFloat(260 - index * 70)))
            }
            path.addLine(to: CGPoint(x: width, y: 0))
            path.closeSubpath()
            context.addPath(path)
            context.fillPath()
        }
        return context.makeImage()!
    }
}

/// The menu bar panel, laid out as it appears under the menu bar.
private struct MenuBarStill: View {
    var body: some View {
        MenuBarPlayer()
            .background(Color(white: 0.12))
            .clipShape(RoundedRectangle(cornerRadius: 14))
    }
}
