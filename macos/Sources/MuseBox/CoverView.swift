import MuseBoxCore
import SwiftUI

/// The sleeve, pure content: it sways while the record spins and wears the
/// progress as a needle on its bottom edge. The transport lives under it, in
/// the glass layer, so nothing is painted over the art.
struct CoverView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    var side: CGFloat

    var body: some View {
        // Only the sway and the glow move every frame; the sleeve itself is a
        // separate view SwiftUI can leave alone.
        Pulse(driver: model.ambience) { frame, _ in
            let accent = frame.palette.accent.color
            let sway = reduceMotion ? 0 : (1 - cos(2 * .pi * frame.time / 9)) / 2 * frame.playing
            Sleeve(side: side)
                .background(Rectangle().fill(frame.palette.background.color.opacity(0.55)).padding(-6))
                .shadow(color: .black.opacity(0.7), radius: 34, y: 30)
                .shadow(color: accent.opacity(0.28 + 0.14 * frame.pulse * frame.playing), radius: 56)
                .rotationEffect(.degrees(reduceMotion ? 0 : -0.5 * frame.playing + sway))
                .scaleEffect(1 + 0.012 * sway)
        }
    }
}

private struct Sleeve: View {
    @EnvironmentObject private var model: AppModel
    var side: CGFloat

    var body: some View {
        let accent = model.palette.accent.color
        ZStack(alignment: .bottom) {
            Color.black
            art
                .frame(width: side, height: side)
                .clipped()
                .id(artKey)
                .transition(.opacity)
            Clock { date in
                Needle(fraction: fraction(at: date), accent: accent) { model.seek(to: $0) }
            }
        }
        .frame(width: side, height: side)
        .overlay(Rectangle().strokeBorder(Ink.edgeStrong, lineWidth: 1))
        .animation(.easeInOut(duration: 0.5), value: artKey)
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
}

/// Progress as a thin needle across the cover's bottom edge. It thickens under
/// the pointer; click to seek.
private struct Needle: View {
    var fraction: Double
    var accent: Color
    var seek: (Double) -> Void
    @State private var hovering = false

    var body: some View {
        GeometryReader { geometry in
            ZStack(alignment: .leading) {
                Rectangle().fill(Color.white.opacity(hovering ? 0.22 : 0.12))
                Rectangle()
                    .fill(accent)
                    .frame(width: geometry.size.width * fraction.clamped(to: 0...1))
                    .shadow(color: accent.opacity(0.65), radius: 5)
            }
            .frame(height: hovering ? 6 : 3)
            .frame(maxHeight: .infinity, alignment: .bottom)
            .contentShape(Rectangle())
            .onTapGesture(coordinateSpace: .local) { location in
                seek(location.x / max(geometry.size.width, 1))
            }
        }
        .frame(height: 18)
        .onHover { hovering = $0 }
        .animation(.easeOut(duration: 0.14), value: hovering)
        .help("Click to seek")
    }
}
