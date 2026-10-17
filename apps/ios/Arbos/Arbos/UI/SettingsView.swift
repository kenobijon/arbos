// Settings, Apple-Account style in Arbos colours (D80, Jacob 2026-10-17): a
// centred profile header (large avatar, bold name, grey handle), then a few
// short grouped cards of icon-tile rows that push to subpages. Everything that
// was on the old flat form is still here, regrouped:
//
//   Account              Credits · Connected apps · On this iPhone
//   Agents & Computer    Machine
//   Notifications        Notifications (system permission + relay push)
//   Appearance           Theme · Surface (Glass / Matte)
//   Help & About         Send feedback · About · Relay (debug builds)
//   Sign out
//
// Reference: media/design-parity/reference/apple/apple-account-settings-light.png.
// Page #FCFCFC / #141414, cards = the assistant-bubble grey, our circular back
// button, 17 pt labels, muted monochrome icon tiles, hairlines inset past the tile.

import PhotosUI
import SwiftUI
import UIKit
import UserNotifications

struct SettingsView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @ObservedObject private var push = PushManager.shared
    @State private var confirmUnpair = false
    /// Connector errors — the root `notice` alert sits under this sheet and looks like a silent fail.
    @State private var connectorError: String?
    @State private var avatarItem: PhotosPickerItem?
    @State private var avatarBusy = false

    var body: some View {
        NavigationStack {
            SettingsPage(title: "Settings", onBack: { dismiss() }) {
                profileHeader
                SettingsGroup {
                    NavigationLink { CreditsPage() } label: {
                        SettingsRow(icon: "creditcard.fill", title: "Credits", value: creditsSummary)
                    }
                    if model.credentials != nil {
                        NavigationLink { ConnectorsPage(connectorError: $connectorError) } label: {
                            SettingsRow(icon: "link", title: "Connected apps", value: connectorsSummary)
                        }
                    }
                    if !model.isFixture {
                        SettingsRow(icon: "iphone", title: "On this iPhone", value: storageLine, chevron: false)
                    }
                }
                SettingsGroup(footer: model.machine.alwaysOn
                              ? "Your one cloud computer, always on. Agents are workspaces on it."
                              : "Your one cloud computer. A message to any agent wakes it.") {
                    NavigationLink { MachinePage() } label: {
                        SettingsRow(icon: "desktopcomputer", title: "Machine", value: machineSummary, dot: machineDotColor(model.machine.state, theme: theme))
                    }
                }
                SettingsGroup {
                    NavigationLink { NotificationsPage() } label: {
                        SettingsRow(icon: "bell.badge.fill", title: "Notifications", value: notificationsSummary)
                    }
                    NavigationLink { AppearancePage() } label: {
                        SettingsRow(icon: "circle.lefthalf.filled", title: "Appearance", value: appearanceSummary)
                    }
                }
                SettingsGroup {
                    Button {
                        dismiss()
                        DispatchQueue.main.asyncAfter(deadline: .now() + 0.45) { FeedbackCenter.shared.open(screen: "settings") }
                    } label: {
                        SettingsRow(icon: "exclamationmark.bubble.fill", title: "Send feedback")
                    }
                    .buttonStyle(.plain)
                    NavigationLink { AboutPage() } label: {
                        SettingsRow(icon: "info.circle.fill", title: "About", value: SettingsView.versionLine)
                    }
                    #if DEBUG
                    NavigationLink { RelayPage() } label: {
                        SettingsRow(icon: "network", title: "Relay", value: URL(string: model.relayUrl)?.host ?? model.relayUrl)
                    }
                    #endif
                }
                SettingsGroup {
                    Button(role: .destructive) { confirmUnpair = true } label: {
                        Text(model.tokens != nil ? "Sign out" : "Sign out this device")
                            .font(.system(size: Theme.body))
                            .foregroundStyle(theme.danger)
                            .frame(maxWidth: .infinity, minHeight: SettingsRow.height)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
            }
            .confirmationDialog("Sign out?", isPresented: $confirmUnpair, titleVisibility: .visible) {
                Button("Sign out", role: .destructive) {
                    if model.tokens != nil { model.signOut() } else { model.unpair() }
                    dismiss()
                }
            } message: {
                Text(model.tokens != nil ? "This device is signed out of the relay; sign in with GitHub to come back." : "You will need a new pairing code to connect again.")
            }
            .onAppear {
                model.refreshConnectors()
                Task { await push.refreshStatus() }
            }
            // After Google OAuth in Safari, refresh so "Needs sign-in" flips to Connected.
            .onChange(of: scenePhase) { _, phase in
                if phase == .active {
                    model.refreshConnectors()
                    Task { await push.refreshStatus() }
                }
            }
            // Steal connector notices so they appear above this sheet, not under it.
            .onChange(of: model.notice) { _, n in
                guard let text = n?.text else { return }
                let isConnector = text.hasPrefix("Connect ")
                    || text.hasPrefix("Connectors:")
                    || text.hasPrefix("Disconnect:")
                    || text.hasPrefix("Could not update profile picture:")
                guard isConnector else { return }
                connectorError = text
                model.notice = nil
            }
            .alert("Arbos", isPresented: Binding(
                get: { connectorError != nil },
                set: { if !$0 { connectorError = nil } }
            )) {
                Button("OK", role: .cancel) { connectorError = nil }
            } message: {
                Text(connectorError ?? "")
            }
            .onChange(of: avatarItem) { _, item in
                guard let item else { return }
                Task { await applyAvatarPick(item) }
            }
        }
        .tint(theme.text)
        .preferredColorScheme(theme.isDark ? .dark : .light)
        .presentationBackground(theme.bg)
    }

    // MARK: Profile header

    /// Large avatar (D79: GitHub picture, else the coloured initials circle), bold
    /// name, grey handle — Apple's account header. Tap the picture to change it.
    private var profileHeader: some View {
        VStack(spacing: 6) {
            PhotosPicker(selection: $avatarItem, matching: .images) {
                PersonAvatar(url: model.user?.avatarUrl, handle: model.user?.login ?? "?", size: 104,
                             name: model.user?.name, id: model.user?.id,
                             colorIndex: model.user?.avatarColorIndexEffective)
                    .opacity(avatarBusy ? 0.55 : 1)
            }
            .buttonStyle(.plain)
            .disabled(model.user == nil || avatarBusy || model.isFixture)
            .accessibilityLabel("Change profile picture")
            .padding(.bottom, 8)
            Text(model.user?.name ?? model.user?.login ?? "Arbos")
                .font(.system(size: 28, weight: .bold))
                .foregroundStyle(theme.text)
                .lineLimit(1)
                .minimumScaleFactor(0.8)
            Text(model.user.map { "@\($0.login)" } ?? (model.relayUrl.isEmpty ? "Not signed in" : model.relayUrl))
                .font(.system(size: Theme.body))
                .foregroundStyle(theme.muted)
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .frame(maxWidth: .infinity)
        .padding(.top, 14)
        .padding(.bottom, 22)
    }

    private func applyAvatarPick(_ item: PhotosPickerItem) async {
        avatarBusy = true
        defer {
            avatarBusy = false
            avatarItem = nil
        }
        guard let data = try? await item.loadTransferable(type: Data.self),
              let img = UIImage(data: data) else { return }
        let square = Self.squareAvatar(img, side: 512)
        guard let jpeg = square.jpegData(compressionQuality: 0.85) else { return }
        _ = await model.setUserAvatar(jpeg: jpeg)
    }

    /// Centre-crop to a square and resize (same idea as group edit).
    private static func squareAvatar(_ image: UIImage, side: CGFloat) -> UIImage {
        let s = min(image.size.width, image.size.height)
        let rect = CGRect(x: (image.size.width - s) / 2, y: (image.size.height - s) / 2, width: s, height: s)
        let renderer = UIGraphicsImageRenderer(size: CGSize(width: side, height: side))
        return renderer.image { _ in
            guard let cg = image.cgImage?.cropping(to: rect) else {
                image.draw(in: CGRect(origin: .zero, size: CGSize(width: side, height: side)))
                return
            }
            UIImage(cgImage: cg, scale: image.scale, orientation: image.imageOrientation)
                .draw(in: CGRect(origin: .zero, size: CGSize(width: side, height: side)))
        }
    }

    // MARK: Row summaries

    private var creditsSummary: String? {
        guard let c = model.credits else { return nil }
        return String(format: "$%.0f left", c.balanceUsd)
    }

    private var connectorsSummary: String? {
        let n = model.connectors.items.filter { $0.status == .connected }.count
        return n == 0 ? nil : "\(n) connected"
    }

    private var machineSummary: String {
        if model.machine.alwaysOn, !model.machine.held, model.machineBusy { return "Restarting…" }
        return model.machine.label
    }

    private var notificationsSummary: String {
        switch push.status {
        case .authorized, .provisional, .ephemeral: return "On"
        case .denied: return "Off"
        case .notDetermined: return "Not asked"
        @unknown default: return ""
        }
    }

    private var appearanceSummary: String {
        let a = Appearance(rawValue: model.appearanceSetting)?.label ?? "System"
        return "\(a) · \(model.surface.label)"
    }

    /// "12.4 MB · 23 chats" from the on-device store (#929).
    private var storageLine: String {
        let u = model.storeUsage
        let size = ByteCountFormatter.string(fromByteCount: u.bytes, countStyle: .file)
        return u.docs == 0 ? "Nothing saved yet" : "\(size) · \(u.docs) chat\(u.docs == 1 ? "" : "s")"
    }

    static func relative(_ ms: Int64) -> String {
        let date = Date(timeIntervalSince1970: Double(ms) / 1000)
        let f = RelativeDateTimeFormatter()
        f.unitsStyle = .short
        return f.localizedString(for: date, relativeTo: Date())
    }

    static var versionLine: String {
        let info = Bundle.main.infoDictionary
        let v = info?["CFBundleShortVersionString"] as? String ?? "?"
        let b = info?["CFBundleVersion"] as? String ?? "?"
        return "\(v) (\(b))"
    }
}

// MARK: - Building blocks (Apple structure, Arbos skin)

/// A settings screen: our circular back button, centred title, scrolling content on the page colour.
struct SettingsPage<Content: View>: View {
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    let title: String
    var onBack: (() -> Void)? = nil
    @ViewBuilder var content: () -> Content

    var body: some View {
        ZStack(alignment: .top) {
            theme.bg.ignoresSafeArea()
            ScrollView {
                VStack(spacing: 24) {
                    // Clear the custom header + the soft end of `headerBlur`
                    // so the first card (Credits Balance, etc.) is not frosted.
                    Color.clear.frame(height: 72)
                    content()
                    Color.clear.frame(height: 24)
                }
                .padding(.horizontal, 20)
            }
            // Apple Account pull-down: frosted fade under the title so the
            // avatar softens as it scrolls / rubber-bands into the header.
            headerBlur
            HStack {
                Button { (onBack ?? { dismiss() })() } label: {
                    GlassCircle {
                        Image(systemName: "chevron.left")
                            .font(.system(size: 17, weight: .semibold))
                            .foregroundStyle(theme.text)
                    }
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Back")
                Spacer()
            }
            .padding(.horizontal, 20)
            .padding(.top, 6)
            .overlay {
                Text(title)
                    .font(.system(size: Theme.body, weight: .semibold))
                    .foregroundStyle(theme.text)
                    .padding(.top, 6)
            }
        }
        .toolbar(.hidden, for: .navigationBar)
        .navigationBarBackButtonHidden(true)
        // Hiding the system back button also disables the edge-swipe pop; re-enable it.
        .background(InteractivePopGestureEnabler())
    }

    /// Strip under the back button + title — Apple Account always frosts what
    /// peeks under the nav on pull-down, so Settings keeps the material even
    /// when the rest of the app is on Matte.
    private var headerBlur: some View {
        ZStack {
            Rectangle().fill(.ultraThinMaterial)
            theme.bg.opacity(0.78)
        }
        .frame(height: 110)
        .mask(LinearGradient(colors: [.black, .black, .clear], startPoint: .top, endPoint: .bottom))
        .ignoresSafeArea(edges: .top)
        .allowsHitTesting(false)
    }
}

/// One rounded inset card holding rows, with hairlines inset past the icon tiles and an optional footer.
struct SettingsGroup<Rows: View>: View {
    @Environment(\.theme) private var theme
    var header: String? = nil
    var footer: String? = nil
    @ViewBuilder var rows: () -> Rows

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let header {
                Text(header.uppercased())
                    .font(.system(size: Theme.tiny, weight: .medium))
                    .foregroundStyle(theme.muted)
                    .padding(.leading, 16)
            }
            _VariadicView.Tree(SettingsGroupLayout(separator: theme.separator)) { rows() }
                .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(theme.bubbleAssistant))
            if let footer {
                Text(footer)
                    .font(.system(size: Theme.tiny))
                    .foregroundStyle(theme.muted)
                    .padding(.horizontal, 16)
            }
        }
    }
}

/// Lays the group's children out vertically with a hairline between them, inset to the label.
private struct SettingsGroupLayout: _VariadicView_UnaryViewRoot {
    let separator: Color

    @ViewBuilder
    func body(children: _VariadicView.Children) -> some View {
        let last = children.last?.id
        VStack(spacing: 0) {
            ForEach(children) { child in
                child
                if child.id != last {
                    Rectangle()
                        .fill(separator)
                        .frame(height: 1 / UIScreen.main.scale)
                        .padding(.leading, SettingsRow.separatorInset)
                }
            }
        }
    }
}

/// Icon tile · title · optional trailing value · chevron. 48 pt tall.
struct SettingsRow: View {
    @Environment(\.theme) private var theme
    let icon: String
    let title: String
    var value: String? = nil
    var chevron: Bool = true
    /// Small status dot drawn on the tile (machine state).
    var dot: Color? = nil

    static let height: CGFloat = 48
    static let tile: CGFloat = 30
    /// 16 pt padding + 30 pt tile + 14 pt gap: separators start under the label.
    static let separatorInset: CGFloat = 60

    var body: some View {
        HStack(spacing: 14) {
            ZStack(alignment: .bottomTrailing) {
                RoundedRectangle(cornerRadius: 7.5, style: .continuous)
                    .fill(theme.isDark ? Color(hex: "#5C5C60")! : Color(hex: "#6E6E73")!)
                    .frame(width: Self.tile, height: Self.tile)
                    .overlay(
                        Image(systemName: icon)
                            .font(.system(size: 15, weight: .semibold))
                            .foregroundStyle(.white)
                    )
                if let dot {
                    Circle().fill(dot).frame(width: 9, height: 9)
                        .overlay(Circle().strokeBorder(theme.bubbleAssistant, lineWidth: 1.5))
                        .offset(x: 3, y: 3)
                }
            }
            Text(title)
                .font(.system(size: Theme.body))
                .foregroundStyle(theme.text)
                .lineLimit(1)
            Spacer(minLength: 8)
            if let value, !value.isEmpty {
                Text(value)
                    .font(.system(size: Theme.body))
                    .foregroundStyle(theme.muted)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .frame(maxWidth: 170, alignment: .trailing)
            }
            if chevron {
                Image(systemName: "chevron.right")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(theme.placeholder)
            }
        }
        .padding(.horizontal, 16)
        .frame(minHeight: Self.height)
        .contentShape(Rectangle())
    }
}

/// Label · trailing value, for information rows inside a subpage card.
struct SettingsValueRow<Trailing: View>: View {
    @Environment(\.theme) private var theme
    let title: String
    @ViewBuilder var trailing: () -> Trailing

    init(_ title: String, value: String) where Trailing == Text {
        self.title = title
        self.trailing = { Text(value) }
    }

    init(_ title: String, @ViewBuilder trailing: @escaping () -> Trailing) {
        self.title = title
        self.trailing = trailing
    }

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Text(title).font(.system(size: Theme.body)).foregroundStyle(theme.text)
            Spacer(minLength: 8)
            trailing()
                .font(.system(size: Theme.body))
                .foregroundStyle(theme.muted)
                .multilineTextAlignment(.trailing)
        }
        .padding(.horizontal, 16)
        .frame(minHeight: SettingsRow.height)
    }
}

/// A row that picks one option (checkmark on the chosen one), Apple's list-picker look.
struct SettingsChoiceRow: View {
    @Environment(\.theme) private var theme
    let title: String
    let selected: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack {
                Text(title).font(.system(size: Theme.body)).foregroundStyle(theme.text)
                Spacer()
                if selected {
                    Image(systemName: "checkmark").font(.system(size: 15, weight: .semibold)).foregroundStyle(theme.text)
                }
            }
            .padding(.horizontal, 16)
            .frame(minHeight: SettingsRow.height)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

/// A text action row (Start machine, Connect, Switch relay).
struct SettingsActionRow: View {
    @Environment(\.theme) private var theme
    let title: String
    var destructive: Bool = false
    var disabled: Bool = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Text(title)
                .font(.system(size: Theme.body))
                .foregroundStyle(destructive ? theme.danger : theme.text)
                .frame(maxWidth: .infinity, minHeight: SettingsRow.height, alignment: .leading)
                .padding(.horizontal, 16)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(disabled)
        .opacity(disabled ? 0.5 : 1)
    }
}

// MARK: - Subpages

struct CreditsPage: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme

    var body: some View {
        SettingsPage(title: "Credits") {
            if let c = model.credits {
                SettingsGroup(footer: "Credits pay for your agents' model use. Machine time is billed separately.") {
                    SettingsValueRow("Balance", value: String(format: "$%.2f left of $%.0f", c.balanceUsd, c.grantedUsd))
                    if let cap = c.dailyCapUsd {
                        SettingsValueRow("Today", value: String(format: "$%.2f of $%.0f cap", c.spentTodayUsd, cap))
                    } else {
                        SettingsValueRow("Today", value: String(format: "$%.2f", c.spentTodayUsd))
                    }
                }
            } else {
                SettingsGroup { SettingsValueRow("Balance", value: "Not available yet") }
            }
        }
    }
}

struct MachinePage: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme

    var body: some View {
        SettingsPage(title: "Machine") {
            SettingsGroup {
                SettingsValueRow("State") {
                    HStack(spacing: 7) {
                        if model.machine.alwaysOn, !model.machine.held, model.machineBusy {
                            ProgressView().controlSize(.mini).tint(theme.muted)
                            Text("Restarting…")
                        } else {
                            Circle().fill(machineDotColor(model.machine.state, theme: theme)).frame(width: 8, height: 8)
                            Text(model.machine.label)
                        }
                    }
                }
                if let since = model.machine.since, since > 0 {
                    SettingsValueRow("Since", value: SettingsView.relative(since))
                }
                SettingsValueRow("Spec", value: model.machine.spec.isEmpty ? (model.machineError ?? "Not reported yet") : model.machine.spec)
                if model.machine.reported {
                    if model.machine.alwaysOn {
                        SettingsValueRow("Machine time today") {
                            VStack(alignment: .trailing, spacing: 2) {
                                Text(String(format: "$%.2f", model.machine.machineTodayUsd))
                                Text("Not taken from credits").font(.system(size: Theme.tiny))
                            }
                        }
                    } else {
                        SettingsValueRow("Spend today", value: String(format: "$%.2f of $%.0f", model.machine.spendTodayUsd, model.machine.spendCapUsd))
                    }
                    SettingsValueRow("Workspaces", value: "\(model.machine.workspaces.values.filter { $0 == .active }.count) running · \(model.machine.workspaces.count) total")
                    if let id = model.machine.sandboxId, !id.isEmpty {
                        SettingsValueRow("Sandbox") {
                            Text(id).font(.system(size: 12, design: .monospaced)).lineLimit(1).truncationMode(.middle)
                        }
                    }
                }
            }
            SettingsGroup(footer: model.machine.alwaysOn
                          ? "Your one cloud computer, always on. Agents are workspaces on it. Machine time is billed separately from your credits."
                          : "Your one cloud computer. Agents are workspaces on it; a message to any agent wakes it.") {
                if model.machine.alwaysOn {
                    if model.machine.held {
                        SettingsActionRow(title: "Start machine", disabled: model.machineBusy) { model.startMachine() }
                    } else {
                        SettingsActionRow(title: "Restart machine", disabled: model.machineBusy) { model.restartMachine() }
                    }
                } else {
                    switch model.machine.state {
                    case .awake, .waking:
                        SettingsActionRow(title: "Stop machine", destructive: true, disabled: model.machineBusy) { model.stopMachine() }
                    case .hibernatedLight, .hibernatedDeep, .stoppedByUser, .error:
                        SettingsActionRow(title: "Start machine", disabled: model.machineBusy) { model.startMachine() }
                    }
                }
            }
        }
    }
}

struct ConnectorsPage: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Binding var connectorError: String?
    /// Provider whose token paste field is open (`nil` = none).
    @State private var tokenFor: String?
    @State private var tokenDraft = ""

    var body: some View {
        SettingsPage(title: "Connected apps") {
            SettingsGroup(footer: "Connect Gmail, Calendar, Slack, GitHub, or Harness Comms so your agents can use them. Tokens stay on the relay; the agent asks before it sends.") {
                ForEach(catalogProviders) { p in
                    connectorRow(p)
                }
            }
        }
        .onAppear { model.refreshConnectors() }
    }

    /// Full relay catalog (Google, Slack, GitHub, Harness Comms), or stand-ins before load.
    private var catalogProviders: [ConnectorProvider] {
        let fromRelay = model.connectors.providers
        if !fromRelay.isEmpty { return fromRelay }
        if model.connectors.loaded { return [] }
        return [
            ConnectorProvider(provider: "google", label: "Google (Gmail, Calendar)", connect: "oauth", oauthConfigured: true),
            ConnectorProvider(provider: "slack", label: "Slack", connect: "oauth_or_token", oauthConfigured: true),
            ConnectorProvider(provider: "github", label: "GitHub", connect: "oauth_or_token", oauthConfigured: true),
            ConnectorProvider(provider: "harness_comms", label: "Harness Comms (Beeper + Gmail)", connect: "token", oauthConfigured: true),
        ]
    }

    @ViewBuilder
    private func connectorRow(_ p: ConnectorProvider) -> some View {
        let cn = model.connectors.connector(for: p.provider)
        let connected = cn?.status == .connected
        let tokenOpen = tokenFor == p.provider
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .center, spacing: 12) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(p.label).font(.system(size: Theme.body)).foregroundStyle(theme.text)
                    Text(connectorSubline(p, cn: cn))
                        .font(.system(size: Theme.tiny))
                        .foregroundStyle(theme.muted)
                }
                Spacer(minLength: 8)
                if connected, let id = cn?.id {
                    Button("Disconnect") { model.disconnectConnector(id: id) }
                        .font(.system(size: Theme.small, weight: .semibold))
                        .buttonStyle(.borderless)
                        .foregroundStyle(theme.text)
                        .disabled(model.connectorsBusy)
                } else {
                    if p.hasOauth {
                        Button("Connect") { startOauth(p) }
                            .font(.system(size: Theme.small, weight: .semibold))
                            .buttonStyle(.borderless)
                            .foregroundStyle(theme.accent)
                            .disabled(model.connectorsBusy || model.isFixture)
                    }
                    if p.acceptsToken {
                        Button(tokenOpen ? "Cancel" : "Token…") {
                            if tokenOpen {
                                tokenFor = nil
                                tokenDraft = ""
                            } else {
                                tokenFor = p.provider
                                tokenDraft = ""
                            }
                        }
                        .font(.system(size: Theme.small, weight: .semibold))
                        .buttonStyle(.borderless)
                        .foregroundStyle(p.hasOauth ? theme.text : theme.accent)
                        .disabled(model.connectorsBusy || model.isFixture)
                    }
                }
            }
            if tokenOpen {
                HStack(spacing: 8) {
                    SecureField("Paste a token, then Save", text: $tokenDraft)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .font(.system(size: Theme.small))
                        .foregroundStyle(theme.text)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 8)
                        .background(theme.field, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                        .overlay(
                            RoundedRectangle(cornerRadius: 8, style: .continuous)
                                .strokeBorder(theme.fieldBorder, lineWidth: 1)
                        )
                    Button("Save") { saveToken(p) }
                        .font(.system(size: Theme.small, weight: .semibold))
                        .buttonStyle(.borderless)
                        .foregroundStyle(theme.accent)
                        .disabled(model.connectorsBusy || model.isFixture
                                  || tokenDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .frame(minHeight: 56)
    }

    private func startOauth(_ p: ConnectorProvider) {
        // Avoid a silent fail: without a client on the relay, Connect only 409s.
        if model.connectors.loaded, !p.oauthConfigured, !model.connectors.mock {
            connectorError = "Connect \(p.label): OAuth client not configured on the relay."
                + (p.acceptsToken ? " Use Token… with an API key or personal access token instead." : "")
            return
        }
        tokenFor = nil
        tokenDraft = ""
        model.connectConnector(provider: p.provider)
    }

    private func saveToken(_ p: ConnectorProvider) {
        let token = tokenDraft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !token.isEmpty else { return }
        tokenFor = nil
        tokenDraft = ""
        model.connectConnector(provider: p.provider, token: token)
    }

    private func connectorSubline(_ p: ConnectorProvider, cn: Connector?) -> String {
        if let cn, cn.status == .connected {
            return cn.label.isEmpty ? ConnectorStatus.connected.label : cn.label
        }
        // Prefer the real reason over a stuck "Needs sign-in" from a failed start.
        if model.connectors.loaded, !p.oauthConfigured, !model.connectors.mock, p.hasOauth {
            if p.acceptsToken {
                return "OAuth not configured — connect with a token"
            }
            return "OAuth client not configured on the relay"
        }
        if !p.hasOauth, p.acceptsToken {
            return "API key: use Token…"
        }
        if let err = cn?.lastError, !err.isEmpty, cn?.status != .connected {
            return err
        }
        if let cn, cn.status == .needsAuth {
            return ConnectorStatus.needsAuth.label
        }
        if let cn { return cn.status.label }
        return "Not connected"
    }
}

struct NotificationsPage: View {
    @Environment(\.theme) private var theme
    @ObservedObject private var push = PushManager.shared

    var body: some View {
        SettingsPage(title: "Notifications") {
            SettingsGroup(footer: "A banner with the sender's picture when an agent finishes or a person writes, only while you are not looking. Permission lives in iOS Settings.") {
                SettingsValueRow("Allowed", value: permissionLabel)
                if let relay = push.relayPushEnabled {
                    SettingsValueRow("Push from the relay", value: relay ? "On" : "Off")
                }
                SettingsActionRow(title: "Open iOS Settings") {
                    if let url = URL(string: UIApplication.openNotificationSettingsURLString) {
                        UIApplication.shared.open(url)
                    }
                }
            }
        }
    }

    private var permissionLabel: String {
        switch push.status {
        case .authorized: return "Yes"
        case .provisional: return "Quietly"
        case .ephemeral: return "For now"
        case .denied: return "No"
        case .notDetermined: return "Not asked yet"
        @unknown default: return ""
        }
    }
}

struct AppearancePage: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme

    var body: some View {
        SettingsPage(title: "Appearance") {
            SettingsGroup(header: "Theme") {
                ForEach(Appearance.allCases) { a in
                    SettingsChoiceRow(title: a.label, selected: model.appearanceSetting == a.rawValue) {
                        model.appearanceSetting = a.rawValue
                    }
                }
            }
            SettingsGroup(header: "Surface", footer: "Glass blurs what scrolls under the header and the controls; Matte keeps them solid.") {
                ForEach(Surface.allCases) { s in
                    SettingsChoiceRow(title: s.label, selected: model.surfaceSetting == s.rawValue) {
                        model.surfaceSetting = s.rawValue
                    }
                }
            }
        }
    }
}

struct AboutPage: View {
    @Environment(\.theme) private var theme

    var body: some View {
        SettingsPage(title: "About") {
            SettingsGroup {
                SettingsValueRow("Version", value: SettingsView.versionLine)
                SettingsValueRow("Protocol", value: "room proto \(protoVersion) · Loro")
            }
        }
    }
}

#if DEBUG
/// Self-hosters point the app at their own relay here (debug builds only, as before).
struct RelayPage: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    @State private var relayUrl = ""

    var body: some View {
        SettingsPage(title: "Relay") {
            SettingsGroup(footer: "Switching relays signs this device out of the current one; sessions stay in the Keychain.") {
                HStack {
                    TextField("", text: $relayUrl, prompt: Text("https://relay.example.com").foregroundStyle(theme.placeholder))
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .font(.system(size: Theme.body))
                        .foregroundStyle(theme.text)
                        .onSubmit(applyRelay)
                }
                .padding(.horizontal, 16)
                .frame(minHeight: SettingsRow.height)
                if RelayEndpoint(relayUrl).httpUrl != model.relayUrl {
                    SettingsActionRow(title: "Switch relay", action: applyRelay)
                }
            }
            SettingsGroup {
                SettingsValueRow("Device") {
                    Text(model.deviceId).font(.system(size: 12, design: .monospaced)).lineLimit(1).truncationMode(.middle)
                }
                SettingsValueRow("Link", value: linkLabel)
            }
        }
        .onAppear { relayUrl = model.relayUrl }
    }

    private var linkLabel: String {
        switch model.link {
        case .connecting: return "Connecting…"
        case .live: return "Live"
        case .reconnecting: return "Reconnecting…"
        }
    }

    private func applyRelay() {
        let trimmed = relayUrl.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return }
        model.setRelayUrl(trimmed)
        dismiss()
    }
}
#endif
