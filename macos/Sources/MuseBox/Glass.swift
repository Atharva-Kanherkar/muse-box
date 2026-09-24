import AppKit
import SwiftUI

/// Set while rendering stills (`--snapshot`): live materials cannot be
/// rendered offscreen, so glass falls back to a painted look-alike.
private struct SnapshottingKey: EnvironmentKey {
    static let defaultValue = false
}

extension EnvironmentValues {
    var snapshotting: Bool {
        get { self[SnapshottingKey.self] }
        set { self[SnapshottingKey.self] = newValue }
    }
}

extension View {
    /// Frosted glass in the album's tint: Liquid Glass on macOS 26, a
    /// material with a lit rim everywhere else.
    func glass<S: InsettableShape>(_ shape: S, tint: Color = .clear, interactive: Bool = false) -> some View {
        modifier(GlassSurface(shape: shape, tint: tint, interactive: interactive))
    }
}

private struct GlassSurface<S: InsettableShape>: ViewModifier {
    var shape: S
    var tint: Color
    var interactive: Bool
    @Environment(\.snapshotting) private var snapshotting

    func body(content: Content) -> some View {
        if snapshotting {
            painted(content)
        } else {
            live(content)
        }
    }

    private func painted(_ content: Content) -> some View {
        content
            .background(shape.fill(Color.black.opacity(0.42)))
            .background(shape.fill(tint.opacity(0.14)))
            .overlay(rim)
    }

    #if compiler(>=6.2)
    @ViewBuilder
    private func live(_ content: Content) -> some View {
        if #available(macOS 26.0, *) {
            content.glassEffect(
                interactive ? .regular.tint(tint.opacity(0.22)).interactive() : .regular.tint(tint.opacity(0.22)),
                in: shape
            )
        } else {
            material(content)
        }
    }
    #else
    private func live(_ content: Content) -> some View {
        material(content)
    }
    #endif

    private func material(_ content: Content) -> some View {
        content
            .background(shape.fill(tint.opacity(0.12)))
            .background(shape.fill(Color.black.opacity(0.28)))
            .background(.ultraThinMaterial, in: shape)
            .overlay(rim)
    }

    /// The lit edge that makes glass read as glass.
    private var rim: some View {
        shape.strokeBorder(
            LinearGradient(
                colors: [Color.white.opacity(0.30), Color.white.opacity(0.06), Color.white.opacity(0.14)],
                startPoint: .top,
                endPoint: .bottom
            ),
            lineWidth: 1
        )
    }
}

/// Behind-window blur: the desktop (and the room light on it) shows through
/// the whole window, frosted.
struct BackdropBlur: NSViewRepresentable {
    var material: NSVisualEffectView.Material = .hudWindow

    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = material
        view.blendingMode = .behindWindow
        view.state = .active
        view.appearance = NSAppearance(named: .darkAqua)
        return view
    }

    func updateNSView(_ view: NSVisualEffectView, context: Context) {
        if view.material != material { view.material = material }
    }
}

/// Reaches the hosting NSWindow to make it see-through, dark, and draggable
/// from its empty top edge. Applies each setting once: touching window
/// properties relayouts the hosting view, and doing it on every update would
/// spin the view graph forever.
struct WindowChrome: NSViewRepresentable {
    var translucent: Bool
    /// Told whether any of the window can be seen (occlusion, minimise, close).
    var visibility: (Bool) -> Void = { _ in }

    final class Coordinator {
        weak var window: NSWindow?
        var translucent: Bool?
        var observers: [NSObjectProtocol] = []

        deinit { observers.forEach(NotificationCenter.default.removeObserver) }
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        DispatchQueue.main.async { configure(view.window, context.coordinator) }
        return view
    }

    func updateNSView(_ view: NSView, context: Context) {
        let coordinator = context.coordinator
        guard coordinator.window !== view.window || coordinator.translucent != translucent else { return }
        DispatchQueue.main.async { configure(view.window, coordinator) }
    }

    private func configure(_ window: NSWindow?, _ coordinator: Coordinator) {
        guard let window, coordinator.window !== window || coordinator.translucent != translucent else { return }
        if coordinator.window !== window {
            coordinator.observers.forEach(NotificationCenter.default.removeObserver)
            let report = visibility
            coordinator.observers = [
                NotificationCenter.default.addObserver(forName: NSWindow.didChangeOcclusionStateNotification, object: window, queue: .main) { _ in
                    report(window.occlusionState.contains(.visible))
                },
                NotificationCenter.default.addObserver(forName: NSWindow.willCloseNotification, object: window, queue: .main) { _ in
                    report(false)
                },
            ]
            report(window.occlusionState.contains(.visible))
        }
        coordinator.window = window
        coordinator.translucent = translucent
        window.isOpaque = !translucent
        window.backgroundColor = translucent ? .clear : NSColor(srgbRed: 0x0B / 255, green: 0x0A / 255, blue: 0x09 / 255, alpha: 1)
        if !window.titlebarAppearsTransparent { window.titlebarAppearsTransparent = true }
        if window.titleVisibility != .hidden { window.titleVisibility = .hidden }
        if window.appearance?.name != .darkAqua { window.appearance = NSAppearance(named: .darkAqua) }
        if !window.styleMask.contains(.fullSizeContentView) { window.styleMask.insert(.fullSizeContentView) }
    }
}

/// Reports whether the window hosting it can be seen, for views that are not
/// the main window (the menu bar panel).
struct VisibilityProbe: NSViewRepresentable {
    var report: (Bool) -> Void

    final class Coordinator {
        var observer: NSObjectProtocol?
        deinit { observer.map(NotificationCenter.default.removeObserver) }
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        let coordinator = context.coordinator
        let report = report
        DispatchQueue.main.async {
            guard let window = view.window else { return }
            coordinator.observer = NotificationCenter.default.addObserver(
                forName: NSWindow.didChangeOcclusionStateNotification, object: window, queue: .main
            ) { _ in report(window.occlusionState.contains(.visible)) }
            report(window.occlusionState.contains(.visible))
        }
        return view
    }

    func updateNSView(_ view: NSView, context: Context) {}
}
