import MuseBoxCore
import QuartzCore
import SwiftUI

/// Where Bitka lives in the window: she can be picked up and put down
/// anywhere (the spot persists), petted with a click, and she compliments the
/// odd track.
struct BitkaStage: View {
    @EnvironmentObject private var model: AppModel
    var size: CGSize

    @AppStorage("bitka.x") private var storedX = -1.0
    @AppStorage("bitka.y") private var storedY = -1.0
    @State private var dragStart: CGPoint?
    @State private var dragPoint: CGPoint?
    @State private var bubble: String?
    @State private var bubbleID = 0
    @State private var pettedUntil = 0.0
    @State private var praised: String?

    private var catWidth: CGFloat { min(max(size.width * 0.1, 96), 140) }
    private var catHeight: CGFloat { catWidth * Bitka.cells.height / Bitka.cells.width }

    private var origin: CGPoint {
        if let dragPoint { return dragPoint }
        if storedX >= 0, storedY >= 0 {
            return clamp(CGPoint(x: storedX * size.width, y: storedY * size.height))
        }
        return CGPoint(x: min(max(size.width * 0.03, 14), 40), y: size.height - min(max(size.height * 0.12, 76), 110) - catHeight)
    }

    private func mood(_ frame: AmbientFrame) -> Bitka.Mood {
        if frame.time < pettedUntil { return .petted }
        if model.loading { return .thinking }
        return model.isPlaying ? .vibing : .dozing
    }

    var body: some View {
        ZStack(alignment: .topLeading) {
            Pulse(driver: model.ambience) { frame, _ in
                let mood = mood(frame)
                ZStack(alignment: .topLeading) {
                    Bitka(mood: mood, accent: frame.palette.accent.color, beatIndex: frame.beatIndex, time: frame.time)
                        .frame(width: catWidth, height: catHeight)
                    if mood == .dozing { Snores(time: frame.time).offset(x: catWidth - 16, y: -14) }
                    if mood == .petted {
                        let age = 1 - (pettedUntil - frame.time) / 1.5
                        PixelHeart()
                            .frame(width: 22)
                            .offset(x: catWidth * 0.3, y: -16 - 18 * age)
                            .opacity(age < 0.2 ? age / 0.2 : 1 - (age - 0.2) / 0.8)
                    }
                }
            }
            .frame(width: catWidth, height: catHeight, alignment: .topLeading)

            if let bubble {
                Text(bubble)
                    .font(.retro(17))
                    .foregroundStyle(Ink.ink)
                    .fixedSize()
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
                    .background(UnevenRoundedRectangle(topLeadingRadius: 8, bottomLeadingRadius: 2, bottomTrailingRadius: 8, topTrailingRadius: 8)
                        .fill(Color(red: 20 / 255, green: 16 / 255, blue: 15 / 255).opacity(0.92)))
                    .overlay(UnevenRoundedRectangle(topLeadingRadius: 8, bottomLeadingRadius: 2, bottomTrailingRadius: 8, topTrailingRadius: 8)
                        .strokeBorder(model.palette.accent.color.opacity(0.5), lineWidth: 1))
                    .alignmentGuide(.top) { $0[.bottom] + 10 }
                    .offset(x: catWidth * 0.55)
                    .id(bubbleID)
                    .transition(.opacity.combined(with: .offset(y: 6)))
            }
        }
        .contentShape(Rectangle())
        .gesture(carry)
        .offset(x: origin.x, y: origin.y)
        .animation(.easeOut(duration: 0.32), value: bubbleID)
        .onChange(of: model.now?.trackID) { _, id in compliment(id) }
        .help("Bitka. Pet her, or carry her somewhere.")
        .accessibilityLabel("Bitka, the muse-box cat")
    }

    /// A few points of slop separate a pet from a carry.
    private var carry: some Gesture {
        DragGesture(minimumDistance: 0, coordinateSpace: .global)
            .onChanged { value in
                let start = dragStart ?? origin
                if dragStart == nil { dragStart = start }
                guard hypot(value.translation.width, value.translation.height) > 4 || dragPoint != nil else { return }
                dragPoint = clamp(CGPoint(x: start.x + value.translation.width, y: start.y + value.translation.height))
            }
            .onEnded { _ in
                if let point = dragPoint {
                    storedX = point.x / max(size.width, 1)
                    storedY = point.y / max(size.height, 1)
                } else {
                    pet()
                }
                dragStart = nil
                dragPoint = nil
            }
    }

    private func clamp(_ point: CGPoint) -> CGPoint {
        CGPoint(
            x: point.x.clamped(to: 0...max(0, size.width - catWidth)),
            y: point.y.clamped(to: 40...max(40, size.height - catHeight))
        )
    }

    private func pet() {
        // The driver's clock is CACurrentMediaTime, so this lines up with frame.time.
        pettedUntil = CACurrentMediaTime() + 1.5
        say(stableLine(for: String(Int(Date().timeIntervalSince1970 * 3)), from: BitkaLines.pets), for: 2.2)
    }

    /// One compliment per track, for about every other track.
    private func compliment(_ id: String?) {
        guard let id, model.isPlaying, praised != id, stableHash(id) % 2 == 0 else { return }
        praised = id
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.5) {
            guard model.now?.trackID == id else { return }
            say(stableLine(for: id, from: BitkaLines.compliments), for: 7)
        }
    }

    private func say(_ text: String, for seconds: Double) {
        bubbleID += 1
        let mine = bubbleID
        bubble = text
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds) {
            if bubbleID == mine { bubble = nil }
        }
    }
}

/// z z z, drifting up while she dozes.
private struct Snores: View {
    var time: Double

    var body: some View {
        ZStack(alignment: .bottomLeading) {
            ForEach(0..<3, id: \.self) { index in
                let t = ((time + Double(index) * 1.3) / 4).truncatingRemainder(dividingBy: 1)
                Text("z")
                    .font(.retro([14, 18, 22][index]))
                    .foregroundStyle(Ink.faint)
                    .offset(x: 14 * t, y: -30 * t)
                    .opacity(t < 0.15 ? t / 0.15 * 0.9 : 0.9 * (1 - (t - 0.15) / 0.85))
            }
        }
    }
}
