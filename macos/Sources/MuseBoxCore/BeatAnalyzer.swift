import Accelerate
import Foundation

/// What the light needs from the music, published after every analysis hop.
///
/// The web client could only guess the pulse from Spotify's (deprecated)
/// tempo field. The Mac actually hears the track, so this is the README's
/// "device-local beat reactivity" rule done properly: the server-free app
/// supplies its own motion, and the album still supplies the colour.
public struct AudioFeatures: Equatable, Sendable {
    public static let barCount = 8

    /// Loudness against its own recent peak, `0...1`.
    public var level: Double = 0
    /// Band envelopes against their own recent peaks, `0...1`.
    public var bass: Double = 0
    public var mid: Double = 0
    public var treble: Double = 0
    /// Slow "how hard is this track going" measure, `0...1`, the stand-in for
    /// Spotify's old `energy` field.
    public var energy: Double = 0
    /// Log-spaced spectrum for the level meter, `0...1` each.
    public var bars: [Double] = Array(repeating: 0, count: AudioFeatures.barCount)
    /// Beats counted so far, and when the last one landed (analyzer clock, s).
    public var beats: Int = 0
    public var lastBeat: Double = -1_000
    /// The last raw transient and how hard it hit, `0...1`.
    public var lastOnset: Double = -1_000
    public var onset: Double = 0
    /// BPM once the beat is steady enough to trust.
    public var tempo: Double?
    /// Analyzer clock at the moment this snapshot was taken, seconds.
    public var time: Double = 0
    /// True after a stretch with nothing above the noise floor.
    public var silent: Bool = true
    /// dBFS of the most recent hop.
    public var loudness: Double = -120

    public init() {}
}

/// Onset detection, tempo estimation, and a phase-locked beat clock over a
/// mono stream. Feed it from any thread; read `snapshot()` from any other.
public final class BeatAnalyzer: @unchecked Sendable {
    public static let frameSize = 2048
    public static let hopSize = 512

    public let sampleRate: Double
    private let hopSeconds: Double

    // Input: a ring of the last `frameSize` samples.
    private var ring: [Float]
    private var ringIndex = 0
    private var pending = 0
    private var samplesSeen: Int64 = 0

    // FFT scratch, allocated once: the audio thread never allocates.
    private let log2n = vDSP_Length(11)
    private let fft: FFTSetup
    private let window: UnsafeMutablePointer<Float>
    private let frame: UnsafeMutablePointer<Float>
    private let real: UnsafeMutablePointer<Float>
    private let imag: UnsafeMutablePointer<Float>
    private let power: UnsafeMutablePointer<Float>
    private let magnitude: UnsafeMutablePointer<Float>
    private let logMagnitude: UnsafeMutablePointer<Float>
    private let previousLog: UnsafeMutablePointer<Float>
    private var bins: Int { Self.frameSize / 2 }

    private let bassBins: Range<Int>
    private let midBins: Range<Int>
    private let trebleBins: Range<Int>
    private let lowFluxBins: Range<Int>
    private let highFluxBins: Range<Int>
    private let barBins: [Range<Int>]

    // Automatic gain: each band against a slowly decaying peak.
    private var peaks = [Double](repeating: 1e-4, count: 4)
    private var barPeaks = [Double](repeating: 1e-4, count: AudioFeatures.barCount)
    private let peakDecay: Double

    // Onset detection function history.
    private var recent: [Double] = []
    private let recentCapacity: Int
    private var history: [Double]
    private var historyIndex = 0
    private var historyCount = 0
    private var odfBefore = 0.0
    private var odfLast = 0.0
    private var lastOnset = -1_000.0
    private var onsetTimes: [Double] = []
    private var hopsSinceTempo = 0

    // Tempo and the beat clock.
    private var period: Double?
    private var challenger: (period: Double, votes: Int)?
    private var nextBeat = 0.0

    private var quietSeconds = 10.0
    private var state = AudioFeatures()
    private let lock = NSLock()

    public init(sampleRate: Double) {
        self.sampleRate = sampleRate
        hopSeconds = Double(Self.hopSize) / sampleRate
        peakDecay = exp(-hopSeconds / 4)
        recentCapacity = max(Int(1.0 / hopSeconds), 8)
        history = [Double](repeating: 0, count: max(Int(8.0 / hopSeconds), 64))
        ring = [Float](repeating: 0, count: Self.frameSize)

        guard let setup = vDSP_create_fftsetup(log2n, FFTRadix(kFFTRadix2)) else {
            preconditionFailure("vDSP could not allocate a \(Self.frameSize)-point FFT")
        }
        fft = setup

        func buffer(_ count: Int) -> UnsafeMutablePointer<Float> {
            let pointer = UnsafeMutablePointer<Float>.allocate(capacity: count)
            pointer.initialize(repeating: 0, count: count)
            return pointer
        }
        window = buffer(Self.frameSize)
        frame = buffer(Self.frameSize)
        real = buffer(Self.frameSize / 2)
        imag = buffer(Self.frameSize / 2)
        power = buffer(Self.frameSize / 2)
        magnitude = buffer(Self.frameSize / 2)
        logMagnitude = buffer(Self.frameSize / 2)
        previousLog = buffer(Self.frameSize / 2)
        vDSP_hann_window(window, vDSP_Length(Self.frameSize), Int32(vDSP_HANN_NORM))

        let binHz = sampleRate / Double(Self.frameSize)
        let nyquistBin = Self.frameSize / 2
        func range(_ low: Double, _ high: Double) -> Range<Int> {
            let lower = max(1, Int((low / binHz).rounded()))
            let upper = min(nyquistBin, max(lower + 1, Int((high / binHz).rounded())))
            return lower..<upper
        }
        bassBins = range(30, 150)
        midBins = range(250, 2_000)
        trebleBins = range(2_000, 8_000)
        lowFluxBins = range(30, 200)
        highFluxBins = range(200, 6_000)
        barBins = (0..<AudioFeatures.barCount).map { index in
            let low = 45 * pow(11_000 / 45, Double(index) / Double(AudioFeatures.barCount))
            let high = 45 * pow(11_000 / 45, Double(index + 1) / Double(AudioFeatures.barCount))
            return range(low, high)
        }
    }

    deinit {
        vDSP_destroy_fftsetup(fft)
        for pointer in [window, frame, real, imag, power, magnitude, logMagnitude, previousLog] {
            pointer.deallocate()
        }
    }

    public func snapshot() -> AudioFeatures {
        lock.lock()
        defer { lock.unlock() }
        return state
    }

    /// Mono samples, any count.
    public func process(_ samples: UnsafePointer<Float>, count: Int) {
        var offset = 0
        while offset < count {
            let take = min(count - offset, Self.hopSize - pending)
            for index in 0..<take {
                ring[ringIndex] = samples[offset + index]
                ringIndex = (ringIndex + 1) % Self.frameSize
            }
            pending += take
            offset += take
            samplesSeen += Int64(take)
            if pending == Self.hopSize {
                pending = 0
                analyzeHop()
            }
        }
    }

    public func process(_ samples: [Float]) {
        samples.withUnsafeBufferPointer { buffer in
            guard let base = buffer.baseAddress else { return }
            process(base, count: buffer.count)
        }
    }

    // MARK: - analysis

    private func analyzeHop() {
        let now = Double(samplesSeen) / sampleRate
        var next = state
        next.time = now

        // Oldest sample first.
        let tail = Self.frameSize - ringIndex
        ring.withUnsafeBufferPointer { source in
            guard let base = source.baseAddress else { return }
            frame.update(from: base + ringIndex, count: tail)
            (frame + tail).update(from: base, count: ringIndex)
        }

        var rms: Float = 0
        vDSP_rmsqv(frame + (Self.frameSize - Self.hopSize), 1, &rms, vDSP_Length(Self.hopSize))
        let loudness = 20 * log10(max(Double(rms), 1e-7))
        next.loudness = loudness

        spectrum()

        // Bands against their own recent peaks.
        let bands = [mean(bassBins), mean(midBins), mean(trebleBins), Double(rms)]
        var normalized = [Double](repeating: 0, count: 4)
        for index in 0..<4 {
            peaks[index] = max(bands[index], peaks[index] * peakDecay, 1e-4)
            normalized[index] = (bands[index] / peaks[index]).clamped(to: 0...1)
        }
        next.bass = follow(next.bass, normalized[0], attack: 0.55, release: 0.1)
        next.mid = follow(next.mid, normalized[1], attack: 0.5, release: 0.12)
        next.treble = follow(next.treble, normalized[2], attack: 0.6, release: 0.15)
        next.level = follow(next.level, normalized[3], attack: 0.5, release: 0.1)
        for index in 0..<AudioFeatures.barCount {
            let value = mean(barBins[index])
            barPeaks[index] = max(value, barPeaks[index] * peakDecay, 1e-4)
            next.bars[index] = follow(next.bars[index], (value / barPeaks[index]).clamped(to: 0...1), attack: 0.6, release: 0.12)
        }

        // Silence: nothing above the floor for a while means no signal at all
        // (paused, muted, or audio capture not allowed).
        quietSeconds = loudness < -58 ? quietSeconds + hopSeconds : 0
        next.silent = quietSeconds > 1.5
        if next.silent {
            next.bass *= 0.9
            next.level *= 0.9
        }

        // Onset detection function: positive spectral flux, bass-weighted.
        var lowFlux = 0.0, highFlux = 0.0
        for bin in lowFluxBins { lowFlux += Double(max(0, logMagnitude[bin] - previousLog[bin])) }
        for bin in highFluxBins { highFlux += Double(max(0, logMagnitude[bin] - previousLog[bin])) }
        let odf = lowFlux / Double(lowFluxBins.count) + 0.6 * highFlux / Double(highFluxBins.count)
        previousLog.update(from: logMagnitude, count: bins)

        remember(odf)
        detectOnset(current: odf, now: now, loudness: loudness, into: &next)

        hopsSinceTempo += 1
        if hopsSinceTempo >= Int(0.5 / hopSeconds) {
            hopsSinceTempo = 0
            estimateTempo()
        }
        advanceBeatClock(now: now, silent: next.silent, into: &next)

        // Energy: how loud, and how busy.
        onsetTimes.removeAll { now - $0 > 4 }
        let loudPart = ((loudness + 45) / 33).clamped(to: 0...1)
        let busyPart = (Double(onsetTimes.count) / 4 / 4).clamped(to: 0...1)
        let target = next.silent ? 0 : 0.6 * loudPart + 0.4 * busyPart
        next.energy += (target - next.energy) * min(hopSeconds / 1.2, 1)
        next.tempo = period.map { 60 / $0 }

        lock.lock()
        state = next
        lock.unlock()
    }

    private func spectrum() {
        vDSP_vmul(frame, 1, window, 1, frame, 1, vDSP_Length(Self.frameSize))
        var split = DSPSplitComplex(realp: real, imagp: imag)
        frame.withMemoryRebound(to: DSPComplex.self, capacity: bins) { complex in
            vDSP_ctoz(complex, 2, &split, 1, vDSP_Length(bins))
        }
        vDSP_fft_zrip(fft, &split, 1, log2n, FFTDirection(FFT_FORWARD))
        vDSP_zvmags(&split, 1, power, 1, vDSP_Length(bins))
        power[0] = 0 // DC and Nyquist share bin 0 in zrip's packing.
        var count = Int32(bins)
        vvsqrtf(magnitude, power, &count)
        var scale = 1 / Float(Self.frameSize)
        vDSP_vsmul(magnitude, 1, &scale, magnitude, 1, vDSP_Length(bins))
        // log(1 + C·|X|): loud and quiet passages flux alike.
        var gain: Float = 1_000
        vDSP_vsmul(magnitude, 1, &gain, logMagnitude, 1, vDSP_Length(bins))
        vvlog1pf(logMagnitude, logMagnitude, &count)
    }

    private func mean(_ range: Range<Int>) -> Double {
        var sum: Float = 0
        vDSP_sve(magnitude + range.lowerBound, 1, &sum, vDSP_Length(range.count))
        return Double(sum) / Double(range.count)
    }

    private func follow(_ value: Double, _ target: Double, attack: Double, release: Double) -> Double {
        value + (target - value) * (target > value ? attack : release)
    }

    private func remember(_ odf: Double) {
        recent.append(odf)
        if recent.count > recentCapacity { recent.removeFirst(recent.count - recentCapacity) }
        history[historyIndex] = odf
        historyIndex = (historyIndex + 1) % history.count
        historyCount = min(historyCount + 1, history.count)
    }

    /// Peak-picks the previous hop against an adaptive threshold, one hop of
    /// lookahead (~11 ms) to know it was a peak.
    private func detectOnset(current: Double, now: Double, loudness: Double, into next: inout AudioFeatures) {
        defer {
            odfBefore = odfLast
            odfLast = current
        }
        guard recent.count >= recentCapacity / 2 else { return }
        let candidate = odfLast
        let mean = recent.reduce(0, +) / Double(recent.count)
        let variance = recent.reduce(0) { $0 + ($1 - mean) * ($1 - mean) } / Double(recent.count)
        let deviation = variance.squareRoot()
        let threshold = mean + 1.5 * deviation + 0.01
        let at = now - hopSeconds
        guard candidate > threshold,
              candidate >= odfBefore,
              candidate > current,
              at - lastOnset > 0.2,
              loudness > -50
        else { return }

        lastOnset = at
        onsetTimes.append(at)
        next.lastOnset = at
        next.onset = ((candidate - mean) / max(4 * deviation, 1e-6)).clamped(to: 0...1)

        if let period {
            // Nudge the beat clock toward what we just heard.
            let previous = nextBeat - period
            let early = at - nextBeat
            let late = at - previous
            let error = abs(late) < abs(early) ? late : early
            if abs(error) < 0.25 * period {
                nextBeat += 0.3 * error
            }
        } else {
            // No steady tempo yet: every hit is a beat.
            next.beats += 1
            next.lastBeat = at
        }
    }

    /// Autocorrelation of the last ~8 s of onset strength, weighted toward
    /// 120 BPM so a half- or double-time reading loses to the felt pulse.
    private func estimateTempo() {
        let count = historyCount
        guard count >= Int(4 / hopSeconds) else { return }
        var series = [Double](repeating: 0, count: count)
        let start = (historyIndex - count + history.count) % history.count
        for index in 0..<count { series[index] = history[(start + index) % history.count] }
        let average = series.reduce(0, +) / Double(count)
        for index in 0..<count { series[index] -= average }
        let energy = series.reduce(0) { $0 + $1 * $1 } / Double(count)
        guard energy > 1e-9 else { return }

        let minLag = max(Int((60 / 190) / hopSeconds), 2)
        let maxLag = min(Int((60 / 60) / hopSeconds), count / 2)
        guard maxLag > minLag + 2 else { return }
        var scores = [Double](repeating: 0, count: maxLag + 2)
        var correlations = [Double](repeating: 0, count: maxLag + 2)
        series.withUnsafeBufferPointer { values in
            guard let base = values.baseAddress else { return }
            for lag in (minLag - 1)...(maxLag + 1) {
                var sum = 0.0
                vDSP_dotprD(base, 1, base + lag, 1, &sum, vDSP_Length(count - lag))
                let correlation = sum / Double(count - lag) / energy
                let bpm = 60 / (Double(lag) * hopSeconds)
                let octaves = log2(bpm / 120)
                correlations[lag] = correlation
                scores[lag] = correlation * exp(-0.5 * (octaves / 0.9) * (octaves / 0.9))
            }
        }
        var best = minLag
        for lag in minLag...maxLag where scores[lag] > scores[best] { best = lag }
        guard correlations[best] > 0.08 else { return }

        // Parabolic interpolation for a sub-hop period.
        let left = scores[best - 1], middle = scores[best], right = scores[best + 1]
        let curvature = left - 2 * middle + right
        let shift = curvature < 0 ? (0.5 * (left - right) / curvature).clamped(to: -0.5...0.5) : 0
        consider(period: (Double(best) + shift) * hopSeconds)
    }

    private func consider(period candidate: Double) {
        guard let current = period else {
            if let challenger, abs(challenger.period - candidate) / candidate < 0.04 {
                period = candidate
                nextBeat = lastOnset + candidate
                self.challenger = nil
            } else {
                challenger = (candidate, 1)
            }
            return
        }
        let ratio = candidate / current
        if abs(ratio - 1) < 0.05 {
            period = current + (candidate - current) * 0.25
            challenger = nil
        } else if abs(ratio - 2) < 0.08 || abs(ratio - 0.5) < 0.04 {
            // An octave away: the same pulse, keep the one we are locked to.
            challenger = nil
        } else if let challenger, abs(challenger.period - candidate) / candidate < 0.05 {
            if challenger.votes >= 2 {
                period = candidate
                self.challenger = nil
            } else {
                self.challenger = (candidate, challenger.votes + 1)
            }
        } else {
            challenger = (candidate, 1)
        }
    }

    private func advanceBeatClock(now: Double, silent: Bool, into next: inout AudioFeatures) {
        guard let period else { return }
        if silent {
            // Hold the phase; do not tick at nothing.
            nextBeat = max(nextBeat, now)
            return
        }
        if nextBeat < now - period {
            // Re-align after a gap: next beat on the grid of the last hit.
            let behind = ((now - lastOnset) / period).rounded(.up)
            nextBeat = lastOnset + max(behind, 1) * period
        }
        while nextBeat <= now {
            next.beats += 1
            next.lastBeat = nextBeat
            nextBeat += period
        }
    }
}
