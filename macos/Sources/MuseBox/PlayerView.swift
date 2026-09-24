import MuseBoxCore
import SwiftUI

/// muse-box, one scene: the cover is the interface, the album's light fills
/// the room, the lyrics glow beside it, and Bitka keeps time.
struct PlayerView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.stillFrame) private var still

    var body: some View {
        GeometryReader { geometry in
            Room(size: geometry.size)
        }
        .background {
            if still == nil {
                ZStack {
                    WindowChrome(translucent: model.glassWindow) { visible in
                        FrameClock.shared.show("player", visible)
                    }
                    if model.glassWindow { BackdropBlur() }
                }
            }
        }
        .ignoresSafeArea()
        .frame(minWidth: 760, minHeight: 540)
        .preferredColorScheme(.dark)
    }
}

private struct Room: View {
    @EnvironmentObject private var model: AppModel
    var size: CGSize

    private var wide: Bool { size.width >= 1020 }
    private var lyricsColumn: CGFloat { min(size.width * 0.34, 420) }
    private var hasLyrics: Bool { model.showLyrics && !(model.lyrics?.lines.isEmpty ?? true) }

    var body: some View {
        let side = coverSide
        let stageWidth = hasLyrics && wide ? size.width - lyricsColumn : size.width
        let coverCenterY = 56 + (size.height - 56 - 96 - side - 150) / 2 + side / 2
        let focus = UnitPoint(x: (stageWidth / 2) / max(size.width, 1), y: coverCenterY / max(size.height, 1))

        ZStack(alignment: .topLeading) {
            AmbientSurface(driver: model.ambience, style: model.glassWindow ? .glass : .window, focus: focus)

            VStack(spacing: 0) {
                Rail()
                Spacer(minLength: 8)
                HStack(spacing: 0) {
                    Centerpiece(side: side)
                        .frame(width: stageWidth)
                    if hasLyrics && wide, let lyrics = model.lyrics {
                        Karaoke(lyrics: lyrics, width: size.width)
                            .frame(width: lyricsColumn)
                            .transition(.opacity)
                    }
                }
                if hasLyrics && !wide, let lyrics = model.lyrics {
                    Karaoke(lyrics: lyrics, width: size.width, compact: true)
                        .frame(maxWidth: .infinity)
                        .padding(.top, 14)
                }
                Spacer(minLength: 8)
                Dock()
                    .padding(.bottom, 26)
            }

            BitkaStage(size: size)
        }
        .frame(width: size.width, height: size.height)
        .clipped()
    }

    private var coverSide: CGFloat {
        let chrome: CGFloat = 56 + 96 + 150 + (hasLyrics && !wide ? 90 : 0)
        let stage = hasLyrics && wide ? size.width - lyricsColumn : size.width
        return max(180, min(470, size.height - chrome, stage * 0.62))
    }
}

// MARK: - rail

private struct Rail: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        HStack(spacing: 14) {
            Text("muse-box")
                .label(11, tracking: 0.34, weight: .semibold)
                .foregroundStyle(Ink.ink)
            Spacer()
            TroubleNote()
            RailButton(symbol: "text.quote", on: model.showLyrics, help: "Lyrics (L)") { model.showLyrics.toggle() }
            RailButton(symbol: "square.grid.3x3.middle.filled", on: model.panelFace, help: "1-bit panel face (B)") {
                model.panelFace.toggle()
            }
            RailButton(symbol: model.roomLight == .off ? "lightbulb" : "lightbulb.max.fill", on: model.roomLight != .off, help: "Room light: \(model.roomLight.title) (R)") {
                model.roomLight = model.roomLight.next
            }
        }
        .padding(.leading, 86) // clear of the traffic lights
        .padding(.trailing, 28)
        .frame(height: 56)
    }
}

private struct RailButton: View {
    var symbol: String
    var on: Bool
    var help: String
    var action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(on ? Ink.ink : Ink.faint)
                .frame(width: 30, height: 30)
                .glass(Circle())
                .scaleEffect(hovering ? 1.06 : 1)
        }
        .buttonStyle(.plain)
        .help(help)
        .onHover { hovering = $0 }
        .animation(.easeOut(duration: 0.14), value: hovering)
    }
}

/// Only trouble gets words (the web client's rule).
private struct TroubleNote: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        if let note {
            Button(action: note.action) {
                HStack(spacing: 8) {
                    Circle().fill(note.color).frame(width: 6, height: 6)
                        .shadow(color: note.color.opacity(0.6), radius: 3)
                    Text(note.text).label(10.5, tracking: 0.16)
                        .foregroundStyle(Ink.muted)
                }
                .padding(.horizontal, 12)
                .frame(height: 30)
                .glass(Capsule())
            }
            .buttonStyle(.plain)
            .help(note.help)
        }
    }

    private var note: (text: String, color: Color, help: String, action: () -> Void)? {
        switch model.link {
        case .notInstalled:
            return ("Spotify isn't installed", Ink.alert, "muse-box follows the Spotify desktop app", {
                if let url = URL(string: "https://www.spotify.com/download/mac/") { NSWorkspace.shared.open(url) }
            })
        case .notRunning:
            return ("Open Spotify", Color(red: 0.85, green: 0.64, blue: 0.25), "Launch Spotify", model.openSpotify)
        case .denied:
            return ("Allow Spotify access", Ink.alert, "System Settings › Privacy & Security › Automation", model.openAutomationSettings)
        case .asking:
            return ("Waiting for permission", Color(red: 0.85, green: 0.64, blue: 0.25), "Answer the macOS prompt", {})
        case .connected:
            break
        }
        switch model.hearing {
        case .silent:
            return ("Can't hear Spotify", Ink.alert, "Allow System Audio Recording for muse-box", model.openAudioSettings)
        case .failed:
            return ("Listening failed", Ink.alert, "Allow System Audio Recording for muse-box", model.openAudioSettings)
        default:
            return nil
        }
    }
}

// MARK: - cover and titles

private struct Centerpiece: View {
    @EnvironmentObject private var model: AppModel
    var side: CGFloat

    var body: some View {
        VStack(spacing: 26) {
            CoverView(side: side)
            Titles(width: min(side * 1.3, 560))
        }
    }
}

private struct Titles: View {
    @EnvironmentObject private var model: AppModel
    var width: CGFloat

    var body: some View {
        VStack(spacing: 0) {
            Text(model.now?.title.nilIfEmpty ?? "Nothing playing")
                .font(.display(min(max(width * 0.09, 30), 48)))
                .foregroundStyle(Ink.ink)
                .lineLimit(2)
                .multilineTextAlignment(.center)
                .padding(.bottom, 6)
            Text(byline)
                .font(.mono(15))
                .foregroundStyle(Ink.muted)
                .lineLimit(1)
            if let now = model.now, now.durationMs > 0 {
                Clock { date in
                    Text("\(formatDuration(model.progressMs(at: date))) · \(formatDuration(Double(now.durationMs)))")
                        .font(.mono(11))
                        .tracking(11 * 0.12)
                        .monospacedDigit()
                        .foregroundStyle(Ink.faint)
                        .padding(.top, 8)
                }
            }
        }
        .frame(width: width)
        .animation(.easeInOut(duration: 0.4), value: model.now?.trackID)
    }

    private var byline: String {
        if let artist = model.now?.artist.nilIfEmpty { return artist }
        switch model.link {
        case .notRunning, .notInstalled: return "open Spotify to begin"
        default: return "press play in Spotify"
        }
    }
}

/// A little view that follows the track's clock (the times, the needle).
struct Clock<Content: View>: View {
    @EnvironmentObject private var model: AppModel
    @ViewBuilder var content: (Date) -> Content

    var body: some View {
        Pulse(driver: model.ambience) { _, date in
            content(model.isPlaying ? date : (model.now?.stamp ?? date))
        }
    }
}

extension String {
    var nilIfEmpty: String? { isEmpty ? nil : self }
}

// MARK: - dock

/// The muse strip. Where the web client listens for "Muse", the Mac listens
/// to the music: a level meter, what it hears, and the room light.
private struct Dock: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        let accent = model.palette.accent.color
        HStack(spacing: 16) {
            Pulse(driver: model.ambience) { frame, _ in
                HStack(spacing: 16) {
                    Levels(frame: frame)
                    Text(phase(frame).text)
                        .label(11, tracking: 0.14)
                        .foregroundStyle(phase(frame).color)
                        .monospacedDigit()
                        .frame(minWidth: 150)
                }
            }
            Button {
                model.roomLight = model.roomLight.next
            } label: {
                Text("Room light · \(model.roomLight.title)")
                    .label(11, tracking: 0.16)
                    .foregroundStyle(Ink.ink)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 9)
                    .background(Capsule().fill(accent.opacity(model.roomLight == .off ? 0 : 0.16)))
                    .overlay(Capsule().strokeBorder(model.roomLight == .off ? Ink.edgeStrong : accent, lineWidth: 1))
            }
            .buttonStyle(.plain)
            .help("Wash the desktop in the album's light")
            .contextMenu {
                ForEach([0.35, 0.55, 0.7, 0.85, 1.0], id: \.self) { value in
                    Button("\(Int(value * 100))% light") { model.roomStrength = value }
                }
            }
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 10)
        .glass(Capsule(), tint: accent)
        .animation(.easeInOut(duration: 1.2), value: model.palette)
    }

    private func phase(_ frame: AmbientFrame) -> (text: String, color: Color) {
        guard let now = model.now else { return ("Quiet", Ink.muted) }
        if now.state != .playing { return ("Paused", Ink.muted) }
        switch model.hearing {
        case .listening:
            if let tempo = frame.tempo { return ("Listening · \(Int(tempo.rounded())) bpm", Ink.live) }
            return ("Listening", Ink.live)
        case .silent: return ("Can't hear it", Ink.alert)
        case .failed: return ("Deaf", Ink.alert)
        case .unsupported, .off: return ("Playing", Ink.muted)
        }
    }
}

/// `.mini-levels`: eight bars, lit in the accent when they carry signal.
private struct Levels: View {
    var frame: AmbientFrame

    var body: some View {
        HStack(alignment: .bottom, spacing: 2) {
            ForEach(0..<frame.bars.count, id: \.self) { index in
                let value = frame.bars[index] * (frame.hearing ? 1 : 0)
                Rectangle()
                    .fill(value > 0.12 ? frame.palette.accent.color : Ink.edgeStrong)
                    .frame(width: 6, height: max(1, 18 * value))
            }
        }
        .frame(width: 64, height: 18, alignment: .bottom)
    }
}
