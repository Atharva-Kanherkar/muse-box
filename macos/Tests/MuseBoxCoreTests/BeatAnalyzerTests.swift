import Foundation
import Testing
@testable import MuseBoxCore

@Suite struct BeatAnalyzerTests {
    private let rate = 48_000.0

    /// A kick on every beat, a hat on every off-beat, a quiet pad underneath.
    private func track(bpm: Double, seconds: Double) -> [Float] {
        let count = Int(seconds * rate)
        let beat = 60 / bpm
        var generator = SystemRandomNumberGenerator()
        return (0..<count).map { index in
            let t = Double(index) / rate
            let sinceKick = t.truncatingRemainder(dividingBy: beat)
            let sinceHat = (t + beat / 2).truncatingRemainder(dividingBy: beat)
            let kick = 0.8 * sin(2 * .pi * (55 + 90 * exp(-sinceKick / 0.03)) * sinceKick) * exp(-sinceKick / 0.09)
            let hat = 0.08 * Double.random(in: -1...1, using: &generator) * exp(-sinceHat / 0.02)
            let pad = 0.05 * sin(2 * .pi * 220 * t)
            return Float(kick + hat + pad)
        }
    }

    @Test(arguments: [96.0, 120.0, 128.0])
    func findsTheTempoOfASteadyBeat(bpm: Double) throws {
        let analyzer = BeatAnalyzer(sampleRate: rate)
        analyzer.process(track(bpm: bpm, seconds: 14))
        let features = analyzer.snapshot()
        let tempo = try #require(features.tempo)
        #expect(abs(tempo - bpm) / bpm < 0.03, "heard \(tempo) BPM for \(bpm)")
        #expect(!features.silent)
        #expect(features.energy > 0.3)
        // Every beat after the lock-in lands; allow the warm-up.
        let expected = 14 * bpm / 60
        #expect(Double(features.beats) > expected * 0.75 && Double(features.beats) < expected * 1.1)
    }

    @Test func beatsLandOnTheKick() throws {
        let analyzer = BeatAnalyzer(sampleRate: rate)
        analyzer.process(track(bpm: 120, seconds: 12))
        let features = analyzer.snapshot()
        let phase = features.lastBeat.truncatingRemainder(dividingBy: 0.5)
        #expect(min(phase, 0.5 - phase) < 0.06, "last beat \(features.lastBeat) is \(phase)s off the grid")
    }

    @Test func silenceIsSilent() {
        let analyzer = BeatAnalyzer(sampleRate: rate)
        analyzer.process([Float](repeating: 0, count: Int(rate * 4)))
        let features = analyzer.snapshot()
        #expect(features.silent)
        #expect(features.beats == 0)
        #expect(features.tempo == nil)
        #expect(features.energy < 0.01)
    }
}
