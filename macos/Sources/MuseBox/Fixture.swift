import AppKit
import MuseBoxCore

/// A staged record for stills and `--demo`: no Spotify, no permissions.
enum Fixture {
    static func track(at stamp: Date, positionMs: Double = 85_000) -> NowPlaying {
        NowPlaying(
            trackID: "spotify:track:staged", title: "Night Drive", artist: "Static Bloom", album: "Low Hum",
            durationMs: 233_000, artworkURL: nil, state: .playing, positionMs: positionMs, stamp: stamp
        )
    }

    static let lyrics: Lyrics = {
        let words = [
            "headlights on the water", "", "and we don't need to say a word",
            "just the hum of the engine, low", "every light is a slow song",
            "", "keep the radio loud", "we're not going home yet",
        ]
        let lines = (0..<60).map { index in
            LyricLine(atMs: 4_000 + index * 4_200, text: words[index % words.count])
        }
        return Lyrics(synced: true, lines: lines)
    }()

    /// A stand-in sleeve in the web screenshot's colours: dusk violet into
    /// sodium orange, two soft moons.
    static func sleeve() -> CGImage {
        let size = 640
        let context = CGContext(
            data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: size * 4,
            space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        )!
        let colors = [
            CGColor(srgbRed: 0.20, green: 0.12, blue: 0.36, alpha: 1),
            CGColor(srgbRed: 0.44, green: 0.22, blue: 0.44, alpha: 1),
            CGColor(srgbRed: 0.93, green: 0.52, blue: 0.36, alpha: 1),
        ]
        let gradient = CGGradient(colorsSpace: CGColorSpace(name: CGColorSpace.sRGB), colors: colors as CFArray, locations: [0, 0.55, 1])!
        context.drawLinearGradient(gradient, start: CGPoint(x: 0, y: CGFloat(size)), end: CGPoint(x: CGFloat(size), y: 0), options: [])
        context.setFillColor(CGColor(srgbRed: 1, green: 0.85, blue: 0.8, alpha: 0.16))
        context.fillEllipse(in: CGRect(x: 70, y: 40, width: 280, height: 280))
        context.setFillColor(CGColor(srgbRed: 0.8, green: 0.6, blue: 0.9, alpha: 0.14))
        context.fillEllipse(in: CGRect(x: 380, y: 360, width: 190, height: 190))
        return context.makeImage()!
    }
}

/// `muse-box --demo`: the whole live app on a staged track, with a synthetic
/// 120 BPM groove fed through the real beat analyzer. For trying the look
/// (and profiling it) without Spotify.
@MainActor
final class Demo {
    private let analyzer = BeatAnalyzer(sampleRate: 48_000)
    private var timer: Timer?
    private var clock = CACurrentMediaTime()
    private var sampleIndex = 0
    private var noise = SystemRandomNumberGenerator()

    func start(_ model: AppModel, fromMs start: Double = 0) {
        let cover = CoverArt.make(from: Fixture.sleeve(), dither: model.dither)
        stageTrack(model, cover: cover, at: start)
        let stage = { [weak self] in self?.stageTrack(model, cover: cover, at: 0) }
        model.listen(through: analyzer)
        let timer = Timer(timeInterval: 0.01, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.feed()
                if model.progressMs() >= 233_000 { stage() }
            }
        }
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
    }

    private func stageTrack(_ model: AppModel, cover: CoverArt, at positionMs: Double) {
        model.stage(Fixture.track(at: .now, positionMs: positionMs), cover: cover, lyrics: Fixture.lyrics, link: .connected, hearing: .listening)
    }

    private func feed() {
        let now = CACurrentMediaTime()
        let count = min(Int((now - clock) * 48_000), 9_600)
        clock = now
        guard count > 0 else { return }
        let beat = 0.5
        var samples = [Float](repeating: 0, count: count)
        for index in 0..<count {
            let t = Double(sampleIndex + index) / 48_000
            let sinceKick = t.truncatingRemainder(dividingBy: beat)
            let sinceHat = (t + beat / 2).truncatingRemainder(dividingBy: beat)
            let kick = 0.8 * sin(2 * .pi * (55 + 90 * exp(-sinceKick / 0.03)) * sinceKick) * exp(-sinceKick / 0.09)
            let hat = 0.08 * Double.random(in: -1...1, using: &noise) * exp(-sinceHat / 0.02)
            let pad = 0.06 * sin(2 * .pi * 220 * t) * (0.6 + 0.4 * sin(2 * .pi * t / 8))
            samples[index] = Float(kick + hat + pad)
        }
        sampleIndex += count
        analyzer.process(samples)
    }
}
