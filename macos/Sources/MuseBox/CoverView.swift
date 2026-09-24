import MuseBoxCore
import SwiftUI

/// The sleeve: it sways while the record spins, carries the transport behind
/// a scrim on hover, and wears the progress as a needle on its bottom edge.
struct CoverView: View {
    @EnvironmentObject private var model: AppModel
    var side: CGFloat

    var body: some View {
        // Only the sway and the glow move every frame; the sleeve itself (image,
        // glass keys, needle) is a separate view SwiftUI can leave alone.
        Pulse(driver: model.ambience) { frame, _ in
            let accent = frame.palette.accent.color
            let sway = (1 - cos(2 * .pi * frame.time / 9)) / 2 * frame.playing
            Sleeve(side: side)
                .background(Rectangle().fill(frame.palette.background.color.opacity(0.55)).padding(-6))
                .shadow(color: .black.opacity(0.8), radius: 38, y: 34)
                .shadow(color: accent.opacity(0.28 + 0.14 * frame.pulse * frame.playing), radius: 56)
                .rotationEffect(.degrees(-0.5 * frame.playing + sway))
                .scaleEffect(1 + 0.012 * sway)
        }
    }
}

private struct Sleeve: View {
    @EnvironmentObject private var model: AppModel
    var side: CGFloat
    @State private var hovering = false

    var body: some View {
        let accent = model.palette.accent.color
        ZStack(alignment: .bottom) {
            Color.black
            art
                .frame(width: side, height: side)
                .clipped()
                .id(artKey)
                .transition(.opacity)
            veil
            Clock { date in
                Needle(fraction: fraction(at: date), accent: accent) { model.seek(to: $0) }
            }
        }
        .frame(width: side, height: side)
        .overlay(Rectangle().strokeBorder(Ink.edgeStrong, lineWidth: 1))
        .onHover { hovering = $0 }
        .animation(.easeOut(duration: 0.22), value: hovering)
    }

    private func fraction(at date: Date) -> Double {
        guard let now = model.now, now.durationMs > 0 else { return 0 }
        return model.progressMs(at: date) / Double(now.durationMs)
    }

    private var artKey: String {
        "\(model.now?.trackID ?? "idle")-\(model.panelFace)-\(model.cover == nil)"
    }

    @ViewBuilder
    private var art: some View {
        if let cover = model.cover {
            if model.panelFace, let panel = cover.panel {
                Image(decorative: panel, scale: 1).resizable().interpolation(.none)
            } else {
                Image(decorative: cover.image, scale: 1).resizable().interpolation(.high).aspectRatio(contentMode: .fill)
            }
        } else if model.now == nil, let idle = model.idleFace {
            // Nothing playing: the box's own face, the dithered clock.
            Image(decorative: idle, scale: 1).resizable().interpolation(.none)
        } else {
            Text(model.loading ? "tuning in" : "quiet")
                .label(11, tracking: 0.4)
                .foregroundStyle(Ink.faint)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    /// Transport, living on the cover behind a scrim. Built only while hovered.
    @ViewBuilder
    private var veil: some View {
        if hovering && model.link == .connected {
            ZStack(alignment: .bottom) {
                LinearGradient(
                    stops: [
                        .init(color: .black.opacity(0.72), location: 0),
                        .init(color: .black.opacity(0.25), location: 0.3),
                        .init(color: .clear, location: 0.55),
                    ],
                    startPoint: .bottom,
                    endPoint: .top
                )
                Pulse(driver: model.ambience) { frame, _ in
                    HStack(spacing: 18) {
                        Key(symbol: "backward.end.fill", size: 46, frame: frame, action: model.previous)
                        Key(symbol: model.isPlaying ? "pause.fill" : "play.fill", size: 62, frame: frame, main: true, action: model.playPause)
                        Key(symbol: "forward.end.fill", size: 46, frame: frame, action: model.next)
                    }
                    .padding(.bottom, 22)
                }
            }
            .transition(.opacity)
        }
    }
}

/// A round glass key. The main one keeps time while the record spins.
struct Key: View {
    var symbol: String
    var size: CGFloat
    var frame: AmbientFrame
    var main = false
    var action: () -> Void
    @State private var hovering = false

    var body: some View {
        let accent = frame.palette.accent.color
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: size * 0.34, weight: .semibold))
                .foregroundStyle(.white)
                .frame(width: size, height: size)
                .background(Circle().fill(main ? accent.opacity(0.22) : .black.opacity(0.35)))
                .glass(Circle(), tint: main ? accent : .clear, interactive: true)
                .overlay(Circle().strokeBorder(main ? accent : .white.opacity(0.35), lineWidth: 1))
                .shadow(color: main ? accent.opacity((0.25 + frame.energy * 0.3) * frame.pulse * frame.playing) : .clear, radius: 8)
                .scaleEffect(hovering ? 1.06 : 1)
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .animation(.easeOut(duration: 0.14), value: hovering)
    }
}

/// Progress as a thin needle across the cover's bottom edge. Click to seek.
private struct Needle: View {
    var fraction: Double
    var accent: Color
    var seek: (Double) -> Void

    var body: some View {
        GeometryReader { geometry in
            ZStack(alignment: .leading) {
                Rectangle().fill(Color.white.opacity(0.12))
                Rectangle()
                    .fill(accent)
                    .frame(width: geometry.size.width * fraction.clamped(to: 0...1))
                    .shadow(color: accent.opacity(0.65), radius: 5)
            }
            .frame(height: 3)
            .frame(maxHeight: .infinity, alignment: .bottom)
            .contentShape(Rectangle())
            .onTapGesture(coordinateSpace: .local) { location in
                seek(location.x / max(geometry.size.width, 1))
            }
        }
        .frame(height: 14)
    }
}
