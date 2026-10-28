// Chat pictures, decoded once and off the main actor (iphone-scroll-perf release 3,
// 2026-10-28). `ImageBubble` used to run `UIImage(data:)` + `ImageTrim` on the main actor
// inside its `.task`, and — since the transcript rows went lazy (#2149) — did it again every
// time a picture row was realised on a trip through history, showing the 240 × 160
// placeholder until the bytes came back (a layout jump under the reader's finger).
//
// Now: one decode per blob, on a background task, with the bitmap prepared for display
// (`preparingForDisplay`), kept in an `NSCache` by blob ref with the picture's natural size
// beside it. A re-realised row paints at its final size at once from the cache; a row whose
// picture is not cached yet still lays out at the remembered size while the bytes load.

import Foundation
import UIKit

enum ChatImageCache {
    /// What a bubble shows: the picture (all frames for an animated one), the first frame
    /// to hold while the row is off screen, whether it animates.
    struct Decoded: @unchecked Sendable {
        let image: UIImage
        let stillFrame: UIImage?
        let animated: Bool
        var naturalSize: CGSize { image.size }
    }

    private static let images: NSCache<NSString, Entry> = {
        let c = NSCache<NSString, Entry>()
        c.totalCostLimit = 96 * 1024 * 1024
        return c
    }()

    private final class Entry {
        let decoded: Decoded
        init(_ decoded: Decoded) { self.decoded = decoded }
    }

    /// Natural sizes outlive the bitmaps (a few bytes each): a picture evicted from the
    /// bitmap cache still lays out at its real size while it reloads.
    @MainActor private static var sizes: [String: CGSize] = [:]

    @MainActor
    static func decoded(for ref: String) -> Decoded? {
        images.object(forKey: ref as NSString)?.decoded
    }

    @MainActor
    static func naturalSize(for ref: String) -> CGSize? {
        if let hit = images.object(forKey: ref as NSString) { return hit.decoded.naturalSize }
        return sizes[ref]
    }

    @MainActor
    static func store(_ decoded: Decoded, for ref: String) {
        let cost = Int(decoded.image.size.width * decoded.image.scale * decoded.image.size.height * decoded.image.scale * 4)
            * max(1, decoded.image.images?.count ?? 1)
        images.setObject(Entry(decoded), forKey: ref as NSString, cost: cost)
        sizes[ref] = decoded.naturalSize
    }

    /// Decode the bytes away from the main actor. Still pictures are trimmed (`ImageTrim`)
    /// and prepared for display so the first draw does no decoding; animated GIF / WebP keep
    /// every frame (`AnimatedImageFrames`). `nil` when the bytes are not a picture.
    static func decode(_ data: Data) async -> Decoded? {
        await Task.detached(priority: .userInitiated) { () -> Decoded? in
            if let animated = AnimatedImageFrames.animatedImage(from: data) {
                return Decoded(image: animated, stillFrame: animated.images?.first ?? animated, animated: true)
            }
            guard let img = UIImage(data: data) else { return nil }
            let trimmed = ImageTrim.trimmed(img)
            let ready = trimmed.preparingForDisplay() ?? trimmed
            return Decoded(image: ready, stillFrame: nil, animated: false)
        }.value
    }
}
