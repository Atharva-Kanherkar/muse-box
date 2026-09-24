import Accelerate
import AudioToolbox
import CoreAudio
import Foundation
import MuseBoxCore

/// Listens to Spotify's own output through a Core Audio process tap
/// (macOS 14.2+), so the light moves with what is actually playing.
///
/// Nothing is recorded or leaves the Mac: samples go straight into the beat
/// analyzer and are dropped. The tap is unmuted and private; Spotify keeps
/// playing through your speakers exactly as before. The one-time prompt is
/// macOS's "System Audio Recording" permission (`NSAudioCaptureUsageDescription`).
@available(macOS 14.2, *)
final class SpotifyAudioTap: @unchecked Sendable {
    enum Source: Equatable {
        /// Spotify's audio processes, by object ID.
        case spotify
        /// Spotify by bundle ID (macOS 26), before it has an audio process.
        case spotifyBundle
        /// Everything this Mac plays: the macOS 14/15 fallback while Spotify
        /// has no audio process yet.
        case system
    }

    private(set) var analyzer: BeatAnalyzer?
    private(set) var source: Source = .system
    private(set) var isRunning = false

    private var tapID = AudioObjectID(kAudioObjectUnknown)
    private var aggregateID = AudioObjectID(kAudioObjectUnknown)
    private var procID: AudioDeviceIOProcID?
    private let ioQueue = DispatchQueue(label: "box.muse.audio", qos: .userInteractive)
    private var mono = [Float](repeating: 0, count: 4096)
    private var channels = 2
    private var interleaved = true
    private var tappedProcesses: [AudioObjectID] = []

    /// Called on main when a restart would pick a better source.
    var onTopologyChange: (() -> Void)?
    private var listening = false

    deinit { stop() }

    func start() throws {
        guard !isRunning else { return }
        let spotify = Self.spotifyProcesses()
        let description: CATapDescription
        if !spotify.isEmpty {
            description = CATapDescription(stereoMixdownOfProcesses: spotify)
            source = .spotify
        } else if #available(macOS 26.0, *) {
            // Tahoe can tap by bundle ID, before Spotify has made a sound.
            description = CATapDescription(stereoMixdownOfProcesses: [])
            description.bundleIDs = Self.spotifyBundleIDs
            description.isProcessRestoreEnabled = true
            source = .spotifyBundle
        } else {
            description = CATapDescription(stereoGlobalTapButExcludeProcesses: [])
            source = .system
        }
        tappedProcesses = spotify
        description.uuid = UUID()
        description.name = "muse-box"
        description.isPrivate = true
        description.muteBehavior = .unmuted

        try check(AudioHardwareCreateProcessTap(description, &tapID), "create the audio tap")

        let outputUID = try Self.defaultOutputUID()
        let aggregate: [String: Any] = [
            kAudioAggregateDeviceNameKey: "muse-box listener",
            kAudioAggregateDeviceUIDKey: UUID().uuidString,
            kAudioAggregateDeviceMainSubDeviceKey: outputUID,
            kAudioAggregateDeviceIsPrivateKey: true,
            kAudioAggregateDeviceIsStackedKey: false,
            kAudioAggregateDeviceTapAutoStartKey: true,
            kAudioAggregateDeviceSubDeviceListKey: [[kAudioSubDeviceUIDKey: outputUID]],
            kAudioAggregateDeviceTapListKey: [[
                kAudioSubTapDriftCompensationKey: true,
                kAudioSubTapUIDKey: description.uuid.uuidString,
            ]],
        ]
        do {
            try check(AudioHardwareCreateAggregateDevice(aggregate as CFDictionary, &aggregateID), "create the listening device")

            var format = AudioStreamBasicDescription()
            var size = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
            var address = AudioObjectPropertyAddress(
                mSelector: kAudioTapPropertyFormat,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain
            )
            try check(AudioObjectGetPropertyData(tapID, &address, 0, nil, &size, &format), "read the tap format")
            guard format.mFormatID == kAudioFormatLinearPCM, format.mFormatFlags & kAudioFormatFlagIsFloat != 0 else {
                throw TapError(message: "the tap is not float PCM")
            }
            channels = max(Int(format.mChannelsPerFrame), 1)
            interleaved = format.mFormatFlags & kAudioFormatFlagIsNonInterleaved == 0
            let analyzer = BeatAnalyzer(sampleRate: format.mSampleRate > 0 ? format.mSampleRate : 48_000)
            self.analyzer = analyzer

            try check(AudioDeviceCreateIOProcIDWithBlock(&procID, aggregateID, ioQueue) { [weak self] _, input, _, _, _ in
                self?.consume(input)
            }, "attach to the listening device")
            try check(AudioDeviceStart(aggregateID, procID), "start listening")
        } catch {
            stop()
            throw error
        }
        isRunning = true
        watchTopology()
    }

    func stop() {
        if aggregateID != kAudioObjectUnknown {
            if let procID {
                AudioDeviceStop(aggregateID, procID)
                // Let an in-flight callback finish before the analyzer goes.
                ioQueue.sync {}
                AudioDeviceDestroyIOProcID(aggregateID, procID)
            }
            AudioHardwareDestroyAggregateDevice(aggregateID)
        }
        if tapID != kAudioObjectUnknown {
            AudioHardwareDestroyProcessTap(tapID)
        }
        procID = nil
        aggregateID = AudioObjectID(kAudioObjectUnknown)
        tapID = AudioObjectID(kAudioObjectUnknown)
        isRunning = false
    }

    /// Would a fresh tap hear Spotify better than this one? True once Spotify
    /// has an audio process we are not tapping by ID, including after it quits
    /// and relaunches with new ones.
    var wantsRestart: Bool {
        guard isRunning else { return false }
        let current = Self.spotifyProcesses()
        switch source {
        case .system, .spotifyBundle: return !current.isEmpty
        case .spotify: return Set(current) != Set(tappedProcesses)
        }
    }

    // MARK: IO

    private func consume(_ input: UnsafePointer<AudioBufferList>) {
        guard let analyzer else { return }
        let buffers = UnsafeMutableAudioBufferListPointer(UnsafeMutablePointer(mutating: input))
        guard let first = buffers.first, let firstData = first.mData else { return }

        if interleaved || buffers.count == 1 {
            let perFrame = max(Int(first.mNumberChannels), 1)
            let frames = Int(first.mDataByteSize) / MemoryLayout<Float>.size / perFrame
            guard frames > 0 else { return }
            ensureCapacity(frames)
            let samples = firstData.assumingMemoryBound(to: Float.self)
            mono.withUnsafeMutableBufferPointer { out in
                guard let out = out.baseAddress else { return }
                if perFrame == 1 {
                    out.update(from: samples, count: frames)
                } else {
                    // Average the first two channels.
                    var half: Float = 0.5
                    vDSP_vadd(samples, vDSP_Stride(perFrame), samples + 1, vDSP_Stride(perFrame), out, 1, vDSP_Length(frames))
                    vDSP_vsmul(out, 1, &half, out, 1, vDSP_Length(frames))
                }
                analyzer.process(out, count: frames)
            }
        } else {
            let frames = Int(first.mDataByteSize) / MemoryLayout<Float>.size
            guard frames > 0, let secondData = buffers[1].mData else { return }
            ensureCapacity(frames)
            let left = firstData.assumingMemoryBound(to: Float.self)
            let right = secondData.assumingMemoryBound(to: Float.self)
            mono.withUnsafeMutableBufferPointer { out in
                guard let out = out.baseAddress else { return }
                var half: Float = 0.5
                vDSP_vadd(left, 1, right, 1, out, 1, vDSP_Length(frames))
                vDSP_vsmul(out, 1, &half, out, 1, vDSP_Length(frames))
                analyzer.process(out, count: frames)
            }
        }
    }

    private func ensureCapacity(_ frames: Int) {
        if mono.count < frames { mono = [Float](repeating: 0, count: frames) }
    }

    // MARK: topology

    private func watchTopology() {
        guard !listening else { return }
        listening = true
        for selector in [kAudioHardwarePropertyProcessObjectList, kAudioHardwarePropertyDefaultOutputDevice] {
            var address = AudioObjectPropertyAddress(
                mSelector: selector,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain
            )
            AudioObjectAddPropertyListenerBlock(AudioObjectID(kAudioObjectSystemObject), &address, .main) { [weak self] _, _ in
                self?.onTopologyChange?()
            }
        }
    }

    // MARK: HAL helpers

    static let spotifyBundleIDs = [
        "com.spotify.client",
        "com.spotify.client.helper",
        "com.spotify.client.helper.renderer",
        "com.spotify.client.helper.gpu",
        "com.spotify.client.helper.plugin",
    ]

    /// Audio process objects that belong to Spotify (the app or its helpers).
    static func spotifyProcesses() -> [AudioObjectID] {
        let system = AudioObjectID(kAudioObjectSystemObject)
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyProcessObjectList,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var size: UInt32 = 0
        guard AudioObjectGetPropertyDataSize(system, &address, 0, nil, &size) == noErr, size > 0 else { return [] }
        var objects = [AudioObjectID](repeating: 0, count: Int(size) / MemoryLayout<AudioObjectID>.size)
        guard AudioObjectGetPropertyData(system, &address, 0, nil, &size, &objects) == noErr else { return [] }
        return objects.filter { object in
            bundleID(of: object).map { $0.hasPrefix("com.spotify.client") } ?? false
        }
    }

    private static func bundleID(of process: AudioObjectID) -> String? {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioProcessPropertyBundleID,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var value: Unmanaged<CFString>?
        var size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
        guard AudioObjectGetPropertyData(process, &address, 0, nil, &size, &value) == noErr else { return nil }
        return value?.takeRetainedValue() as String?
    }

    private static func defaultOutputUID() throws -> String {
        let system = AudioObjectID(kAudioObjectSystemObject)
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDefaultSystemOutputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var device = AudioDeviceID(kAudioObjectUnknown)
        var size = UInt32(MemoryLayout<AudioDeviceID>.size)
        guard AudioObjectGetPropertyData(system, &address, 0, nil, &size, &device) == noErr,
              device != kAudioObjectUnknown
        else { throw TapError(message: "there is no output device") }

        address.mSelector = kAudioDevicePropertyDeviceUID
        var uid: Unmanaged<CFString>?
        size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
        guard AudioObjectGetPropertyData(device, &address, 0, nil, &size, &uid) == noErr,
              let value = uid?.takeRetainedValue()
        else { throw TapError(message: "the output device has no UID") }
        return value as String
    }

    private func check(_ status: OSStatus, _ action: String) throws {
        guard status == noErr else { throw TapError(message: "could not \(action) (\(status))") }
    }
}

struct TapError: LocalizedError {
    var message: String
    var errorDescription: String? { message }
}
