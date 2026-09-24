import AppKit
import MuseBoxCore
import QuartzCore
import SwiftUI

/// One frame of the room's light, sampled by every view that draws it.
struct AmbientFrame {
    var palette: AlbumPalette = .quiet
    /// `--energy`: how hard everything moves, `0...1`.
    var energy: Double = 0.35
    /// Continuous beat count; every long cycle (wander, drift, sweep) is a
    /// multiple of it, exactly like the web's `calc(var(--beat) * n)`.
    var beats: Double = 0
    /// Lands on each beat and decays, `0...1`.
    var pulse: Double = 0
    /// The bass breathing under the glow, `0...1`.
    var breath: Double = 0
    /// The last raw transient, `0...1`.
    var hit: Double = 0
    var beatIndex: Int = 0
    /// Seconds per beat.
    var period: Double = 8
    /// `0` paused/idle ... `1` playing, eased so nothing snaps.
    var playing: Double = 0
    /// True while the Mac can actually hear Spotify.
    var hearing = false
    var tempo: Double?
    var level: Double = 0
    var bars: [Double] = Array(repeating: 0, count: AudioFeatures.barCount)
    /// Monotonic seconds, for motion that is not beat-bound (the cover's sway).
    var time: Double = 0
}

/// Owns the one clock the whole app shares, so the window, the menu bar and
/// the desktop light all breathe in phase.
@MainActor
final class AmbientDriver {
    /// Live analysis, when there is any.
    var features: () -> AudioFeatures? = { nil }
    var isPlaying = false

    private var from = AlbumPalette.quiet
    private var to = AlbumPalette.quiet
    private var blendStart = -10.0
    private let blendSeconds = 1.6
    private var last: Double?
    private var frame = AmbientFrame()

    private var presence = 0.0
    private var syntheticIndex = 0

    var palette: AlbumPalette { to }

    func setPalette(_ palette: AlbumPalette, animated: Bool = true) {
        guard palette != to else { return }
        let now = CACurrentMediaTime()
        from = animated ? currentPalette(at: now) : palette
        to = palette
        blendStart = now
    }

    private func currentPalette(at now: Double) -> AlbumPalette {
        let t = ((now - blendStart) / blendSeconds).clamped(to: 0...1)
        return from.interpolated(to: to, t: t * t * (3 - 2 * t))
    }

    func sample(at now: Double = CACurrentMediaTime()) -> AmbientFrame {
        let dt = (now - (last ?? now)).clamped(to: 0...0.25)
        if let last, now <= last { return frame }
        last = now

        var next = frame
        next.time = now
        next.palette = currentPalette(at: now)

        let live = features()
        let hearing = isPlaying && live.map { !$0.silent } == true
        next.hearing = hearing
        presence += ((hearing ? 1 : 0) - presence) * min(dt / 0.8, 1)
        next.playing += ((isPlaying ? 1 : 0) - next.playing) * min(dt / 0.9, 1)

        // Tempo: heard when we can hear, otherwise a slow drift so the room is
        // never frozen and never pretends to know the beat.
        let target: Double = if let tempo = live?.tempo, hearing {
            60 / tempo
        } else if isPlaying {
            0.6
        } else {
            3
        }
        next.period += (target - next.period) * min(dt / 1.5, 1)
        next.beats += dt / next.period
        next.tempo = hearing ? live?.tempo : nil

        // Pulse: the real beat when heard, a slow breath otherwise.
        let synthetic = Self.keyframe(next.beats / 8, peak: 0.35)
        var livePulse = 0.0, liveBreath = 0.0, liveHit = 0.0
        if let live, hearing {
            let age = live.time - live.lastBeat
            let attack = 0.045
            let decay = (0.32 * next.period).clamped(to: 0.1...0.45)
            livePulse = age < 0 ? 0 : age < attack ? age / attack : exp(-(age - attack) / decay)
            liveBreath = live.bass
            let onsetAge = live.time - live.lastOnset
            liveHit = onsetAge >= 0 ? live.onset * exp(-onsetAge / 0.14) : 0
            next.beatIndex = live.beats
            next.level = live.level
            next.bars = live.bars
            next.energy += (max(live.energy, 0.2) - next.energy) * min(dt / 1.2, 1)
        } else {
            syntheticIndex = Int(next.beats)
            next.beatIndex = syntheticIndex
            next.level *= 0.9
            next.bars = next.bars.map { $0 * 0.9 }
            next.energy += (0.35 - next.energy) * min(dt / 1.5, 1)
        }
        let breathing = Self.keyframe(next.beats / 8, peak: 0.38)
        next.pulse = presence * livePulse + (1 - presence) * synthetic
        next.breath = presence * liveBreath + (1 - presence) * breathing
        next.hit = presence * liveHit

        frame = next
        return next
    }

    /// A CSS keyframe loop that rises to 1 at `peak` of the cycle and falls
    /// back, ease-in-out both ways.
    nonisolated static func keyframe(_ cycles: Double, peak: Double) -> Double {
        let u = cycles - cycles.rounded(.down)
        let x = u < peak ? u / peak : 1 - (u - peak) / (1 - peak)
        return x * x * (3 - 2 * x)
    }

    /// `ease-in-out infinite alternate`: 0 → 1 → 0 over two cycles.
    nonisolated static func pingPong(_ cycles: Double) -> Double {
        let u = (cycles / 2 - (cycles / 2).rounded(.down)) * 2
        let x = u < 1 ? u : 2 - u
        return (1 - cos(.pi * x)) / 2
    }
}
