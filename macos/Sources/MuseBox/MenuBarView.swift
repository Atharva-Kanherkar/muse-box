import AppKit
import MuseBoxCore
import SwiftUI

/// The mini player that lives in the menu bar: enough to run the room without
/// the window. It sits on the system's own panel material, so it uses system
/// controls (their knobs turn to Liquid Glass as you drag them) and adds no
/// background or glass of its own: glass on glass is the one thing to avoid.
struct MenuBarPlayer: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        content
            .frame(width: 320)
            .preferredColorScheme(.dark)
            .background(VisibilityProbe { FrameClock.shared.show("menu", $0) })
            .onDisappear { FrameClock.shared.show("menu", false) }
    }

    private var content: some View {
        let accent = model.palette.accent.color
        return VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 14) {
                Pulse(driver: model.ambience) { frame, _ in thumbnail(frame) }
                VStack(alignment: .leading, spacing: 3) {
                    Text(model.now?.title.nilIfEmpty ?? "Nothing playing")
                        .font(.display(22))
                        .foregroundStyle(.primary)
                        .lineLimit(2)
                    Text(model.now?.artist.nilIfEmpty ?? "open Spotify to begin")
                        .font(.mono(12))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 0)
            }

            if let now = model.now, now.durationMs > 0 {
                Clock { date in
                    progress(now, date: date, accent: accent)
                }
            }

            HStack(spacing: 18) {
                Spacer()
                PanelKey(symbol: "backward.fill", size: 34, label: "Previous track", action: model.previous)
                PanelKey(symbol: model.isPlaying ? "pause.fill" : "play.fill", size: 44, tint: accent, label: model.isPlaying ? "Pause" : "Play", action: model.playPause)
                PanelKey(symbol: "forward.fill", size: 34, label: "Next track", action: model.next)
                Spacer()
            }
            .disabled(model.link != .connected)
            .opacity(model.link == .connected ? 1 : 0.4)

            Divider()

            section("Room light") {
                Picker("Room light", selection: $model.roomLight) {
                    ForEach(RoomLightMode.allCases) { mode in
                        Text(mode.title).tag(mode)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                HStack(spacing: 8) {
                    Image(systemName: "sun.min").foregroundStyle(.secondary)
                    Slider(value: $model.roomStrength, in: 0.2...1)
                        .labelsHidden()
                        .controlSize(.small)
                    Image(systemName: "sun.max").foregroundStyle(.secondary)
                }
                .font(.system(size: 11))
                .disabled(model.roomLight == .off)
            }

            section("Room") {
                Setting(title: "Lyrics", isOn: $model.showLyrics)
                Setting(title: "1-bit panel face", isOn: $model.panelFace)
                Setting(title: "See-through window", isOn: $model.glassWindow)
                Setting(title: "Open at login", isOn: Binding(get: { model.launchAtLogin }, set: { model.launchAtLogin = $0 }))
            }

            Divider()

            HStack {
                Button("Open muse-box") {
                    NSApp.activate(ignoringOtherApps: true)
                    openWindow(id: "player")
                }
                Spacer()
                Button("Quit") { NSApp.terminate(nil) }
                    .keyboardShortcut("q")
            }
            .buttonStyle(.plain)
            .label(10.5, tracking: 0.16)
            .foregroundStyle(.secondary)
        }
        .tint(accent)
        .padding(16)
    }

    private func progress(_ now: NowPlaying, date: Date, accent: Color) -> some View {
        let fraction = model.progressMs(at: date) / Double(now.durationMs)
        return VStack(spacing: 6) {
            GeometryReader { geometry in
                ZStack(alignment: .leading) {
                    Capsule().fill(.quaternary)
                    Capsule().fill(accent)
                        .frame(width: geometry.size.width * fraction.clamped(to: 0...1))
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
            .foregroundStyle(.tertiary)
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
            Text(title).label(10, tracking: 0.24).foregroundStyle(.tertiary)
            content()
        }
    }
}

/// A setting row: the label in the muse-box type, a system switch on the
/// trailing edge (the label still names it for VoiceOver).
private struct Setting: View {
    var title: String
    @Binding var isOn: Bool

    var body: some View {
        HStack {
            Text(title).font(.mono(12))
            Spacer()
            Toggle(title, isOn: $isOn)
                .labelsHidden()
                .toggleStyle(.switch)
                .controlSize(.mini)
        }
    }
}

/// A transport key for surfaces that are already a material: a fill and a
/// vibrant symbol rather than more glass. The primary one takes the accent.
private struct PanelKey: View {
    var symbol: String
    var size: CGFloat
    var tint: Color? = nil
    var label: String
    var action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: size * 0.36, weight: .semibold))
                .contentTransition(.symbolEffect(.replace))
                .foregroundStyle(tint == nil ? AnyShapeStyle(.primary) : AnyShapeStyle(.white))
                .frame(width: size, height: size)
                .background(Circle().fill(tint.map { AnyShapeStyle($0) } ?? AnyShapeStyle(.white.opacity(hovering ? 0.16 : 0.08))))
                .brightness(tint != nil && hovering ? 0.06 : 0)
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .animation(.easeOut(duration: 0.14), value: hovering)
        .help(label)
        .accessibilityLabel(label)
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
