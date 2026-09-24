import AppKit
import Foundation
import MuseBoxCore
import ScriptingBridge

/// How far we can see into Spotify.
enum SpotifyLink: Equatable {
    case notInstalled
    case notRunning
    /// Running, and we are waiting on the "control Spotify" prompt.
    case asking
    /// Automation was declined. Track changes still arrive (Spotify broadcasts
    /// them), but transport keys and the exact position need the permission.
    case denied
    case connected
}

// MARK: - Scripting Bridge surface (from Spotify.sdef)

@objc enum SpotifyPlayerState: AEKeyword {
    case stopped = 0x6B50_5353 // 'kPSS'
    case playing = 0x6B50_5350 // 'kPSP'
    case paused = 0x6B50_5370 // 'kPSp'
}

@objc protocol SpotifyTrack {
    @objc optional var name: String { get }
    @objc optional var artist: String { get }
    @objc optional var album: String { get }
    /// Milliseconds in practice, whatever the dictionary says.
    @objc optional var duration: Int { get }
    @objc optional var artworkUrl: String { get }
    @objc optional func id() -> String
}

extension SBObject: SpotifyTrack {}

@objc protocol SpotifyApplication {
    @objc optional var currentTrack: SpotifyTrack { get }
    @objc optional var playerState: SpotifyPlayerState { get }
    @objc optional var playerPosition: Double { get }
    @objc optional func setPlayerPosition(_ position: Double)
    @objc optional func playpause()
    @objc optional func nextTrack()
    @objc optional func previousTrack()
}

extension SBApplication: SpotifyApplication {}

// MARK: - Bridge

/// Reads the Spotify app that is already on this Mac. No Web API, no login,
/// no developer app, no 25-user cap: Spotify's own desktop client already
/// publishes what it is playing to the system.
///
/// Two channels, belt and braces:
/// - `com.spotify.client.PlaybackStateChanged`, a distributed notification
///   Spotify posts on every track change, play, pause and seek. Needs no
///   permission at all, but carries no cover URL.
/// - AppleScript (via Scripting Bridge) for the cover, the exact position and
///   the transport keys. Needs the one-time "control Spotify" consent.
final class SpotifyBridge: NSObject, SBApplicationDelegate, @unchecked Sendable {
    static let bundleID = "com.spotify.client"

    /// Both are delivered on the main thread.
    var onSample: ((NowPlaying?) -> Void)?
    var onLink: ((SpotifyLink) -> Void)?

    private let queue = DispatchQueue(label: "box.muse.spotify", qos: .userInitiated)
    // Touched only on `queue`.
    private var application: SBApplication?
    private var applicationPID: pid_t = 0
    private var consent: OSStatus?
    private var lastEventError: Int?
    private var deniedChecks = 0

    // Touched only on main.
    private var timer: Timer?
    private var polling = false
    private var link: SpotifyLink?

    func start() {
        DistributedNotificationCenter.default().addObserver(
            self,
            selector: #selector(playbackChanged(_:)),
            name: SpotifyBroadcast.name,
            object: nil,
            suspensionBehavior: .deliverImmediately
        )
        let workspace = NSWorkspace.shared.notificationCenter
        workspace.addObserver(self, selector: #selector(applicationsChanged(_:)), name: NSWorkspace.didLaunchApplicationNotification, object: nil)
        workspace.addObserver(self, selector: #selector(applicationsChanged(_:)), name: NSWorkspace.didTerminateApplicationNotification, object: nil)

        let timer = Timer(timeInterval: 1, repeats: true) { [weak self] _ in self?.poll() }
        timer.tolerance = 0.2
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
        poll()
    }

    /// Asked of Launch Services at most once a minute: the poll runs every second.
    var isInstalled: Bool {
        if let installed, Date().timeIntervalSince(installed.at) < 60 { return installed.value }
        let value = NSWorkspace.shared.urlForApplication(withBundleIdentifier: Self.bundleID) != nil
        installed = (value, Date())
        return value
    }
    private var installed: (value: Bool, at: Date)?

    private var runningSpotify: NSRunningApplication? {
        NSRunningApplication.runningApplications(withBundleIdentifier: Self.bundleID)
            .first { !$0.isTerminated }
    }

    func launch() {
        guard let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: Self.bundleID) else { return }
        let configuration = NSWorkspace.OpenConfiguration()
        configuration.activates = false
        NSWorkspace.shared.openApplication(at: url, configuration: configuration)
    }

    // MARK: transport

    func playPause() { command { $0.playpause?() } }
    func next() { command { $0.nextTrack?() } }
    func previous() { command { $0.previousTrack?() } }
    func seek(toMs position: Double) { command { $0.setPlayerPosition?(position / 1000) } }

    private func command(_ body: @escaping (SpotifyApplication) -> Void) {
        guard let pid = runningSpotify?.processIdentifier else { return }
        queue.async { [self] in
            guard consent == noErr, let app = scriptable(pid) else { return }
            body(app)
        }
        // Spotify broadcasts the change too; the poll just makes it immediate.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) { [weak self] in self?.poll() }
    }

    // MARK: polling

    @objc private func applicationsChanged(_ note: Notification) {
        let app = note.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication
        guard app?.bundleIdentifier == Self.bundleID else { return }
        poll()
    }

    func poll() {
        guard let spotify = runningSpotify else {
            report(isInstalled ? .notRunning : .notInstalled)
            deliver(nil)
            return
        }
        guard !polling else { return }
        polling = true
        // Until the queue answers (it may be sitting on the consent prompt),
        // say so rather than leaving "Open Spotify" up.
        if link == .notRunning || link == .notInstalled || link == nil { report(.asking) }
        let pid = spotify.processIdentifier
        queue.async { [self] in
            let status = ensureConsent()
            var reading: Reading = .failed
            if status == noErr, let app = scriptable(pid) {
                reading = read(app)
            }
            let link: SpotifyLink = switch status {
            case noErr: .connected
            case -1744: .asking
            case -600: .notRunning
            default: .denied
            }
            DispatchQueue.main.async {
                self.polling = false
                self.report(link)
                // Without consent the broadcast is the only source; keep what it
                // gave us. A failed read (a timeout, Spotify busy) changes nothing.
                switch reading {
                case .playing(let sample): self.deliver(sample)
                case .stopped: self.deliver(nil)
                case .failed: break
                }
            }
        }
    }

    private enum Reading {
        case playing(NowPlaying)
        case stopped
        case failed
    }

    /// Asks once, on this background queue: the prompt blocks until answered.
    /// After a "Don't Allow", re-checks quietly now and then, so flipping the
    /// switch in System Settings takes effect without a relaunch.
    private func ensureConsent() -> OSStatus {
        if consent == noErr { return noErr }
        if consent == -1743 {
            deniedChecks += 1
            guard deniedChecks % 5 == 0 else { return -1743 }
        }
        let target = NSAppleEventDescriptor(bundleIdentifier: Self.bundleID)
        guard let descriptor = target.aeDesc else { return -600 }
        let status = AEDeterminePermissionToAutomateTarget(
            descriptor, AEEventClass(typeWildCard), AEEventID(typeWildCard), consent == nil
        )
        if status != -600 { consent = status }
        return status
    }

    /// Scripting Bridge by PID, never by bundle ID: talking to a closed
    /// Spotify by bundle ID would relaunch it.
    private func scriptable(_ pid: pid_t) -> SpotifyApplication? {
        if application == nil || applicationPID != pid {
            application = SBApplication(processIdentifier: pid)
            application?.delegate = self
            application?.timeout = 120 // ticks: two seconds
            applicationPID = pid
        }
        return application
    }

    private func read(_ app: SpotifyApplication) -> Reading {
        lastEventError = nil
        let state = app.playerState ?? .stopped
        guard lastEventError == nil else { return .failed }
        guard state != .stopped else { return .stopped }
        guard let track = app.currentTrack, let id = track.id?(), !id.isEmpty else {
            return lastEventError == nil ? .stopped : .failed
        }
        let position = (app.playerPosition ?? 0) * 1000
        let stamp = Date()
        let rawDuration = track.duration ?? 0
        let artwork = track.artworkUrl.flatMap { $0.isEmpty ? nil : URL(string: $0) }
        let sample = NowPlaying(
            trackID: id,
            title: track.name ?? "",
            artist: track.artist ?? "",
            album: track.album ?? "",
            durationMs: SpotifyBroadcast.milliseconds(scriptDuration: rawDuration),
            artworkURL: artwork,
            state: state == .playing ? .playing : .paused,
            positionMs: position,
            stamp: stamp
        )
        return lastEventError == nil ? .playing(sample) : .failed
    }

    func eventDidFail(_ event: UnsafePointer<AppleEvent>, withError error: Error) -> Any? {
        lastEventError = (error as NSError).code
        return nil
    }

    // MARK: broadcast

    @objc private func playbackChanged(_ note: Notification) {
        switch SpotifyBroadcast.reading(note.userInfo ?? [:], at: Date()) {
        case .track(let track):
            deliver(track)
            // Pick up the cover and an exact position right away.
            poll()
        case .stopped:
            deliver(nil)
        case .ignored:
            break
        }
    }

    private func deliver(_ sample: NowPlaying?) {
        onSample?(sample)
    }

    private func report(_ next: SpotifyLink) {
        guard next != link else { return }
        link = next
        onLink?(next)
    }
}
