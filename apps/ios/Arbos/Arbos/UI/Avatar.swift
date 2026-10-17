// Telegram-style avatars (D79): a coloured gradient circle with initials.
// `AgentAvatar` draws a project (name initials, colour from its id or the
// owner's pick); people go through `PersonAvatar` (picture first). The
// palette, derivation and initials rule live in Shared/AvatarPalette.swift
// so the notification extension draws the identical circle.

import SwiftUI

/// The circle with initials. Size is the diameter; text ≈ 42 % of it.
struct InitialsAvatar: View {
    @Environment(\.theme) private var theme
    let initials: String
    let colorIndex: Int
    var size: CGFloat = 48

    init(name: String?, handle: String?, id: String, override: Int? = nil, size: CGFloat = 48) {
        self.initials = AvatarPalette.initials(name: name, handle: handle)
        self.colorIndex = AvatarPalette.clamp(override) ?? AvatarPalette.index(for: id)
        self.size = size
    }

    init(initials: String, colorIndex: Int, size: CGFloat = 48) {
        self.initials = initials
        self.colorIndex = colorIndex
        self.size = size
    }

    var body: some View {
        ZStack {
            Circle().fill(AvatarPalette.gradient(colorIndex, dark: theme.isDark))
            Text(initials)
                .font(.system(size: size * AvatarPalette.initialsScale, weight: .semibold))
                .foregroundStyle(.white)
                .lineLimit(1)
                .minimumScaleFactor(0.6)
                // Optical centre: cap-height text sits a hair high in a circle.
                .offset(y: size * 0.01)
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

extension AvatarPalette {
    static func gradient(_ index: Int, dark: Bool) -> LinearGradient {
        let s = stops(index, dark: dark)
        return LinearGradient(colors: [Color(uiColor: s.top), Color(uiColor: s.bottom)], startPoint: .top, endPoint: .bottom)
    }
}

/// An agent: initials of its name on its colour, with the optional machine dot.
/// `working` / `still` are accepted for call-site compatibility (the art no longer animates).
struct AgentAvatar: View {
    let project: Project
    let theme: Theme
    var size: CGFloat = Theme.avatar
    var dot: Color? = nil
    var working: Bool = false
    var still: Bool = false

    var body: some View {
        ZStack(alignment: .bottomTrailing) {
            InitialsAvatar(name: project.name, handle: mentionSlug(project.name), id: project.id,
                           override: project.effectiveAvatarColorIndex, size: size)
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

/// "Automatic" plus the seven swatches; the chosen one is ringed.
struct AvatarColorPicker: View {
    @Environment(\.theme) private var theme
    /// Preview initials / id for the automatic swatch.
    let name: String
    let id: String
    let current: Int?
    let onPick: (Int?) -> Void

    private var automaticIndex: Int { AvatarPalette.index(for: id) }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 9) {
                swatch(index: automaticIndex, selected: current == nil, label: "Automatic", automatic: true) { onPick(nil) }
                ForEach(0..<AvatarPalette.count, id: \.self) { i in
                    swatch(index: i, selected: current == i, label: AvatarPalette.names[i], automatic: false) { onPick(i) }
                }
            }
            Text(current == nil ? "Automatic: the colour comes from the agent's id, the same on every device." : "\(AvatarPalette.names[current!]), chosen by you.")
                .font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
        }
    }

    private func swatch(index: Int, selected: Bool, label: String, automatic: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            ZStack {
                Circle().fill(AvatarPalette.gradient(index, dark: theme.isDark))
                if automatic {
                    Text("A").font(.system(size: 12, weight: .semibold)).foregroundStyle(.white)
                }
                if selected {
                    Circle().strokeBorder(theme.text, lineWidth: 2).frame(width: 32, height: 32)
                }
            }
            .frame(width: 32, height: 32)
            .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label + (selected ? ", selected" : ""))
    }
}

/// Small coloured dot for machine / workspace state.
func machineDotColor(_ state: BoxState, theme: Theme) -> Color {
    switch state {
    case .awake: return theme.online
    case .waking: return theme.warn
    case .hibernatedLight, .hibernatedDeep, .stoppedByUser: return theme.muted.opacity(0.6)
    case .error: return theme.danger
    }
}
