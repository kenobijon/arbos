// Rounds a downloaded GitHub/uploaded picture for a notification (INPerson).
// Initials circles come from AvatarPalette.initialsPNG (D79).

import UIKit

enum AvatarImage {
    /// Fixed @3x: the extension has no screen to ask.
    static let scale: CGFloat = 3

    /// A downloaded picture clipped to a circle, for INPerson.
    static func roundPNG(_ data: Data, pixels: CGFloat) -> Data? {
        guard let src = UIImage(data: data) else { return nil }
        let side = pixels / AvatarImage.scale
        let rect = CGRect(x: 0, y: 0, width: side, height: side)
        let format = UIGraphicsImageRendererFormat()
        format.scale = AvatarImage.scale
        format.opaque = false
        let image = UIGraphicsImageRenderer(size: rect.size, format: format).image { _ in
            UIBezierPath(ovalIn: rect).addClip()
            let scale = max(side / src.size.width, side / src.size.height)
            let w = src.size.width * scale, h = src.size.height * scale
            src.draw(in: CGRect(x: (side - w) / 2, y: (side - h) / 2, width: w, height: h))
        }
        return image.pngData()
    }
}
