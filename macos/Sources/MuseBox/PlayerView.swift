import MuseBoxCore
import SwiftUI

/// muse-box, one scene. The content layer is the room itself: the album's light,
/// the cover, the titles, the lyrics and Bitka. Floating over it, in Liquid
/// Glass, is only what you press: the view toggles and room light in the rail,
/// and the transport under the cover.
struct PlayerView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    var body: some View {
        // Reduce Transparency wins over the see-through window.
        let translucent = model.glassWindow && !reduceTransparency
        GeometryReader { geometry in
            Room(size: geometry.size, translucent: translucent)
        }
        .background {
            ZStack {
                WindowChrome(translucent: translucent) { visible in
                    FrameClock.shared.show("player", visible)
                }
                if translucent { BackdropBlur() }
            }
        }
        .ignoresSafeArea()
        .frame(minWidth: 760, minHeight: 560)
        .preferredColorScheme(.dark)
    }
}

private struct Room: View {
    @EnvironmentObject private var model: AppModel
    var size: CGSize
    var translucent: Bool

    /// Heights that are not the cover, top to bottom: the rail, then under the
    /// cover the titles, the status line, the transport and the bottom margin.
    private static let rail: CGFloat = 56
    private static let belowCover: CGFloat = 24 + 100 + 12 + 16 + 22 + 60 + 30

    private var wide: Bool { size.width >= 1020 }
    private var lyricsColumn: CGFloat { min(size.width * 0.34, 420) }
    private var hasLyrics: Bool { model.showLyrics && !(model.lyrics?.lines.isEmpty ?? true) }
    private var stageWidth: CGFloat { hasLyrics && wide ? size.width - lyricsColumn : size.width }
    private var compactLyrics: CGFloat { hasLyrics && !wide ? 96 : 0 }

    private var coverSide: CGFloat {
        let room = size.height - Self.rail - Self.belowCover - compactLyrics - 16
        return max(180, min(470, room, stageWidth * 0.62))
    }

    var body: some View {
        let side = coverSide
        let free = size.height - Self.rail - side - Self.belowCover - compactLyrics
        let coverTop = Self.rail + max(0, free / 2)
        let focus = UnitPoint(
            x: stageWidth / 2 / max(size.width, 1),
            y: (coverTop + side / 2) / max(size.height, 1)
        )

        ZStack(alignment: .topLeading) {
            AmbientSurface(driver: model.ambience, style: translucent ? .glass : .window, focus: focus)

            VStack(spacing: 0) {
                Rail()
                Spacer(minLength: 0)
                HStack(spacing: 0) {
                    Stage(side: side)
                        .frame(width: stageWidth)
                    if hasLyrics && wide, let lyrics = model.lyrics {
                        Karaoke(lyrics: lyrics, width: size.width)
                            .frame(width: lyricsColumn)
                            .padding(.bottom, Self.belowCover - 30)
                            .transition(.opacity)
                    }
                }
                if hasLyrics && !wide, let lyrics = model.lyrics {
                    Karaoke(lyrics: lyrics, width: size.width, compact: true)
                        .frame(maxWidth: .infinity)
                        .padding(.top, 16)
                }
                Spacer(minLength: 0)
            }
            .padding(.bottom, 30)

            BitkaStage(size: size)
        }
        .frame(width: size.width, height: size.height)
        .clipped()
        .animation(.smooth(duration: 0.4), value: hasLyrics)
    }
}

/// The cover and everything that belongs to it, in one column.
private struct Stage: View {
    var side: CGFloat

    var body: some View {
        VStack(spacing: 0) {
            CoverView(side: side)
            Titles(width: min(side * 1.3, 560))
                .padding(.top, 24)
            Status()
                .padding(.top, 12)
            Transport()
                .padding(.top, 22)
        }
    }
}

// MARK: - rail

private struct Rail: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        HStack(spacing: 12) {
            Text("muse-box")
                .label(11, tracking: 0.34, weight: .semibold)
                .foregroundStyle(Ink.ink.opacity(0.92))
            Spacer()
            // One container for everything glass up here: neighbours have to
            // sample the same backdrop, and a trouble note morphs in and out.
            GlassGroup(spacing: 10) {
                HStack(spacing: 10) {
                    TroubleNote()
                    // Related toggles share one capsule, like a toolbar group.
                    HStack(spacing: 0) {
                        RailToggle(symbol: "quote.bubble", on: model.showLyrics, label: "Lyrics (L)") {
                            model.showLyrics.toggle()
                        }
                        RailToggle(symbol: "checkerboard.rectangle", on: model.panelFace, label: "1-bit panel face (B)") {
                            model.panelFace.toggle()
                        }
                    }
                    .padding(.horizontal, 4)
                    .glass(Capsule())
                    RoomLightMenu()
                }
            }
            .animation(.smooth(duration: 0.35), value: model.link)
            .animation(.smooth(duration: 0.35), value: model.hearing)
        }
        .padding(.leading, 86) // clear of the traffic lights
        .padding(.trailing, 18)
        .frame(height: 56)
    }
}

/// A symbol in a glass group. Hover is a thin fill on the glass, not more glass.
private struct RailToggle: View {
    var symbol: String
    var on: Bool
    var label: String
    var action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .symbolVariant(on ? .fill : .none)
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(on ? AnyShapeStyle(.primary) : AnyShapeStyle(.secondary))
                .frame(width: 34, height: 32)
                .background(Capsule().fill(.white.opacity(hovering ? 0.12 : 0)).padding(.vertical, 3))
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .animation(.easeOut(duration: 0.14), value: hovering)
        .help(label)
        .accessibilityLabel(label)
        .accessibilityAddTraits(on ? .isSelected : [])
    }
}

/// The room light is a menu: glass buttons open into menus on macOS 26.
private struct RoomLightMenu: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        Menu {
            Picker("Room light", selection: $model.roomLight) {
                ForEach(RoomLightMode.allCases) { mode in
                    Label(mode.menuTitle, systemImage: mode.symbol).tag(mode)
                }
            }
            .pickerStyle(.inline)
            Divider()
            Picker("Brightness", selection: $model.roomStrength) {
                ForEach(RoomLightMode.strengths, id: \.self) { value in
                    Text("\(Int(value * 100))%").tag(value)
                }
            }
            .disabled(model.roomLight == .off)
        } label: {
            Image(systemName: model.roomLight.symbol)
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(model.roomLight == .off ? AnyShapeStyle(.secondary) : AnyShapeStyle(.primary))
                .frame(width: 34, height: 32)
                .contentShape(Capsule())
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .padding(.horizontal, 4)
        .glass(Capsule())
        .help("Room light: \(model.roomLight.title) (R)")
        .accessibilityLabel("Room light, \(model.roomLight.title)")
    }
}

/// Only trouble gets words (the web client's rule). Text sits on its own glass,
/// never sharing a capsule with symbols.
private struct TroubleNote: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        if let note {
            Button(action: note.action) {
                HStack(spacing: 8) {
                    Circle()
                        .fill(note.color)
                        .frame(width: 6, height: 6)
                    Text(note.text)
                        .label(10.5, tracking: 0.14)
                }
                .padding(.horizontal, 14)
                .frame(height: 32)
                .glass(Capsule())
            }
            .buttonStyle(.plain)
            .help(note.help)
            .transition(.opacity)
        }
    }

    private var note: (text: String, color: Color, help: String, action: () -> Void)? {
        switch model.link {
        case .notInstalled:
            return ("Spotify isn't installed", Ink.alert, "muse-box follows the Spotify desktop app", {
                if let url = URL(string: "https://www.spotify.com/download/mac/") { NSWorkspace.shared.open(url) }
            })
        case .notRunning:
            return ("Open Spotify", Ink.amber, "Launch Spotify", model.openSpotify)
        case .denied:
            return ("Allow Spotify access", Ink.alert, "System Settings › Privacy & Security › Automation", model.openAutomationSettings)
        case .asking:
            return ("Waiting for permission", Ink.amber, "Answer the macOS prompt", {})
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

// MARK: - under the cover

private struct Titles: View {
    @EnvironmentObject private var model: AppModel
    var width: CGFloat

    var body: some View {
        VStack(spacing: 0) {
            Text(model.now?.title.nilIfEmpty ?? "Nothing playing")
                .font(.display(min(max(width * 0.09, 30), 48)))
                .foregroundStyle(Ink.ink)
                .lineLimit(2)
                .minimumScaleFactor(0.8)
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

/// What the room hears, as a caption: information, so it stays in the content
/// layer rather than pretending to be a control.
private struct Status: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        Pulse(driver: model.ambience) { frame, _ in
            let phase = phase(frame)
            HStack(spacing: 10) {
                Levels(frame: frame)
                Text(phase.text)
                    .label(10.5, tracking: 0.18)
                    .foregroundStyle(phase.color)
                    .monospacedDigit()
            }
            .frame(height: 16)
        }
        .accessibilityElement(children: .combine)
    }

    private func phase(_ frame: AmbientFrame) -> (text: String, color: Color) {
        guard let now = model.now else { return ("Quiet", Ink.faint) }
        if now.state != .playing { return ("Paused", Ink.faint) }
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
                RoundedRectangle(cornerRadius: 1)
                    .fill(value > 0.12 ? frame.palette.accent.color : Ink.edgeStrong)
                    .frame(width: 4, height: max(2, 14 * value))
            }
        }
        .frame(width: 46, height: 14, alignment: .bottom)
    }
}

/// The transport floats under the cover: three glass keys in one container,
/// with play/pause the only tinted one (the album's accent, as stained glass).
private struct Transport: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        GlassGroup(spacing: 14) {
            HStack(spacing: 14) {
                GlassKey(symbol: "backward.fill", size: 44, label: "Previous track (⌘←)", action: model.previous)
                GlassKey(
                    symbol: model.isPlaying ? "pause.fill" : "play.fill",
                    size: 60,
                    tint: model.palette.accent.color,
                    label: model.isPlaying ? "Pause (Space)" : "Play (Space)",
                    action: model.playPause
                )
                GlassKey(symbol: "forward.fill", size: 44, label: "Next track (⌘→)", action: model.next)
            }
        }
        .disabled(model.link != .connected)
        .opacity(model.link == .connected ? 1 : 0.45)
        .animation(.easeInOut(duration: 1.2), value: model.palette)
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
