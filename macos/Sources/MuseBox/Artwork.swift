import CoreGraphics
import Foundation
import ImageIO
import MuseBoxCore

/// Everything the room needs from one cover, computed once per track off the
/// main thread: the image, its two colours, and its 1-bit panel face.
struct CoverArt: @unchecked Sendable {
    var image: CGImage
    var palette: AlbumPalette
    /// The 1-bit render the shelf panel would show, in the album's colours.
    var panel: CGImage?

    static let panelSize = 240

    static func make(from image: CGImage, dither: DitherMode) -> CoverArt {
        let palette = PaletteExtractor.extract(rgb: Pixels.rgb(image, width: 100, height: 100))
        return CoverArt(image: image, palette: palette, panel: panelFace(image, palette: palette, dither: dither))
    }

    static func panelFace(_ image: CGImage, palette: AlbumPalette, dither: DitherMode) -> CGImage? {
        let size = panelSize
        let luma = Dither.luminance(rgb: Pixels.rgb(image, width: size, height: size))
        let ink = Dither.ink(luma, width: size, height: size, mode: dither)
        return Pixels.image(Dither.paint(ink, ink: palette.accent, paper: palette.background), width: size, height: size)
    }

    /// The idle cover: the dithered clock, ink in the accent, paper in the ground.
    static func idleFace(at date: Date, palette: AlbumPalette, dither: DitherMode) -> CGImage? {
        let size = panelSize
        let luma = IdleClock.frame(at: date, width: size, height: size)
        let ink = Dither.ink(luma, width: size, height: size, mode: dither)
        return Pixels.image(Dither.paint(ink, ink: palette.accent, paper: palette.background), width: size, height: size)
    }
}

enum Pixels {
    /// Center-crops to square, resamples, composites over white (like the
    /// backend), and returns packed RGB.
    static func rgb(_ image: CGImage, width: Int, height: Int) -> [UInt8] {
        var rgba = [UInt8](repeating: 255, count: width * height * 4)
        let side = min(image.width, image.height)
        let crop = CGRect(x: (image.width - side) / 2, y: (image.height - side) / 2, width: side, height: side)
        let source = image.cropping(to: crop) ?? image
        rgba.withUnsafeMutableBytes { buffer in
            guard let context = CGContext(
                data: buffer.baseAddress, width: width, height: height, bitsPerComponent: 8,
                bytesPerRow: width * 4, space: CGColorSpace(name: CGColorSpace.sRGB)!,
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
            ) else { return }
            context.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
            context.fill(CGRect(x: 0, y: 0, width: width, height: height))
            context.interpolationQuality = .high
            context.draw(source, in: CGRect(x: 0, y: 0, width: width, height: height))
        }
        var rgb = [UInt8](repeating: 0, count: width * height * 3)
        for index in 0..<(width * height) {
            rgb[index * 3] = rgba[index * 4]
            rgb[index * 3 + 1] = rgba[index * 4 + 1]
            rgb[index * 3 + 2] = rgba[index * 4 + 2]
        }
        return rgb
    }

    static func image(_ rgba: [UInt8], width: Int, height: Int) -> CGImage? {
        guard let provider = CGDataProvider(data: Data(rgba) as CFData) else { return nil }
        return CGImage(
            width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: width * 4,
            space: CGColorSpace(name: CGColorSpace.sRGB)!,
            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.noneSkipLast.rawValue),
            provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent
        )
    }

    static func decode(_ data: Data) -> CGImage? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        return CGImageSourceCreateImageAtIndex(source, 0, nil)
    }
}

/// Covers by track, from the URL Spotify's AppleScript hands us, or, without
/// automation consent, from Spotify's public oEmbed endpoint (no key, no login).
actor ArtworkStore {
    private var cache: [String: CGImage] = [:]
    private var order: [String] = []
    private let session: URLSession = {
        let configuration = URLSessionConfiguration.default
        configuration.timeoutIntervalForRequest = 10
        configuration.urlCache = URLCache(memoryCapacity: 8 << 20, diskCapacity: 64 << 20)
        return URLSession(configuration: configuration)
    }()

    func cover(for track: NowPlaying) async -> CGImage? {
        if let hit = cache[track.trackID] { return hit }
        var url = track.artworkURL
        if url == nil, let id = track.spotifyTrackID { url = await oEmbedThumbnail(trackID: id) }
        guard let url, let image = await download(url) else { return nil }
        remember(track.trackID, image)
        return image
    }

    private func download(_ url: URL) async -> CGImage? {
        guard let (data, response) = try? await session.data(from: url),
              (response as? HTTPURLResponse)?.statusCode == 200
        else { return nil }
        return Pixels.decode(data)
    }

    private func oEmbedThumbnail(trackID: String) async -> URL? {
        var components = URLComponents(string: "https://open.spotify.com/oembed")
        components?.queryItems = [URLQueryItem(name: "url", value: "https://open.spotify.com/track/\(trackID)")]
        guard let endpoint = components?.url,
              let (data, _) = try? await session.data(from: endpoint),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let thumbnail = object["thumbnail_url"] as? String
        else { return nil }
        // oEmbed hands out the 300 px size; the same image id at 640 px is one
        // prefix away on Spotify's CDN.
        return URL(string: thumbnail.replacingOccurrences(of: "ab67616d00001e02", with: "ab67616d0000b273"))
    }

    private func remember(_ id: String, _ image: CGImage) {
        cache[id] = image
        order.removeAll { $0 == id }
        order.append(id)
        while order.count > 24 { cache[order.removeFirst()] = nil }
    }
}
