import AppKit
import SwiftUI

// Liquid Glass, used the way Apple's guidance asks (HIG › Materials, and the
// WWDC25 sessions "Meet Liquid Glass" and "Build a SwiftUI app with the new
// design"):
//
// - Glass is the functional layer only: controls float on it, while the cover,
//   the light, the titles and the lyrics stay content.
// - Neighbours share one GlassGroup. Glass cannot sample other glass, and a
//   shared container also lets the shapes blend and morph.
// - Nothing is painted over it: no fills, rims or strokes, and no fixed label
//   colours (text on glass is made vibrant by the system).
// - Tint is only for the one primary action, as colour in the background.
//
// macOS 14 and 15 get a frosted material in the same shapes.

extension View {
    /// Liquid Glass behind this view, in `shape`. Pass a tint only for the
    /// primary action.
    func glass<S: Shape>(_ shape: S, tint: Color? = nil, interactive: Bool = true) -> some View {
        modifier(GlassSurface(shape: shape, tint: tint, interactive: interactive))
    }

    /// Ties a glass shape to its identity inside a GlassGroup, so it morphs
    /// (rather than fades) as it comes and goes.
    @ViewBuilder
    func glassIdentity(_ id: String, in namespace: Namespace.ID) -> some View {
        #if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            glassEffectID(id, in: namespace)
        } else {
            self
        }
        #else
        self
        #endif
    }
}

/// Glass neighbours share one container, so they refract the same backdrop and
/// can blend and morph into one another. Keep `spacing` equal to the layout's
/// spacing so shapes stay apart at rest.
struct GlassGroup<Content: View>: View {
    var spacing: CGFloat
    @ViewBuilder var content: Content

    var body: some View {
        #if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            GlassEffectContainer(spacing: spacing) { content }
        } else {
            content
        }
        #else
        content
        #endif
    }
}

private struct GlassSurface<S: Shape>: ViewModifier {
    var shape: S
    var tint: Color?
    var interactive: Bool

    func body(content: Content) -> some View {
        #if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            // Regular, not clear: Apple keeps clear for controls over photos and
            // video, and this glass floats over the room's dark light.
            let base = Glass.regular.tint(tint)
            content.glassEffect(interactive ? base.interactive() : base, in: shape)
        } else {
            frosted(content)
        }
        #else
        frosted(content)
        #endif
    }

    /// Before Liquid Glass: a frosted material with a lit rim.
    private func frosted(_ content: Content) -> some View {
        content
            .background(tint.map { shape.fill($0.opacity(0.6)) })
            .background(.ultraThinMaterial, in: shape)
            .overlay(shape.stroke(
                LinearGradient(
                    colors: [.white.opacity(0.3), .white.opacity(0.06), .white.opacity(0.14)],
                    startPoint: .top,
                    endPoint: .bottom
                ),
                lineWidth: 1
            ))
    }
}

/// A round glass control, labelled with a symbol. The system handles the hover,
/// press and focus response (`interactive()`), and vibrancy keeps the
/// symbol legible over whatever the album light is doing.
struct GlassKey: View {
    var symbol: String
    var size: CGFloat
    /// Only for the primary action.
    var tint: Color? = nil
    var label: String
    var action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: size * 0.36, weight: .semibold))
                .contentTransition(.symbolEffect(.replace))
                .foregroundStyle(tint == nil ? AnyShapeStyle(.primary) : AnyShapeStyle(.white))
                .frame(width: size, height: size)
                .contentShape(Circle())
                .glass(Circle(), tint: tint)
        }
        .buttonStyle(.plain)
        .help(label)
        .accessibilityLabel(label)
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
