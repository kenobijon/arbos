// Renders a tree character to PNG for a notification: the first idle frame
// of the bundled Lottie, tinted on the tree sub-items from manifest
// `colors[]` (bg and ring untouched), drawn with Lottie's main-thread engine
// into an off-screen context. Also rounds a downloaded GitHub picture.

import UIKit
import Lottie

enum AvatarImage {
    /// Fixed @3x: the extension has no screen to ask.
    static let scale: CGFloat = 3
    private struct ColorSet { let tree, treeDark, treeLight, face: String }

    /// Tintable manifest colours by slug, plus their manifest order (the relay's hash contract).
    private static let palette: (sets: [String: ColorSet], order: [String]) = {
        guard let url = Bundle.main.url(forResource: "manifest", withExtension: "json"),
              let data = try? Data(contentsOf: url),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let list = obj["colors"] as? [[String: Any]] else { return ([:], []) }
        var out: [String: ColorSet] = [:]
        var order: [String] = []
        for c in list {
            guard let slug = c["slug"] as? String, slug != "mono",
                  let tree = c["tree"] as? String, let dark = c["tree_dark"] as? String, let light = c["tree_light"] as? String else { continue }
            out[slug] = ColorSet(tree: tree, treeDark: dark, treeLight: light, face: (c["face"] as? String) ?? "#000000")
            order.append(slug)
        }
        return (out, order)
    }()
    private static var colors: [String: ColorSet] { palette.sets }
    private static var coloredOrder: [String] { palette.order }

    /// The colour an agent without a chosen one shows (D74: coloured, never
    /// mono) — the relay's `colored[fnv1a64(id + ":color") % n]`.
    static func defaultColorName(forProjectId projectId: String) -> String? {
        guard !coloredOrder.isEmpty else { return nil }
        var h: UInt64 = 0xcbf29ce484222325
        for b in (projectId + ":color").utf8 { h = (h ^ UInt64(b)) &* 0x100000001b3 }
        return coloredOrder[Int(h % UInt64(coloredOrder.count))]
    }

    /// `colorName` nil → the project's default colour; "mono" → the art as drawn.
    @MainActor
    static func treePNG(slug: String, colorName: String?, pixels: CGFloat, projectId: String? = nil) -> Data? {
        guard let animation = LottieAnimation.named("idle", bundle: .main, subdirectory: "lottie/\(slug)") else { return nil }
        let view = LottieAnimationView(animation: animation, configuration: LottieConfiguration(renderingEngine: .mainThread))
        let side = pixels / AvatarImage.scale
        view.frame = CGRect(x: 0, y: 0, width: side, height: side)
        view.contentMode = .scaleAspectFit
        view.backgroundColor = .clear
        let name = colorName ?? projectId.flatMap(defaultColorName(forProjectId:))
        if let name, let set = colors[name] {
            let pairs: [(String, String)] = [
                ("tree.**.tree.Color", set.tree), ("tree.**.tree_dark.Color", set.treeDark),
                ("tree.**.tree_light.Color", set.treeLight), ("tree.**.face.Color", set.face),
            ]
            for (path, hex) in pairs {
                if let c = lottieColor(hex) {
                    view.setValueProvider(ColorValueProvider(c), keypath: AnimationKeypath(keypath: path))
                }
            }
        }
        view.currentProgress = 0
        view.layoutIfNeeded()
        view.forceDisplayUpdate()
        let format = UIGraphicsImageRendererFormat()
        format.scale = AvatarImage.scale
        format.opaque = false
        let image = UIGraphicsImageRenderer(size: view.bounds.size, format: format).image { ctx in
            view.layer.render(in: ctx.cgContext)
        }
        return image.pngData()
    }

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

    private static func lottieColor(_ hex: String) -> LottieColor? {
        var s = hex.trimmingCharacters(in: .whitespaces)
        if s.hasPrefix("#") { s.removeFirst() }
        guard s.count == 6, let v = UInt32(s, radix: 16) else { return nil }
        return LottieColor(r: Double((v >> 16) & 0xff) / 255, g: Double((v >> 8) & 0xff) / 255, b: Double(v & 0xff) / 255, a: 1)
    }
}
