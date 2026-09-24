import Foundation

/// The two colours a record gives the room: `[dominant background, accent]`,
/// exactly the `palette` field of the backend's render document.
public struct AlbumPalette: Hashable, Sendable {
    public var background: RGB
    public var accent: RGB

    public init(background: RGB, accent: RGB) {
        self.background = background
        self.accent = accent
    }

    /// What the web client paints before any document arrives.
    public static let quiet = AlbumPalette(
        background: RGB(bytes: 0x1A, 0x1A, 0x1A),
        accent: RGB(bytes: 0xE0, 0xE0, 0xE0)
    )

    /// `color-mix(in oklab, accent 45%, background)`: the second orb's light.
    public var blend: RGB { accent.mixed(with: background, share: 0.45) }

    /// The same two colours as light rather than paint, for washing over a
    /// wallpaper: a dark dominant colour would only dim the desktop, so both
    /// are lifted (hue kept) until they read as a lamp.
    public var lamp: AlbumPalette {
        func lift(_ color: RGB, saturation: Double, lightness: ClosedRange<Double>) -> RGB {
            let hsl = color.hsl
            // A grey cover has no hue to lift; leave it grey rather than invent one.
            let s = hsl.s < 0.06 ? hsl.s : max(hsl.s, saturation)
            return RGB(h: hsl.h, s: s, l: hsl.l.clamped(to: lightness))
        }
        return AlbumPalette(
            background: lift(background, saturation: 0.5, lightness: 0.42...0.6),
            accent: lift(accent, saturation: 0.62, lightness: 0.5...0.68)
        )
    }

    public func interpolated(to target: AlbumPalette, t: Double) -> AlbumPalette {
        AlbumPalette(
            background: background.interpolated(to: target.background, t: t),
            accent: accent.interpolated(to: target.accent, t: t)
        )
    }
}

/// A straight port of `extract_palette` in `src/image.rs`: a two-centroid
/// k-means over the exact-colour histogram, heavier cluster wins background,
/// and the accent is clamped so dark covers never hand the room a near-black
/// "accent" that looks broken as light.
public enum PaletteExtractor {
    /// `pixels` is packed RGB, three bytes per pixel, already composited over white.
    public static func extract(rgb pixels: [UInt8]) -> AlbumPalette {
        let histogram = histogram(pixels)
        guard let first = histogram.first else {
            return AlbumPalette(background: RGB(bytes: 0, 0, 0), accent: clampAccent([0, 0, 0]).rgb)
        }

        // The colour furthest from the most common one, weighted by how much of
        // the cover it covers. Ties go to the smaller colour, as in Rust.
        var second = first
        var best = -Double.infinity
        for entry in histogram {
            let score = distance(entry.point, first.point) * Double(entry.count)
            if score > best || (score == best && entry.key < second.key) {
                best = score
                second = entry
            }
        }

        var centroids = [first.point, second.point]
        for _ in 0..<16 {
            var sums = [SIMD3<Double>.zero, .zero]
            var weights = [0, 0]
            for entry in histogram {
                let cluster = nearest(entry.point, centroids)
                weights[cluster] += entry.count
                sums[cluster] += entry.point * Double(entry.count)
            }
            for cluster in 0..<2 where weights[cluster] > 0 {
                centroids[cluster] = sums[cluster] / Double(weights[cluster])
            }
        }

        var weights = [0, 0]
        for entry in histogram {
            weights[nearest(entry.point, centroids)] += entry.count
        }
        let (dominant, accent) = weights[1] > weights[0]
            ? (centroids[1], centroids[0])
            : (centroids[0], centroids[1])
        return AlbumPalette(
            background: bytes(dominant).rgb,
            accent: clampAccent(bytes(accent)).rgb
        )
    }

    /// HSL clamp from the backend: `S >= 0.42`, `L` in `0.36...0.74`.
    public static func clampAccent(_ color: [UInt8]) -> [UInt8] {
        let hsl = RGB(bytes: color[0], color[1], color[2]).hsl
        return RGB(h: hsl.h, s: max(hsl.s, 0.42), l: hsl.l.clamped(to: 0.36...0.74)).bytes
    }

    private struct Entry {
        var key: UInt32
        var point: SIMD3<Double>
        var count: Int
    }

    private static func histogram(_ pixels: [UInt8]) -> [Entry] {
        var counts: [UInt32: Int] = [:]
        var index = 0
        while index + 2 < pixels.count {
            let key = UInt32(pixels[index]) << 16 | UInt32(pixels[index + 1]) << 8 | UInt32(pixels[index + 2])
            counts[key, default: 0] += 1
            index += 3
        }
        return counts
            .map { key, count in
                Entry(
                    key: key,
                    point: SIMD3(Double(key >> 16 & 0xFF), Double(key >> 8 & 0xFF), Double(key & 0xFF)),
                    count: count
                )
            }
            // Most common first; equal counts in RGB order, like the BTreeMap walk.
            .sorted { $0.count != $1.count ? $0.count > $1.count : $0.key < $1.key }
    }

    /// Cluster 1 only when strictly closer, matching the Rust tie-break.
    private static func nearest(_ point: SIMD3<Double>, _ centroids: [SIMD3<Double>]) -> Int {
        distance(point, centroids[1]) < distance(point, centroids[0]) ? 1 : 0
    }

    private static func distance(_ left: SIMD3<Double>, _ right: SIMD3<Double>) -> Double {
        let delta = left - right
        return (delta * delta).sum()
    }

    private static func bytes(_ point: SIMD3<Double>) -> [UInt8] {
        [point.x, point.y, point.z].map { UInt8($0.rounded().clamped(to: 0...255)) }
    }
}

private extension Array where Element == UInt8 {
    var rgb: RGB { RGB(bytes: self[0], self[1], self[2]) }
}
