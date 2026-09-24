import AppKit
import Combine
import MuseBoxCore
import ServiceManagement
import SwiftUI

enum RoomLightMode: String, CaseIterable, Identifiable {
    /// Just the window.
    case off
    /// The album's light washes over your own wallpaper.
    case tint
    /// The whole muse-box room becomes the desktop.
    case scene

    var id: String { rawValue }

    var title: String {
        switch self {
        case .off: "Off"
        case .tint: "Tint"
        case .scene: "Scene"
        }
    }

    /// How the room light menu names it.
    var menuTitle: String {
        switch self {
        case .off: "Off"
        case .tint: "Tint my wallpaper"
        case .scene: "Room as wallpaper"
        }
    }

    var symbol: String {
        switch self {
        case .off: "lightbulb.slash"
        case .tint: "lightbulb.max"
        case .scene: "lightbulb.max.fill"
        }
    }

    var next: RoomLightMode {
        switch self {
        case .off: .tint
        case .tint: .scene
        case .scene: .off
        }
    }

    /// Brightness steps offered by the room light menu.
    static let strengths: [Double] = [0.35, 0.55, 0.7, 0.85, 1]
}

/// Whether the Mac can hear Spotify.
enum Hearing: Equatable {
    case off
    case unsupported
    case listening
    /// Listening, Spotify says it is playing, and there is nothing to hear:
    /// almost always the System Audio Recording permission.
    case silent
    case failed(String)
}

/// The app's one source of truth, the render-document equivalent: what is
/// playing, its cover and palette, its lyrics, and the room's settings.
@MainActor
final class AppModel: ObservableObject {
    static let shared = AppModel()

    @Published private(set) var now: NowPlaying? {
        didSet { FrameClock.shared.playing = now?.state == .playing }
    }
    @Published private(set) var cover: CoverArt?
    @Published private(set) var idleFace: CGImage?
    @Published private(set) var lyrics: Lyrics?
    @Published private(set) var link: SpotifyLink = .notRunning
    @Published private(set) var hearing: Hearing = .off
    /// A track changed and its cover is on the way.
    @Published private(set) var loading = false

    @Published var roomLight: RoomLightMode { didSet { store(roomLight.rawValue, "roomLight") } }
    @Published var roomStrength: Double { didSet { store(roomStrength, "roomStrength") } }
    @Published var glassWindow: Bool { didSet { store(glassWindow, "glassWindow") } }
    @Published var showLyrics: Bool { didSet { store(showLyrics, "showLyrics") } }
    /// Show the cover as the 1-bit shelf panel would.
    @Published var panelFace: Bool { didSet { store(panelFace, "panelFace") } }
    @Published var dither: DitherMode {
        didSet {
            store(dither.rawValue, "dither")
            redither()
        }
    }

    let ambience = AmbientDriver()
    private let spotify = SpotifyBridge()
    private let artwork = ArtworkStore()
    private let lyricsService = LyricsService()
    private let defaults = AppModel.store

    /// Settings. `--demo` keeps its own, so trying it (or taking screenshots)
    /// never touches the real ones.
    static let store: UserDefaults = CommandLine.arguments.contains("--demo")
        ? UserDefaults(suiteName: "com.atharvakanherkar.musebox.demo") ?? .standard
        : .standard

    private var tap: AnyObject?
    private var analyzer: BeatAnalyzer?
    private var lastTapAttempt = Date.distantPast
    private var tapStarting = false
    private var stopListening: DispatchWorkItem?
    private var restartListening: DispatchWorkItem?
    private var silentSince: Date?
    private var coverTask: Task<Void, Never>?
    private var lyricsTask: Task<Void, Never>?
    private var clock: Timer?
    private var idleMinute = -1
    private var started = false

    private init() {
        roomLight = RoomLightMode(rawValue: defaults.string(forKey: "roomLight") ?? "") ?? .tint
        roomStrength = defaults.object(forKey: "roomStrength") as? Double ?? 0.7
        // Opaque by default: the album light is the backdrop the glass controls
        // are made for. See-through is a choice.
        glassWindow = defaults.object(forKey: "glassWindow") as? Bool ?? false
        showLyrics = defaults.object(forKey: "showLyrics") as? Bool ?? true
        panelFace = defaults.bool(forKey: "panelFace")
        dither = DitherMode(rawValue: defaults.string(forKey: "dither") ?? "") ?? .bayer
        ambience.features = { [weak self] in self?.analyzer?.snapshot() }
    }

    private func store(_ value: Any, _ key: String) {
        defaults.set(value, forKey: key)
    }

    var palette: AlbumPalette { ambience.palette }
    var isPlaying: Bool { now?.state == .playing }

    func progressMs(at date: Date = .now) -> Double { now?.progressMs(at: date) ?? 0 }

    // MARK: lifecycle

    func start() {
        guard !started else { return }
        started = true
        spotify.onSample = { [weak self] sample in
            MainActor.assumeIsolated { self?.apply(sample) }
        }
        spotify.onLink = { [weak self] link in
            MainActor.assumeIsolated { self?.link = link }
        }
        spotify.start()
        let clock = Timer(timeInterval: 1, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.tick() }
        }
        RunLoop.main.add(clock, forMode: .common)
        self.clock = clock
        refreshIdleFace(force: true)
    }

    // MARK: transport

    func playPause() {
        guard var track = now else {
            spotify.playPause()
            return
        }
        // Answer the click now; the next reading confirms it.
        track.positionMs = track.progressMs(at: .now)
        track.stamp = .now
        track.state = track.state == .playing ? .paused : .playing
        now = track
        ambience.isPlaying = track.state == .playing
        spotify.playPause()
        updateListening()
    }

    func next() { spotify.next() }
    func previous() { spotify.previous() }

    func seek(to fraction: Double) {
        guard var track = now, track.durationMs > 0 else { return }
        let target = fraction.clamped(to: 0...1) * Double(track.durationMs)
        track.positionMs = target
        track.stamp = .now
        now = track
        spotify.seek(toMs: target)
    }

    func openSpotify() { spotify.launch() }

    func openAutomationSettings() {
        open("x-apple.systempreferences:com.apple.preference.security?Privacy_Automation")
    }

    func openAudioSettings() {
        open("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
    }

    private func open(_ link: String) {
        if let url = URL(string: link) { NSWorkspace.shared.open(url) }
    }

    var launchAtLogin: Bool {
        get { SMAppService.mainApp.status == .enabled }
        set {
            objectWillChange.send()
            do {
                if newValue { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }
            } catch {
                NSLog("muse-box: launch at login failed: \(error.localizedDescription)")
            }
        }
    }

    // MARK: readings

    private func apply(_ sample: NowPlaying?) {
        guard var next = sample else {
            goIdle()
            return
        }
        let current = now
        let changed = current?.trackID != next.trackID
        if !changed, let current {
            if next.artworkURL == nil { next.artworkURL = current.artworkURL }
            if next.durationMs == 0 { next.durationMs = current.durationMs }
            // Keep the running clock unless the sample shows a real seek.
            if current.agrees(with: next) {
                next.positionMs = current.positionMs
                next.stamp = current.stamp
            }
        }
        if next != current { now = next }
        ambience.isPlaying = next.state == .playing

        if changed {
            lyrics = nil
            loading = true
            loadCover(next)
            loadLyrics(next)
        } else if cover == nil, current?.artworkURL == nil, next.artworkURL != nil {
            // The broadcast arrived first with no cover URL; AppleScript has one now.
            loadCover(next)
        }
        updateListening()
    }

    private func goIdle() {
        guard now != nil || cover != nil else { return }
        coverTask?.cancel()
        lyricsTask?.cancel()
        now = nil
        cover = nil
        lyrics = nil
        loading = false
        ambience.isPlaying = false
        ambience.setPalette(.quiet)
        refreshIdleFace(force: true)
        updateListening()
    }

    private func loadCover(_ track: NowPlaying) {
        coverTask?.cancel()
        let id = track.trackID
        let mode = dither
        coverTask = Task { [artwork] in
            let image = await artwork.cover(for: track)
            guard !Task.isCancelled, now?.trackID == id else { return }
            guard let image else {
                cover = nil
                loading = false
                return
            }
            let art = await Task.detached(priority: .userInitiated) {
                CoverArt.make(from: image, dither: mode)
            }.value
            guard !Task.isCancelled, now?.trackID == id else { return }
            withAnimation(.easeInOut(duration: 0.6)) { cover = art }
            ambience.setPalette(art.palette)
            loading = false
        }
    }

    private func loadLyrics(_ track: NowPlaying) {
        lyricsTask?.cancel()
        let id = track.trackID
        lyricsTask = Task { [lyricsService] in
            let found = await lyricsService.lyrics(for: track)
            guard !Task.isCancelled, now?.trackID == id else { return }
            withAnimation(.easeOut(duration: 0.4)) { lyrics = found }
        }
    }

    private func redither() {
        guard let current = cover else {
            refreshIdleFace(force: true)
            return
        }
        let mode = dither
        Task {
            let panel = await Task.detached { CoverArt.panelFace(current.image, palette: current.palette, dither: mode) }.value
            guard cover?.image === current.image else { return }
            cover?.panel = panel
        }
        refreshIdleFace(force: true)
    }

    private func refreshIdleFace(force: Bool = false) {
        let minute = Int(Date().timeIntervalSince1970 / 60)
        guard force || minute != idleMinute else { return }
        idleMinute = minute
        idleFace = CoverArt.idleFace(at: .now, palette: .quiet, dither: dither)
    }

    private func tick() {
        if now == nil { refreshIdleFace() }
        watchSilence()
    }

    // MARK: listening

    private func updateListening() {
        if isPlaying {
            stopListening?.cancel()
            stopListening = nil
            beginListening()
        } else if analyzer != nil, stopListening == nil {
            // Paused for a while: let go of the tap (and macOS's recording dot).
            let work = DispatchWorkItem { [weak self] in
                MainActor.assumeIsolated { self?.endListening() }
            }
            stopListening = work
            DispatchQueue.main.asyncAfter(deadline: .now() + 45, execute: work)
        }
    }

    private func beginListening() {
        guard #available(macOS 14.2, *) else {
            hearing = .unsupported
            return
        }
        let tap = (self.tap as? SpotifyAudioTap) ?? SpotifyAudioTap()
        self.tap = tap
        guard !tap.isRunning, !tapStarting else { return }
        guard Date().timeIntervalSince(lastTapAttempt) > 10 else { return }
        lastTapAttempt = .now
        tapStarting = true
        tap.onTopologyChange = { [weak self] in
            MainActor.assumeIsolated { self?.topologyChanged() }
        }
        // Off the main thread: creating the tap is where macOS may stop to ask
        // for System Audio Recording, and the window must not freeze meanwhile.
        DispatchQueue.global(qos: .userInitiated).async {
            let failure: String?
            do {
                try tap.start()
                failure = nil
            } catch {
                failure = error.localizedDescription
            }
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    self.tapStarting = false
                    if let failure {
                        self.analyzer = nil
                        self.hearing = .failed(failure)
                        NSLog("muse-box: listening failed: \(failure)")
                    } else if self.isPlaying {
                        self.analyzer = tap.analyzer
                        self.silentSince = nil
                        self.hearing = .listening
                    } else {
                        // Paused while we were starting: let go again.
                        tap.stop()
                    }
                }
            }
        }
    }

    private func endListening() {
        stopListening = nil
        if #available(macOS 14.2, *), let tap = tap as? SpotifyAudioTap {
            tap.stop()
        }
        analyzer = nil
        hearing = .off
    }

    /// Spotify started or stopped making sound, or the output device changed.
    private func topologyChanged() {
        restartListening?.cancel()
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated {
                guard let self, !self.tapStarting, #available(macOS 14.2, *), let tap = self.tap as? SpotifyAudioTap, tap.wantsRestart else { return }
                tap.stop()
                self.analyzer = nil
                self.lastTapAttempt = .distantPast
                self.beginListening()
            }
        }
        restartListening = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.6, execute: work)
    }

    private func watchSilence() {
        guard let analyzer else { return }
        let features = analyzer.snapshot()
        guard isPlaying else {
            silentSince = nil
            if hearing == .silent { hearing = .listening }
            return
        }
        if features.silent {
            silentSince = silentSince ?? .now
            if let since = silentSince, Date().timeIntervalSince(since) > 4 { hearing = .silent }
        } else {
            silentSince = nil
            hearing = .listening
        }
    }

    // MARK: stills and demo

    /// Feeds the room from an analyzer other than the tap (the demo's groove).
    func listen(through analyzer: BeatAnalyzer) {
        self.analyzer = analyzer
        hearing = .listening
    }

    /// For `--snapshot` and `--demo`: a fixed room, no Spotify, no audio.
    func stage(_ track: NowPlaying?, cover: CoverArt?, lyrics: Lyrics?, link: SpotifyLink, hearing: Hearing) {
        now = track
        self.cover = cover
        self.lyrics = lyrics
        self.link = link
        self.hearing = hearing
        ambience.isPlaying = track?.state == .playing
        ambience.setPalette(cover?.palette ?? .quiet, animated: false)
        refreshIdleFace(force: true)
    }
}
