import AppKit
import MuseBoxCore
import QuartzCore
import SwiftUI

/// How a surface paints the room.
struct AmbientStyle: Equatable {
    /// Opacity of the dark ground under the light (0: none, over a wallpaper).
    var ground = 1.0
    /// Scales every light.
    var strength = 1.0
    /// Scales the dominant-colour wash on its own, when set.
    var wash: Double?
    /// The halftone dot screen and the vignette.
    var texture = true
    /// Light in the palette's lifted lamp colours, for washing over a wallpaper.
    var lamp = false

    static let window = AmbientStyle()
    static let glass = AmbientStyle(ground: 0.62)

    /// The muse-box room as the desktop, a touch brighter than the window.
    static func scene(_ intensity: Double) -> AmbientStyle {
        AmbientStyle(strength: 0.9 + 0.7 * intensity)
    }

    /// Light only, over your own wallpaper: the wash tints instead of covering.
    static func tint(_ intensity: Double) -> AmbientStyle {
        AmbientStyle(ground: 0, strength: 1.9 * intensity, wash: 0.55 * intensity, texture: false, lamp: true)
    }
}

/// The six lights of `.ambient` in `web/src/styles.css` (wash, halo, glow,
/// beam, two orbs), plus the dot screen and vignette, as Core Animation
/// layers. Gradients are rasterised once per colour; each frame only moves,
/// scales and fades them, which the render server composites on the GPU. An
/// ambient app has to cost next to nothing to leave running all evening.
final class AmbientLayer: CALayer {
    /// (`style` is taken: CALayer already has one.)
    var look = AmbientStyle() {
        didSet { if look != oldValue { restyle() } }
    }
    /// Where the halo sits, in unit coordinates (behind the cover).
    var focus = CGPoint(x: 0.5, y: 0.444)

    private let ground = CALayer()
    private let wash = CAGradientLayer()
    private let halo = CAGradientLayer()
    private let glow = CAGradientLayer()
    private let beam = CAGradientLayer()
    private let orbA = CAGradientLayer()
    private let orbB = CAGradientLayer()
    private let dots = CALayer()
    private let vignette = CAGradientLayer()
    private var painted: (AlbumPalette, Bool)?
    private var laidOut = CGSize.zero

    override init() {
        super.init()
        setup()
    }

    override init(layer: Any) {
        super.init(layer: layer)
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        setup()
    }

    private func setup() {
        masksToBounds = true
        ground.backgroundColor = CGColor(srgbRed: 0x0B / 255, green: 0x0A / 255, blue: 0x09 / 255, alpha: 1)
        for (layer, fade) in [(wash, 0.74), (halo, 0.72), (glow, 0.76), (orbA, 0.72), (orbB, 0.70)] {
            layer.type = .radial
            layer.startPoint = CGPoint(x: 0.5, y: 0.5)
            layer.endPoint = CGPoint(x: 1, y: 1)
            layer.locations = [0, fade * 0.36, fade * 0.68, fade].map { NSNumber(value: $0) }
        }
        // CSS 115deg (right and a little down), in y-up unit space.
        beam.startPoint = CGPoint(x: 0.5 - 0.453, y: 0.5 + 0.211)
        beam.endPoint = CGPoint(x: 0.5 + 0.453, y: 0.5 - 0.211)
        beam.locations = [0.30, 0.50, 0.70]
        dots.backgroundColor = NSColor(patternImage: NSImage(cgImage: Self.dotTile, size: NSSize(width: 4, height: 4))).cgColor
        dots.opacity = 0.5
        vignette.type = .radial
        vignette.startPoint = CGPoint(x: 0.5, y: 0.5)
        vignette.endPoint = CGPoint(x: 1, y: 1)
        vignette.colors = [CGColor(gray: 0, alpha: 0), CGColor(gray: 0, alpha: 0.5)]
        vignette.locations = [0.52, 1]
        for layer in [ground, wash, halo, glow, beam, orbA, orbB, dots, vignette] {
            layer.actions = ["position": NSNull(), "bounds": NSNull(), "transform": NSNull(), "opacity": NSNull(), "colors": NSNull()]
            addSublayer(layer)
        }
        actions = ["bounds": NSNull(), "position": NSNull()]
        restyle()
    }

    private func restyle() {
        ground.isHidden = look.ground <= 0
        ground.opacity = Float(look.ground)
        dots.isHidden = !look.texture
        vignette.isHidden = !look.texture
        painted = nil
    }

    /// Paints one frame. Cheap: positions, transforms, opacities; colours
    /// only while a palette is blending.
    func render(_ frame: AmbientFrame) {
        let size = bounds.size
        guard size.width > 0, size.height > 0 else { return }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        if size != laidOut { layout(size) }
        paint(look.lamp ? frame.palette.lamp : frame.palette)
        move(frame, size)
        CATransaction.commit()
    }

    private func layout(_ size: CGSize) {
        laidOut = size
        let w = size.width, h = size.height
        let d = 0.58 * max(w, h)
        ground.frame = CGRect(origin: .zero, size: size)
        dots.frame = CGRect(origin: .zero, size: size)
        wash.bounds = CGRect(x: 0, y: 0, width: 2.24 * w, height: 2.24 * h)
        halo.bounds = CGRect(x: 0, y: 0, width: 1.008 * w, height: 0.896 * h)
        glow.bounds = CGRect(x: 0, y: 0, width: 1.68 * w, height: 1.54 * h)
        beam.bounds = CGRect(x: 0, y: 0, width: 1.4 * w, height: 1.4 * h)
        orbA.bounds = CGRect(x: 0, y: 0, width: d, height: d)
        orbB.bounds = CGRect(x: 0, y: 0, width: d, height: d)
        vignette.bounds = CGRect(x: 0, y: 0, width: 2.4 * w, height: 1.9 * h)
        vignette.position = CGPoint(x: 0.5 * w, y: (1 - 0.42) * h)
    }

    private func paint(_ palette: AlbumPalette) {
        if let painted, painted.0 == palette, painted.1 == look.lamp { return }
        painted = (palette, look.lamp)
        func light(_ color: RGB) -> [CGColor] {
            [1, 0.62, 0.22, 0].map { CGColor(srgbRed: color.r, green: color.g, blue: color.b, alpha: $0) }
        }
        wash.colors = light(palette.background)
        halo.colors = light(palette.accent)
        glow.colors = light(palette.accent)
        orbA.colors = light(palette.accent)
        orbB.colors = light(palette.blend)
        let accent = palette.accent
        beam.colors = [0, 1, 0].map { CGColor(srgbRed: accent.r, green: accent.g, blue: accent.b, alpha: $0) }
    }

    /// The geometry below is the web's, top-down; layers are AppKit's, bottom-up.
    private func move(_ frame: AmbientFrame, _ size: CGSize) {
        let w = size.width, h = size.height
        func at(_ x: CGFloat, _ y: CGFloat) -> CGPoint { CGPoint(x: x, y: h - y) }
        let energy = frame.energy
        let still = 1 - frame.playing
        let strength = look.strength

        // Deep wash of the dominant colour, wandering over 32 beats, inhaling over 16.
        let wander = AmbientDriver.pingPong(frame.beats / 32)
        let washScale = 1 + 0.08 * wander
        wash.position = at(
            w / 2 + (0.22 * w - w / 2) * washScale + (-0.056 + 0.112 * wander) * w,
            h / 2 + (0.78 * h - h / 2) * washScale + (-0.028 + 0.07 * wander) * h
        )
        wash.transform = CATransform3DMakeScale(washScale, washScale, 1)
        let inhale = 0.85 + 0.15 * (1 - cos(2 * .pi * frame.beats / 16)) / 2
        wash.opacity = Float(0.96 * inhale * (look.wash ?? strength))

        // A soft halo behind the cover, landing on every beat.
        let haloFloor = 0.5 + energy * 0.25
        let haloLive = haloFloor + (1 - haloFloor) * min(frame.pulse + 0.35 * frame.hit, 1)
        let haloScale = 1 + energy * 0.05 * frame.pulse * frame.playing
        halo.position = at(focus.x * w, focus.y * h)
        halo.transform = CATransform3DMakeScale(haloScale, haloScale, 1)
        halo.opacity = Float((0.10 + energy * 0.16) * (haloLive * frame.playing + 0.55 * still) * strength)

        // The accent, breathing with the bass from below.
        let glowFloor = 0.62 + energy * 0.1
        let glowScale = 1 + energy * 0.045 * frame.breath * frame.playing
        glow.position = at(w / 2, h / 2 + (1.144 * h - h / 2) * glowScale)
        glow.transform = CATransform3DMakeScale(glowScale, glowScale, 1)
        glow.opacity = Float((0.18 + energy * 0.30) * ((glowFloor + (1 - glowFloor) * frame.breath) * frame.playing + 0.55 * still) * strength)

        // A long diagonal sheen every 16 beats, faded at the ends of its sweep.
        let sweep = frame.beats / 16 - (frame.beats / 16).rounded(.down)
        beam.position = at(w / 2 + (-0.55 + 1.1 * sweep) * 1.4 * w, h / 2)
        beam.opacity = Float((0.06 + energy * 0.12) * sin(.pi * sweep) * (frame.playing + 0.55 * still) * strength)

        // Two lamps on long paths, swelling every other beat (A) and every fourth (B).
        let d = 0.58 * max(w, h)
        let driftA = AmbientDriver.pingPong(frame.beats / 48)
        let swellA = (frame.beatIndex % 2 == 0 ? frame.pulse : 0.3 * frame.pulse) * frame.playing
        let scaleA = 1 + energy * 0.05 * swellA
        orbA.position = at(-0.16 * w + d / 2 + 0.12 * w * driftA, -0.14 * h + d / 2 + 0.07 * h * driftA)
        orbA.transform = CATransform3DMakeScale(scaleA, scaleA, 1)
        orbA.opacity = Float((0.08 + energy * 0.12) * (0.8 + 0.2 * swellA + 0.1 * still) * strength)

        let driftB = AmbientDriver.pingPong(frame.beats / 56)
        let swellB = (frame.beatIndex % 4 == 0 ? frame.pulse : 0.25 * frame.pulse) * frame.playing
        let scaleB = 1 + energy * 0.05 * swellB
        orbB.position = at(1.16 * w - d / 2 - 0.10 * w * driftB, 1.18 * h - d / 2 - 0.08 * h * driftB)
        orbB.transform = CATransform3DMakeScale(scaleB, scaleB, 1)
        orbB.opacity = Float((0.12 + energy * 0.14) * (0.8 + 0.2 * swellB + 0.1 * still) * strength)
    }

    /// 4 pt cells with a 1 pt-radius dot, at 2x.
    static let dotTile: CGImage = {
        let context = CGContext(
            data: nil, width: 8, height: 8, bitsPerComponent: 8, bytesPerRow: 32,
            space: CGColorSpace(name: CGColorSpace.sRGB)!,
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        )!
        context.setFillColor(CGColor(srgbRed: 240 / 255, green: 232 / 255, blue: 220 / 255, alpha: 0.1))
        context.fillEllipse(in: CGRect(x: 2, y: 2, width: 4, height: 4))
        return context.makeImage()!
    }()

    /// A still of one frame, for `--snapshot`.
    static func image(_ frame: AmbientFrame, size: CGSize, scale: CGFloat, style: AmbientStyle, focus: CGPoint = CGPoint(x: 0.5, y: 0.444)) -> CGImage? {
        let layer = AmbientLayer()
        layer.look = style
        layer.focus = focus
        layer.bounds = CGRect(origin: .zero, size: size)
        layer.render(frame)
        let width = Int(size.width * scale), height = Int(size.height * scale)
        guard let context = CGContext(
            data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
            space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ) else { return nil }
        context.scaleBy(x: scale, y: scale)
        layer.render(in: context)
        return context.makeImage()
    }
}

/// Hosts an `AmbientLayer` and drives it from the shared driver with a
/// display link: 60 fps while it can hear the beat, 30 while playing, 10 at
/// rest, and nothing at all while the window cannot be seen.
final class AmbientView: NSView {
    let ambient = AmbientLayer()
    var sample: () -> AmbientFrame = { AmbientFrame() }
    private var link: CADisplayLink?
    private var rate: Float = 0
    private var occlusion: NSObjectProtocol?

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layerContentsRedrawPolicy = .never
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        wantsLayer = true
    }

    override func makeBackingLayer() -> CALayer { ambient }
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func layout() {
        super.layout()
        ambient.render(sample())
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        link?.invalidate()
        link = nil
        if let occlusion { NotificationCenter.default.removeObserver(occlusion) }
        occlusion = nil
        guard let window else { return }
        let link = displayLink(target: self, selector: #selector(tick))
        link.add(to: .main, forMode: .common)
        self.link = link
        rate = 0
        occlusion = NotificationCenter.default.addObserver(
            forName: NSWindow.didChangeOcclusionStateNotification, object: window, queue: .main
        ) { [weak self, weak window] _ in
            self?.link?.isPaused = !(window?.occlusionState.contains(.visible) ?? false)
        }
        link.isPaused = !window.occlusionState.contains(.visible)
    }

    @objc private func tick(_ link: CADisplayLink) {
        let frame = sample()
        ambient.render(frame)
        let wanted: Float = frame.playing > 0.5 ? (frame.hearing ? 60 : 30) : 10
        if wanted != rate {
            rate = wanted
            link.preferredFrameRateRange = CAFrameRateRange(minimum: wanted / 2, maximum: wanted, preferred: wanted)
        }
    }
}

/// The room behind a SwiftUI view. In stills (which cannot host AppKit views)
/// it renders the same layers to an image instead.
struct AmbientSurface: View {
    var driver: AmbientDriver
    var style: AmbientStyle
    var focus = UnitPoint(x: 0.5, y: 0.444)
    @Environment(\.stillFrame) private var still

    var body: some View {
        if let still {
            GeometryReader { geometry in
                if let image = AmbientLayer.image(still, size: geometry.size, scale: 2, style: style, focus: CGPoint(x: focus.x, y: focus.y)) {
                    Image(decorative: image, scale: 2)
                }
            }
        } else {
            Live(driver: driver, style: style, focus: CGPoint(x: focus.x, y: focus.y))
        }
    }

    private struct Live: NSViewRepresentable {
        var driver: AmbientDriver
        var style: AmbientStyle
        var focus: CGPoint

        func makeNSView(context: Context) -> AmbientView {
            let view = AmbientView()
            view.sample = { [weak driver] in
                MainActor.assumeIsolated { driver?.sample() ?? AmbientFrame() }
            }
            return view
        }

        func updateNSView(_ view: AmbientView, context: Context) {
            view.ambient.look = style
            view.ambient.focus = focus
        }
    }
}

private struct StillFrameKey: EnvironmentKey {
    static let defaultValue: AmbientFrame? = nil
}

extension EnvironmentValues {
    /// Set while rendering stills: every animated view shows this frame.
    var stillFrame: AmbientFrame? {
        get { self[StillFrameKey.self] }
        set { self[StillFrameKey.self] = newValue }
    }
}

/// One clock for every SwiftUI view that moves with the music, so a window
/// re-renders once per frame however many of them there are: 30 fps while
/// the record spins, 6 at rest (Bitka still breathes), nothing while no
/// muse-box window can be seen.
@MainActor
final class FrameClock: NSObject, ObservableObject {
    static let shared = FrameClock()

    private(set) var now = Date()
    private var link: CADisplayLink?
    private var settleUntil = 0.0
    private var rate: Float = 0

    var playing = false {
        didSet {
            if oldValue && !playing { settleUntil = CACurrentMediaTime() + 1.6 }
            retune()
        }
    }

    /// Windows currently showing moving views.
    private var viewers: Set<String> = []

    func show(_ viewer: String, _ visible: Bool) {
        if visible { viewers.insert(viewer) } else { viewers.remove(viewer) }
        retune()
    }

    private func retune() {
        if link == nil, let screen = NSScreen.main {
            let link = screen.displayLink(target: self, selector: #selector(tick))
            link.add(to: .main, forMode: .common)
            self.link = link
        }
        guard let link else { return }
        link.isPaused = viewers.isEmpty
        let wanted: Float = playing || CACurrentMediaTime() < settleUntil ? 30 : 6
        if wanted != rate {
            rate = wanted
            link.preferredFrameRateRange = CAFrameRateRange(minimum: wanted / 2, maximum: wanted, preferred: wanted)
        }
    }

    @objc private func tick(_ link: CADisplayLink) {
        now = Date()
        objectWillChange.send()
        if rate == 30, !playing, CACurrentMediaTime() >= settleUntil { retune() }
    }
}

/// Samples the shared light for the few views that move with it, on the
/// shared frame clock.
struct Pulse<Content: View>: View {
    var driver: AmbientDriver
    @ViewBuilder var content: (AmbientFrame, Date) -> Content
    @ObservedObject private var clock = FrameClock.shared
    @Environment(\.stillFrame) private var still
    @Environment(\.stillDate) private var stillDate

    init(driver: AmbientDriver, @ViewBuilder content: @escaping (AmbientFrame, Date) -> Content) {
        self.driver = driver
        self.content = content
    }

    var body: some View {
        if let still {
            content(still, stillDate ?? .now)
        } else {
            content(driver.sample(), clock.now)
        }
    }
}

private struct StillDateKey: EnvironmentKey {
    static let defaultValue: Date? = nil
}

extension EnvironmentValues {
    var stillDate: Date? {
        get { self[StillDateKey.self] }
        set { self[StillDateKey.self] = newValue }
    }
}
