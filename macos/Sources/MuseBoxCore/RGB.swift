import Foundation

/// An sRGB colour, channels in `0...1`.
///
/// Mixing happens in OKLab, the same space the web client's
/// `color-mix(in oklab, …)` uses, so a blend of two album colours looks the
/// same on the Mac as it does in the browser.
public struct RGB: Hashable, Sendable {
    public var r: Double
    public var g: Double
    public var b: Double

    public init(r: Double, g: Double, b: Double) {
        self.r = r
        self.g = g
        self.b = b
    }

    public init(bytes red: UInt8, _ green: UInt8, _ blue: UInt8) {
        self.init(r: Double(red) / 255, g: Double(green) / 255, b: Double(blue) / 255)
    }

    /// `#rgb` or `#rrggbb`. Returns nil rather than guessing.
    public init?(hex: String) {
        var digits = hex.trimmingCharacters(in: .whitespaces)
        if digits.hasPrefix("#") { digits.removeFirst() }
        if digits.count == 3 { digits = digits.map { "\($0)\($0)" }.joined() }
        guard digits.count == 6, let value = UInt32(digits, radix: 16) else { return nil }
        self.init(
            bytes: UInt8((value >> 16) & 0xFF),
            UInt8((value >> 8) & 0xFF),
            UInt8(value & 0xFF)
        )
    }

    public var bytes: [UInt8] {
        [r, g, b].map { UInt8(($0 * 255).rounded().clamped(to: 0...255)) }
    }

    public var hex: String {
        "#" + bytes.map { String(format: "%02x", $0) }.joined()
    }

    /// Relative luminance, for picking legible ink on top of a colour.
    public var luminance: Double {
        0.2126 * Self.linear(r) + 0.7152 * Self.linear(g) + 0.0722 * Self.linear(b)
    }

    // MARK: OKLab

    public struct OKLab: Hashable, Sendable {
        public var l: Double
        public var a: Double
        public var b: Double
    }

    public var oklab: OKLab {
        let red = Self.linear(r), green = Self.linear(g), blue = Self.linear(b)
        let l = cbrt(0.4122214708 * red + 0.5363325363 * green + 0.0514459929 * blue)
        let m = cbrt(0.2119034982 * red + 0.6806995451 * green + 0.1073969566 * blue)
        let s = cbrt(0.0883024619 * red + 0.2817188376 * green + 0.6299787005 * blue)
        return OKLab(
            l: 0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
            a: 1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
            b: 0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s
        )
    }

    public init(oklab lab: OKLab) {
        let l = pow(lab.l + 0.3963377774 * lab.a + 0.2158037573 * lab.b, 3)
        let m = pow(lab.l - 0.1055613458 * lab.a - 0.0638541728 * lab.b, 3)
        let s = pow(lab.l - 0.0894841775 * lab.a - 1.2914855480 * lab.b, 3)
        self.init(
            r: Self.gamma(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s),
            g: Self.gamma(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s),
            b: Self.gamma(-0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s)
        )
    }

    /// CSS `color-mix(in oklab, self <share>, other)`.
    public func mixed(with other: RGB, share: Double) -> RGB {
        other.interpolated(to: self, t: share)
    }

    /// OKLab interpolation, `t = 0` is `self`, `t = 1` is `target`.
    public func interpolated(to target: RGB, t: Double) -> RGB {
        let from = oklab, to = target.oklab
        let t = t.clamped(to: 0...1)
        return RGB(oklab: OKLab(
            l: from.l + (to.l - from.l) * t,
            a: from.a + (to.a - from.a) * t,
            b: from.b + (to.b - from.b) * t
        ))
    }

    // MARK: HSL, matching `src/image.rs`

    public var hsl: (h: Double, s: Double, l: Double) {
        let maximum = max(r, g, b), minimum = min(r, g, b)
        let delta = maximum - minimum
        let lightness = (maximum + minimum) / 2
        guard delta > .ulpOfOne else { return (0, 0, lightness) }
        let saturation = delta / (1 - abs(2 * lightness - 1))
        let sector: Double
        if abs(maximum - r) <= .ulpOfOne {
            sector = ((g - b) / delta).euclidean(6)
        } else if abs(maximum - g) <= .ulpOfOne {
            sector = (b - r) / delta + 2
        } else {
            sector = (r - g) / delta + 4
        }
        return (sector * 60, saturation, lightness)
    }

    public init(h: Double, s: Double, l: Double) {
        let chroma = (1 - abs(2 * l - 1)) * s
        let sector = h.euclidean(360) / 60
        let secondary = chroma * (1 - abs(sector.euclidean(2) - 1))
        let (red, green, blue): (Double, Double, Double)
        switch Int(sector) {
        case 0: (red, green, blue) = (chroma, secondary, 0)
        case 1: (red, green, blue) = (secondary, chroma, 0)
        case 2: (red, green, blue) = (0, chroma, secondary)
        case 3: (red, green, blue) = (0, secondary, chroma)
        case 4: (red, green, blue) = (secondary, 0, chroma)
        default: (red, green, blue) = (chroma, 0, secondary)
        }
        let match = l - chroma / 2
        self.init(r: red + match, g: green + match, b: blue + match)
    }

    // MARK: transfer functions

    static func linear(_ channel: Double) -> Double {
        channel <= 0.04045 ? channel / 12.92 : pow((channel + 0.055) / 1.055, 2.4)
    }

    static func gamma(_ channel: Double) -> Double {
        let value = channel <= 0.0031308 ? 12.92 * channel : 1.055 * pow(channel, 1 / 2.4) - 0.055
        return value.clamped(to: 0...1)
    }
}

extension Comparable {
    public func clamped(to range: ClosedRange<Self>) -> Self {
        min(max(self, range.lowerBound), range.upperBound)
    }
}

extension Double {
    /// Rust's `rem_euclid`: always in `0..<modulus`.
    func euclidean(_ modulus: Double) -> Double {
        let value = truncatingRemainder(dividingBy: modulus)
        return value < 0 ? value + modulus : value
    }
}
