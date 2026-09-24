import MuseBoxCore
import SwiftUI

/// Karaoke, not a document: one line at a time in the CRT face, keyed so each
/// change replaces the last, glowing in the album's accent.
struct Karaoke: View {
    @EnvironmentObject private var model: AppModel
    var lyrics: Lyrics
    var width: CGFloat
    var compact = false

    var body: some View {
        Pulse(driver: model.ambience) { frame, date in
            sung(frame, progressMs: model.progressMs(at: date))
        }
    }

    private func sung(_ frame: AmbientFrame, progressMs: Double) -> some View {
        let accent = frame.palette.accent.color
        let size = compact ? 28 : min(max(width * 0.034, 30), 48)
        let phosphor = (frame.beatIndex % 2 == 0 ? frame.pulse : 0.4 * frame.pulse) * frame.playing
        return Group {
            if lyrics.synced {
                let cursor = lyrics.cursor(atMs: progressMs)
                let line = cursor.current >= 0 ? lyrics.lines[cursor.current].text.trimmingCharacters(in: .whitespaces) : ""
                let upcoming = cursor.next < lyrics.lines.count ? lyrics.lines[cursor.next].text : ""
                VStack(alignment: compact ? .center : .leading, spacing: compact ? 10 : 26) {
                    // Keyed on the index: a new line replaces the old, never joins it.
                    Text(line.isEmpty ? " " : line)
                        .font(.retro(size))
                        .lineSpacing(0)
                        .foregroundStyle(accent)
                        .multilineTextAlignment(compact ? .center : .leading)
                        .shadow(color: accent.opacity(0.55 + 0.15 * phosphor), radius: 5 + 1.5 * phosphor)
                        .shadow(color: accent.opacity(0.30 + 0.12 * phosphor), radius: 21 + 7 * phosphor)
                        .frame(minHeight: size * 2.3, alignment: compact ? .center : .bottomLeading)
                        .id(cursor.current)
                        .transition(.lineIn)
                    if !upcoming.isEmpty, !compact {
                        Text(upcoming)
                            .font(.retro(22))
                            .foregroundStyle(Ink.faint)
                            .id("next-\(cursor.next)")
                            .transition(.opacity)
                    }
                }
                .animation(.timingCurve(0.2, 0.7, 0.2, 1, duration: 0.48), value: cursor.current)
            } else {
                // Unsynced lyrics cannot follow the song: quiet, whole, no pretence.
                ScrollView(showsIndicators: false) {
                    VStack(alignment: .leading, spacing: 10) {
                        ForEach(Array(lyrics.lines.enumerated()), id: \.offset) { _, line in
                            Text(line.text).font(.retro(20)).foregroundStyle(Ink.muted)
                        }
                    }
                    .padding(.vertical, 40)
                }
                .mask(LinearGradient(stops: [
                    .init(color: .clear, location: 0),
                    .init(color: .black, location: 0.08),
                    .init(color: .black, location: 0.92),
                    .init(color: .clear, location: 1),
                ], startPoint: .top, endPoint: .bottom))
                .frame(maxHeight: compact ? 120 : 460)
            }
        }
        .padding(.horizontal, compact ? 24 : 44)
        .frame(maxWidth: .infinity, alignment: compact ? .center : .leading)
        .allowsHitTesting(!lyrics.synced)
    }
}

private struct LineIn: ViewModifier {
    var progress: Double

    func body(content: Content) -> some View {
        content
            .opacity(progress)
            .offset(y: 16 * (1 - progress))
            .blur(radius: 5 * (1 - progress))
    }
}

extension AnyTransition {
    /// `line-in`: rise, sharpen, arrive.
    static var lineIn: AnyTransition {
        .asymmetric(
            insertion: .modifier(active: LineIn(progress: 0), identity: LineIn(progress: 1)),
            removal: .opacity.animation(.easeIn(duration: 0.16))
        )
    }
}
