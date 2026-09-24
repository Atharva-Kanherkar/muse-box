import AppKit
import Combine
import MuseBoxCore
import SwiftUI

/// The album's light on the desktop itself: one borderless, click-through
/// window per display, just above the wallpaper and below the desktop icons.
/// Every translucent surface on the Mac (the Dock, the menu bar, sidebars,
/// muse-box's own glass) picks the colour up from there.
@MainActor
final class RoomLightController {
    private let model: AppModel
    private var windows: [NSWindow] = []
    private var subscriptions: Set<AnyCancellable> = []

    init(model: AppModel) {
        self.model = model
    }

    func start() {
        model.$roomLight
            .combineLatest(model.$roomStrength)
            .removeDuplicates { $0 == $1 }
            .sink { [weak self] mode, strength in self?.apply(mode: mode, strength: strength) }
            .store(in: &subscriptions)
        NotificationCenter.default.publisher(for: NSApplication.didChangeScreenParametersNotification)
            .sink { [weak self] _ in self?.rebuild() }
            .store(in: &subscriptions)
    }

    private func apply(mode: RoomLightMode, strength: Double) {
        guard mode != .off else {
            windows.forEach { $0.orderOut(nil) }
            windows = []
            return
        }
        if windows.count != NSScreen.screens.count { build() }
        let style: AmbientStyle = mode == .scene ? .scene(strength) : .tint(strength)
        for window in windows {
            (window.contentView as? AmbientView)?.ambient.look = style
        }
    }

    private func rebuild() {
        windows.forEach { $0.orderOut(nil) }
        windows = []
        apply(mode: model.roomLight, strength: model.roomStrength)
    }

    private func build() {
        windows.forEach { $0.orderOut(nil) }
        let driver = model.ambience
        windows = NSScreen.screens.map { screen in
            let window = RoomWindow(screen: screen)
            let view = AmbientView(frame: NSRect(origin: .zero, size: screen.frame.size))
            view.sample = { [weak driver] in
                MainActor.assumeIsolated { driver?.sample() ?? AmbientFrame() }
            }
            view.autoresizingMask = [.width, .height]
            window.contentView = view
            window.orderFrontRegardless()
            return window
        }
    }
}

/// Borderless, click-through, on every Space, never in the window cycle.
final class RoomWindow: NSWindow {
    init(screen: NSScreen) {
        super.init(contentRect: screen.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        setFrame(screen.frame, display: false)
        // Above the wallpaper, under the icons.
        level = NSWindow.Level(rawValue: Int(CGWindowLevelForKey(.desktopIconWindow)) - 1)
        collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle, .fullScreenNone]
        ignoresMouseEvents = true
        isOpaque = false
        hasShadow = false
        backgroundColor = .clear
        isReleasedWhenClosed = false
        animationBehavior = .none
    }

    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}
