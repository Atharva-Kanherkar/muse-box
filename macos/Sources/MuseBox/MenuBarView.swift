import AppKit
import MuseBoxCore
import SwiftUI

/// The glass mini-player that lives in the menu bar: enough to run the room
/// without the window.
struct MenuBarPlayer: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        content
            .background {
                AmbientSurface(driver: model.ambience, style: AmbientStyle(ground: 0.55), focus: UnitPoint(x: 0.2, y: 0.2))
            }
            .frame(width: 332)
            .preferredColorScheme(.dark)
            .background(VisibilityProbe { FrameClock.shared.show("menu", $0) })
            .onDisappear { FrameClock.shared.show("menu", false) }
    }

    private var content: some View {
        let accent = model.palette.accent.color
        return VStack(alignment: .leading, spacing: 16) {
            HStack(spacing: 14) {
                Pulse(driver: model.ambience) { frame, _ in thumbnail(frame) }
                VStack(alignment: .leading, spacing: 3) {
                    Text(model.now?.title.nilIfEmpty ?? "Nothing playing")
                        .font(.display(22))
                        .foregroundStyle(Ink.ink)
                        .lineLimit(2)
                    Text(model.now?.artist.nilIfEmpty ?? "open Spotify to begin")
                        .font(.mono(12))
                        .foregroundStyle(Ink.muted)
                        .lineLimit(1)
                }
                Spacer(minLength: 0)
            }

            if let now = model.now, now.durationMs > 0 {
                Clock { date in
                    progress(now, date: date, accent: accent)
                }
            }

            Pulse(driver: model.ambience) { frame, _ in
                HStack(spacing: 16) {
                    Spacer()
                    Key(symbol: "backward.end.fill", size: 36, frame: frame, action: model.previous)
                    Key(symbol: model.isPlaying ? "pause.fill" : "play.fill", size: 46, frame: frame, main: true, action: model.playPause)
                    Key(symbol: "forward.end.fill", size: 36, frame: frame, action: model.next)
                    Spacer()
                }
            }
            .disabled(model.link != .connected)
            .opacity(model.link == .connected ? 1 : 0.4)

            section("Room light") {
                HStack(spacing: 6) {
                    ForEach(RoomLightMode.allCases) { mode in
                        Pill(title: mode.title, on: model.roomLight == mode, accent: accent) { model.roomLight = mode }
                    }
                }
                if model.roomLight != .off {
                    HStack(spacing: 10) {
                        Image(systemName: "sun.min").foregroundStyle(Ink.faint)
                        LightSlider(value: $model.roomStrength, range: 0.2...1, accent: accent)
                        Image(systemName: "sun.max").foregroundStyle(Ink.faint)
                    }
                    .font(.system(size: 11))
                }
            }

            section("Room") {
                Switch(title: "Glass window", isOn: $model.glassWindow, accent: accent)
                Switch(title: "Lyrics", isOn: $model.showLyrics, accent: accent)
                Switch(title: "1-bit panel face", isOn: $model.panelFace, accent: accent)
                Switch(title: "Open at login", isOn: Binding(get: { model.launchAtLogin }, set: { model.launchAtLogin = $0 }), accent: accent)
            }

            HStack {
                Button("Open muse-box") {
                    NSApp.activate(ignoringOtherApps: true)
                    openWindow(id: "player")
                }
                .buttonStyle(.plain)
                Spacer()
                Button("Quit") { NSApp.terminate(nil) }
                    .buttonStyle(.plain)
                    .keyboardShortcut("q")
            }
            .label(10.5, tracking: 0.16)
            .foregroundStyle(Ink.muted)
        }
        .padding(18)
    }

    private func progress(_ now: NowPlaying, date: Date, accent: Color) -> some View {
        let fraction = model.progressMs(at: date) / Double(now.durationMs)
        return VStack(spacing: 6) {
            GeometryReader { geometry in
                ZStack(alignment: .leading) {
                    Capsule().fill(Color.white.opacity(0.12))
                    Capsule().fill(accent)
                        .frame(width: geometry.size.width * fraction.clamped(to: 0...1))
                        .shadow(color: accent.opacity(0.6), radius: 4)
                }
            }
            .frame(height: 3)
            HStack {
                Text(formatDuration(model.progressMs(at: date)))
                Spacer()
                Text(formatDuration(Double(now.durationMs)))
            }
            .font(.mono(10))
            .monospacedDigit()
            .foregroundStyle(Ink.faint)
        }
    }

    private func thumbnail(_ frame: AmbientFrame) -> some View {
        ZStack {
            Color.black
            if let cover = model.cover {
                if model.panelFace, let panel = cover.panel {
                    Image(decorative: panel, scale: 1).resizable().interpolation(.none)
                } else {
                    Image(decorative: cover.image, scale: 1).resizable().aspectRatio(contentMode: .fill)
                }
            } else if let idle = model.idleFace {
                Image(decorative: idle, scale: 1).resizable().interpolation(.none)
            }
        }
        .frame(width: 64, height: 64)
        .clipped()
        .overlay(Rectangle().strokeBorder(Ink.edgeStrong, lineWidth: 1))
        .shadow(color: frame.palette.accent.color.opacity(0.35 + 0.2 * frame.pulse * frame.playing), radius: 14)
    }

    private func section<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).label(10, tracking: 0.24).foregroundStyle(Ink.faint)
            content()
        }
        .font(.mono(12))
        .foregroundStyle(Ink.ink)
    }
}

// MARK: - controls in the muse-box voice (the web's `.controlish`)

/// An outlined pill; lit in the accent when chosen.
struct Pill: View {
    var title: String
    var on: Bool
    var accent: Color
    var action: () -> Void

    var body: some View {
        Button(action: action) {
            Text(title)
                .label(10.5, tracking: 0.16)
                .foregroundStyle(on ? Ink.ink : Ink.muted)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 7)
                .background(Capsule().fill(accent.opacity(on ? 0.2 : 0)))
                .overlay(Capsule().strokeBorder(on ? accent : Ink.edgeStrong, lineWidth: 1))
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
    }
}

/// A thin glowing track with a glass knob.
struct LightSlider: View {
    @Binding var value: Double
    var range: ClosedRange<Double>
    var accent: Color

    var body: some View {
        GeometryReader { geometry in
            let width = geometry.size.width
            let fraction = ((value - range.lowerBound) / (range.upperBound - range.lowerBound)).clamped(to: 0...1)
            ZStack(alignment: .leading) {
                Capsule().fill(Color.white.opacity(0.12)).frame(height: 3)
                Capsule().fill(accent).frame(width: max(3, width * fraction), height: 3)
                    .shadow(color: accent.opacity(0.6), radius: 4)
                Circle()
                    .fill(Ink.ink)
                    .frame(width: 14, height: 14)
                    .shadow(color: .black.opacity(0.4), radius: 3, y: 1)
                    .overlay(Circle().strokeBorder(accent, lineWidth: 1.5))
                    .offset(x: (width - 14) * fraction)
            }
            .frame(maxHeight: .infinity)
            .contentShape(Rectangle())
            .gesture(DragGesture(minimumDistance: 0).onChanged { drag in
                let fraction = (drag.location.x / max(width, 1)).clamped(to: 0...1)
                value = range.lowerBound + fraction * (range.upperBound - range.lowerBound)
            })
        }
        .frame(height: 16)
    }
}

/// A small switch that lights up in the accent.
struct Switch: View {
    var title: String
    @Binding var isOn: Bool
    var accent: Color

    var body: some View {
        Button { isOn.toggle() } label: {
            HStack {
                Text(title).font(.mono(12)).foregroundStyle(Ink.ink)
                Spacer()
                ZStack(alignment: isOn ? .trailing : .leading) {
                    Capsule().fill(isOn ? accent.opacity(0.55) : Color.white.opacity(0.1))
                    Capsule().strokeBorder(isOn ? accent : Ink.edgeStrong, lineWidth: 1)
                    Circle().fill(isOn ? Ink.ink : Ink.muted).frame(width: 12, height: 12).padding(3)
                }
                .frame(width: 32, height: 18)
                .animation(.easeOut(duration: 0.16), value: isOn)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

/// The menu bar face: Bitka's visor, as a template image.
enum MenuBarIcon {
    static let image: NSImage = {
        let rows = [
            ".#............#.",
            "###..........###",
            "################",
            "##............##",
            "##..#......#..##",
            "##.#.#....#.#.##",
            "##............##",
            "################",
            ".##############.",
            "..############..",
        ]
        let cell: CGFloat = 1
        let size = NSSize(width: CGFloat(rows[0].count) * cell, height: CGFloat(rows.count) * cell)
        let image = NSImage(size: size, flipped: true) { _ in
            NSColor.black.setFill()
            for (y, row) in rows.enumerated() {
                for (x, character) in row.enumerated() where character == "#" {
                    NSRect(x: CGFloat(x) * cell, y: CGFloat(y) * cell, width: cell, height: cell).fill()
                }
            }
            return true
        }
        image.isTemplate = true
        return image
    }()
}
