// Tree-character agent avatars (Lottie). The bundle carries
// assets/avatars/trees/manifest.json (at its root) and the folder
// `lottie/<slug>/{idle,working,reply}.json`. A project shows its `avatar`
// slug from the relay (the relay defaults it to
// slugs[fnv1a64(project id) % n] over the manifest), else the same hash
// locally, deduplicated within the user's list. idle loops when quiet,
// working loops during a turn, reply plays once when the turn ends. Reduced
// motion shows the first idle frame. A slug whose art is not in the bundle
// yet (manifest `ready: false`) falls back to the Grok blob.

import SwiftUI
import Lottie

/// One colour set from the manifest `colors[]`. The TREE carries the colour
/// (`tree` plus its `tree_dark`/`tree_light` shading and a dark `face`); the
/// disc (`bg`) and the thin ring stay neutral and are never tinted. `mono`
/// stays selectable but is never a default (D74: agents are always coloured;
/// the white tree is only the brand mark). Entries without the v2 shading
/// hexes are not tintable (the v1 manifest), so the art renders as drawn.
struct TreeColor: Identifiable, Equatable, Hashable {
    let name: String
    let label: String
    let bg: String
    let ring: String
    let tree: String
    let face: String
    var treeDark: String? = nil
    var treeLight: String? = nil

    var id: String { name }
    var isMono: Bool { name == "mono" }
    /// Carries the v2 tree shading hexes, so the tint can be applied.
    var isTintable: Bool { !isMono && treeDark != nil && treeLight != nil }

    static let mono = TreeColor(name: "mono", label: "Mono", bg: "#000000", ring: "#ffffff", tree: "#ffffff", face: "#000000")

    /// The relay's palette names in its order (arbos-relay avatar.rs
    /// DEFAULT_COLORS). Name-only (no shading hexes) so nothing is tinted
    /// until a manifest `colors[]` with `tree_dark`/`tree_light` is bundled.
    static let builtin: [TreeColor] = [mono] + [
        ("red", "Red", "#FF3B5C"), ("orange", "Orange", "#FF7A00"), ("amber", "Amber", "#FFB000"),
        ("green", "Green", "#10B981"), ("teal", "Teal", "#14B8A6"), ("cyan", "Cyan", "#06B6D4"),
        ("blue", "Blue", "#1084FE"), ("indigo", "Indigo", "#5856D6"), ("violet", "Violet", "#8B5CF6"),
        ("pink", "Pink", "#FF2D9B"), ("rose", "Rose", "#F43F5E"), ("slate", "Slate", "#8E8E93"),
    ].map { TreeColor(name: $0.0, label: $0.1, bg: $0.2, ring: "#ffffff", tree: "#ffffff", face: $0.2) }

    /// A bare `#rrggbb` stored as the colour: the tree in that hue, shaded
    /// by hand, on the neutral disc.
    static func fromHex(_ hex: String) -> TreeColor? {
        guard Color(hex: hex) != nil else { return nil }
        return TreeColor(name: hex.lowercased(), label: hex, bg: "#000000", ring: "#ffffff", tree: hex, face: "#000000",
                         treeDark: TreeColor.shade(hex, by: -0.25), treeLight: TreeColor.shade(hex, by: 0.25))
    }

    /// Accent for swatches: the tree hue.
    var swatch: Color { Color(hex: tree) ?? .white }

    /// Lighten (+) or darken (−) a hex by a fraction, for hand-made sets.
    static func shade(_ hex: String, by f: Double) -> String? {
        var s = hex.trimmingCharacters(in: .whitespaces)
        if s.hasPrefix("#") { s.removeFirst() }
        guard s.count == 6, let v = UInt32(s, radix: 16) else { return nil }
        func ch(_ x: UInt32) -> Int {
            let d = Double(x)
            let r = f >= 0 ? d + (255 - d) * f : d * (1 + f)
            return max(0, min(255, Int(r.rounded())))
        }
        return String(format: "#%02x%02x%02x", ch((v >> 16) & 0xff), ch((v >> 8) & 0xff), ch(v & 0xff))
    }
}

struct TreeCharacter: Identifiable, Equatable {
    let slug: String
    let name: String
    /// Accent from the manifest, when it has one.
    let color: String?
    /// Manifest `ready` (art finished); the bundle check decides what shows.
    let ready: Bool

    var id: String { slug }
}

/// The bundled catalog: slugs in manifest order (the hash rule depends on it).
final class TreeAvatars {
    static let shared = TreeAvatars()

    /// Every manifest entry, in manifest order (the relay's hash contract).
    let all: [TreeCharacter]
    /// Entries whose idle animation is in the bundle — what the app can draw.
    let characters: [TreeCharacter]
    /// Colour sets, mono first (manifest `colors[]`, else the built-in list).
    let colors: [TreeColor]
    private let slugSet: Set<String>
    private var missing: Set<String> = []
    private let lock = NSLock()

    private init() {
        var list: [TreeCharacter] = []
        var palette: [TreeColor] = []
        if let url = Bundle.main.url(forResource: "manifest", withExtension: "json"),
           let data = try? Data(contentsOf: url),
           let json = try? JSONSerialization.jsonObject(with: data) {
            list = TreeAvatars.parse(manifest: json)
            palette = TreeAvatars.parseColors(manifest: json)
        }
        if palette.isEmpty { palette = TreeColor.builtin }
        if !palette.contains(where: \.isMono) { palette.insert(.mono, at: 0) }
        colors = palette
        if list.isEmpty, let dir = Bundle.main.url(forResource: "lottie", withExtension: nil),
           let names = try? FileManager.default.contentsOfDirectory(atPath: dir.path) {
            list = names.sorted().filter { !$0.hasPrefix(".") }.map { TreeCharacter(slug: $0, name: TreeAvatars.title($0), color: nil, ready: true) }
        }
        all = list
        characters = list.filter { Bundle.main.url(forResource: "idle", withExtension: "json", subdirectory: "lottie/\($0.slug)") != nil }
        slugSet = Set(characters.map(\.slug))
    }

    /// Accepts `{characters:[{slug,name,color}]}`, `{avatars:[…]}`, a bare
    /// array of objects, or an array of slugs.
    static func parse(manifest json: Any) -> [TreeCharacter] {
        var items: [Any] = []
        if let arr = json as? [Any] { items = arr }
        else if let obj = json as? [String: Any] {
            items = (obj["characters"] as? [Any]) ?? (obj["avatars"] as? [Any]) ?? (obj["trees"] as? [Any]) ?? []
            if items.isEmpty, let dict = (obj["characters"] as? [String: Any]) ?? (obj["avatars"] as? [String: Any]) {
                items = dict.keys.sorted().map { k in ["slug": k].merging(dict[k] as? [String: Any] ?? [:]) { a, _ in a } }
            }
        }
        return items.compactMap { raw in
            if let s = raw as? String { return TreeCharacter(slug: s, name: title(s), color: nil, ready: true) }
            guard let d = raw as? [String: Any], let slug = (d["slug"] as? String) ?? (d["id"] as? String) else { return nil }
            return TreeCharacter(slug: slug, name: (d["name"] as? String) ?? title(slug),
                                 color: (d["color"] as? String) ?? (d["accent"] as? String),
                                 ready: (d["ready"] as? Bool) ?? true)
        }
    }

    /// `colors: [{name|slug|id, label?, bg, ring, tree, face?}]`; a `mono`
    /// entry may omit hexes. Entries with no usable bg/tree are skipped.
    static func parseColors(manifest json: Any) -> [TreeColor] {
        guard let obj = json as? [String: Any] else { return [] }
        let raw = (obj["colors"] as? [Any]) ?? (obj["colours"] as? [Any]) ?? (obj["palette"] as? [Any]) ?? []
        return raw.compactMap { item -> TreeColor? in
            if let name = item as? String { return TreeColor.builtin.first { $0.name == name } }
            guard let d = item as? [String: Any],
                  let name = (d["slug"] as? String) ?? (d["name"] as? String) ?? (d["id"] as? String) else { return nil }
            let hex = { (k: String) -> String? in (d[k] as? String).flatMap { Color(hex: $0) != nil ? $0 : nil } }
            if name == "mono" {
                return TreeColor(name: "mono", label: (d["label"] as? String) ?? "Mono",
                                 bg: hex("bg") ?? "#000000", ring: hex("ring") ?? "#ffffff", tree: hex("tree") ?? "#ffffff", face: hex("face") ?? hex("bg") ?? "#000000")
            }
            guard let tree = hex("tree") ?? hex("fg") ?? hex("foreground") else { return nil }
            return TreeColor(name: name, label: (d["label"] as? String) ?? title(name),
                             bg: hex("bg") ?? "#000000", ring: hex("ring") ?? "#ffffff", tree: tree,
                             face: hex("face") ?? "#000000",
                             treeDark: hex("tree_dark") ?? hex("treeDark"), treeLight: hex("tree_light") ?? hex("treeLight"))
        }
    }

    /// True once the manifest carries the v2 shading hexes (the picker shows then).
    var colorsTintable: Bool { colors.contains { $0.isTintable } }

    func color(named name: String) -> TreeColor? {
        if let c = colors.first(where: { $0.name == name }) { return c }
        return name.hasPrefix("#") ? TreeColor.fromHex(name) : nil
    }

    static func title(_ slug: String) -> String {
        slug.split(whereSeparator: { $0 == "-" || $0 == "_" }).map { $0.prefix(1).uppercased() + $0.dropFirst() }.joined(separator: " ")
    }

    var isEmpty: Bool { characters.isEmpty }
    func has(_ slug: String) -> Bool { slugSet.contains(slug) }
    func character(_ slug: String) -> TreeCharacter? { characters.first { $0.slug == slug } }

    /// FNV-1a 64 over the project id, mod the count — the relay's
    /// `slugs[fnv1a64(id) % n]` (crates/arbos-relay/src/avatar.rs), so the
    /// local fallback lands on the same tree the relay would send.
    static func hashIndex(_ projectId: String, count: Int) -> Int {
        var h: UInt64 = 0xcbf29ce484222325
        for b in projectId.utf8 { h = (h ^ UInt64(b)) &* 0x100000001b3 }
        return Int(h % UInt64(max(count, 1)))
    }

    /// The relay's default for a project: hashed over the whole manifest.
    func relayDefault(_ projectId: String) -> String? {
        guard !all.isEmpty else { return nil }
        return all[TreeAvatars.hashIndex(projectId, count: all.count)].slug
    }

    /// Tintable colours in manifest order — the palette a default is drawn from.
    var coloredPalette: [TreeColor] { colors.filter(\.isTintable) }

    /// The colour an agent shows when nothing was chosen (D74: never mono).
    /// The relay's `default_color_for`: `colored[fnv1a64(id + ":color") % n]`
    /// over `colors[]` without `mono`, so both ends land on the same hue.
    func defaultColor(forProjectId projectId: String) -> TreeColor? {
        let palette = coloredPalette
        guard !palette.isEmpty else { return nil }
        return palette[TreeAvatars.hashIndex(projectId + ":color", count: palette.count)]
    }

    /// Slug per project: an explicit `avatar` the bundle can draw wins; the
    /// rest get the relay's hash over the drawable set, probing forward to
    /// the next free tree when it is taken. Projects are visited by id
    /// (ULIDs sort by creation), so an older agent keeps its tree when a
    /// newer one collides with it.
    func assign(_ projects: [Project]) -> [Id: String] {
        guard !characters.isEmpty else { return [:] }
        var out: [Id: String] = [:]
        var taken = Set<String>()
        for p in projects where p.avatar != nil && has(p.avatar!) {
            out[p.id] = p.avatar!
            taken.insert(p.avatar!)
        }
        let n = characters.count
        for p in projects.sorted(by: { $0.id < $1.id }) where out[p.id] == nil {
            var i = TreeAvatars.hashIndex(p.id, count: n)
            var tries = 0
            while taken.contains(characters[i].slug), tries < n { i = (i + 1) % n; tries += 1 }
            out[p.id] = characters[i].slug
            taken.insert(characters[i].slug)
        }
        return out
    }

    /// Animation for a slug and phase, nil when the file is missing.
    func animation(_ slug: String, _ phase: TreePhase) -> LottieAnimation? {
        let key = "\(slug)/\(phase.rawValue)"
        lock.lock(); let known = missing.contains(key); lock.unlock()
        if known { return nil }
        if let a = LottieAnimation.named(phase.rawValue, bundle: .main, subdirectory: "lottie/\(slug)") { return a }
        lock.lock(); missing.insert(key); lock.unlock()
        return nil
    }
}

enum TreePhase: String {
    case idle, working, reply
}

/// One animated tree. `working` drives idle → working → reply → idle.
struct TreeCharacterView: View {
    let slug: String
    var size: CGFloat
    /// nil = the art as drawn (mono).
    var color: TreeColor? = nil
    var working: Bool = false
    /// Static first frame regardless of the motion setting (pickers, lists of many).
    var still: Bool = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var phase: TreePhase = .idle

    private var catalog: TreeAvatars { TreeAvatars.shared }

    /// The phase that has an animation file; working/reply fall back to idle.
    private var shownPhase: TreePhase {
        catalog.animation(slug, phase) != nil ? phase : .idle
    }

    private var frozen: Bool { still || reduceMotion }

    var body: some View {
        let p = shownPhase
        tinted(LottieView(animation: catalog.animation(slug, p)))
            .playbackMode(frozen ? .paused(at: .progress(0)) : playback(p))
            .animationDidFinish { _ in if phase == .reply { phase = .idle } }
            .resizable()
            .id("\(p)|\(color?.name ?? "mono")")
            .frame(width: size, height: size)
            .onAppear { phase = working ? .working : .idle }
            .onChange(of: working) { was, now in
                if now { phase = .working }
                else if was, !frozen, catalog.animation(slug, .reply) != nil { phase = .reply }
                else { phase = .idle }
            }
            .accessibilityHidden(true)
    }

    /// Runtime retint of the tree only: the `tree` layer's named fills and
    /// strokes `tree`, `tree_dark`, `tree_light` take the hue with its tonal
    /// shading and `face` goes dark. `bg` and `ring` are never touched.
    private func tinted(_ view: LottieView<EmptyView>) -> LottieView<EmptyView> {
        guard let color, color.isTintable, let dark = color.treeDark, let light = color.treeLight else { return view }
        var v = view
        let pairs: [(String, String)] = [
            ("tree.**.tree.Color", color.tree),
            ("tree.**.tree_dark.Color", dark),
            ("tree.**.tree_light.Color", light),
            ("tree.**.face.Color", color.face),
        ]
        for (path, hex) in pairs {
            guard let c = TreeCharacterView.lottieColor(hex) else { continue }
            v = v.valueProvider(ColorValueProvider(c), for: AnimationKeypath(keypath: path))
        }
        return v
    }

    static func lottieColor(_ hex: String) -> LottieColor? {
        var s = hex.trimmingCharacters(in: .whitespaces)
        if s.hasPrefix("#") { s.removeFirst() }
        guard s.count == 6, let v = UInt32(s, radix: 16) else { return nil }
        return LottieColor(r: Double((v >> 16) & 0xff) / 255, g: Double((v >> 8) & 0xff) / 255, b: Double(v & 0xff) / 255, a: 1)
    }

    private func playback(_ p: TreePhase) -> LottiePlaybackMode {
        switch p {
        case .idle, .working: return .playing(.fromProgress(0, toProgress: 1, loopMode: .loop))
        case .reply: return .playing(.fromProgress(0, toProgress: 1, loopMode: .playOnce))
        }
    }
}

/// The agent avatar everywhere: the tree when the bundle has it, else the blob.
struct AgentAvatar: View {
    let project: Project
    let theme: Theme
    var size: CGFloat = Theme.avatar
    var dot: Color? = nil
    var working: Bool = false
    var still: Bool = false
    @EnvironmentObject private var model: AppModel

    var body: some View {
        ZStack(alignment: .bottomTrailing) {
            if let slug = model.avatarSlug(for: project) {
                TreeCharacterView(slug: slug, size: size, color: model.avatarColor(for: project), working: working, still: still)
            } else {
                BlobAvatar(project: project, theme: theme, size: size)
            }
            if let dot {
                // Grok's status badge is 12 pt on a 48 pt row avatar; smaller avatars scale.
                let d = min(size * 0.36, 12)
                Circle()
                    .fill(dot)
                    .frame(width: d, height: d)
                    .overlay(Circle().strokeBorder(theme.bg, lineWidth: 2))
                    .offset(x: size * 0.08, y: size * 0.08)
            }
        }
        .frame(width: size, height: size)
    }
}

/// Grid of every tree; the current one ringed. Used by the agent profile.
struct TreePickerGrid: View {
    let current: String?
    var color: TreeColor? = nil
    let onPick: (String) -> Void
    @Environment(\.theme) private var theme

    private let columns = Array(repeating: GridItem(.flexible(), spacing: 10), count: 4)

    var body: some View {
        LazyVGrid(columns: columns, spacing: 12) {
            ForEach(TreeAvatars.shared.characters) { c in
                Button { onPick(c.slug) } label: {
                    VStack(spacing: 6) {
                        TreeCharacterView(slug: c.slug, size: 56, color: color, still: c.slug != current)
                            .padding(6)
                            .background(
                                RoundedRectangle(cornerRadius: 16, style: .continuous)
                                    .fill(c.slug == current ? theme.field : .clear)
                            )
                            .overlay(
                                RoundedRectangle(cornerRadius: 16, style: .continuous)
                                    .strokeBorder(c.slug == current ? theme.text : .clear, lineWidth: 2)
                            )
                        Text(c.name).font(.system(size: 11)).foregroundStyle(c.slug == current ? theme.text : theme.muted).lineLimit(1)
                    }
                }
                .buttonStyle(.plain)
                .accessibilityLabel(c.name)
                .accessibilityAddTraits(c.slug == current ? .isSelected : [])
            }
        }
    }
}

/// Grid of colour sets for one tree: every cell shows the tree in that
/// colour (static), the current one animates. Mono is first.
struct TreeColorGrid: View {
    let slug: String
    let current: TreeColor?
    let onPick: (TreeColor) -> Void
    @Environment(\.theme) private var theme

    private let columns = Array(repeating: GridItem(.flexible(), spacing: 6), count: 7)

    var body: some View {
        LazyVGrid(columns: columns, spacing: 8) {
            ForEach(TreeAvatars.shared.colors.filter { $0.isMono || $0.isTintable }) { c in
                let selected = (current?.name ?? "mono") == c.name
                Button { onPick(c) } label: {
                    TreeCharacterView(slug: slug, size: 38, color: c.isMono ? nil : c, still: !selected)
                        .padding(4)
                        .background(Circle().fill(selected ? theme.field : .clear))
                        .overlay(Circle().strokeBorder(selected ? theme.text : .clear, lineWidth: 2))
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.plain)
                .accessibilityLabel(c.label)
                .accessibilityAddTraits(selected ? .isSelected : [])
            }
        }
    }
}
