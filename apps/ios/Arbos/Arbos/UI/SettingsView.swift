import SwiftUI

struct SettingsView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    #if DEBUG
    @State private var relayUrl = ""
    #endif
    @State private var confirmUnpair = false
    /// Connector errors — the root `notice` alert sits under this sheet and looks like a silent fail.
    @State private var connectorError: String?

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    HStack(spacing: 14) {
                        if let url = model.user?.avatarUrl.flatMap(URL.init(string:)) {
                            AsyncImage(url: url) { img in img.resizable().scaledToFill() } placeholder: { theme.field }
                                .frame(width: 56, height: 56)
                                .clipShape(Circle())
                        } else {
                            GlassCircle(size: 56) {
                                Image(systemName: "person.fill").font(.system(size: 22)).foregroundStyle(theme.text)
                            }
                        }
                        VStack(alignment: .leading, spacing: 3) {
                            Text(model.user?.name ?? model.user?.login ?? "Arbos")
                                .font(.system(size: Theme.body, weight: .semibold)).foregroundStyle(theme.text)
                            Text(model.user.map { "@\($0.login)" } ?? (model.relayUrl.isEmpty ? "Not signed in" : model.relayUrl))
                                .font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                                .lineLimit(1).truncationMode(.middle)
                        }
                    }
                    .padding(.vertical, 4)
                    if let c = model.credits {
                        LabeledContent("Credits", value: String(format: "$%.2f left of $%.0f", c.balanceUsd, c.grantedUsd))
                        if let cap = c.dailyCapUsd {
                            LabeledContent("Today", value: String(format: "$%.2f of $%.0f cap", c.spentTodayUsd, cap))
                        } else {
                            LabeledContent("Today", value: String(format: "$%.2f", c.spentTodayUsd))
                        }
                    }
                    if model.tokens != nil {
                        Button("Sign out", role: .destructive) { confirmUnpair = true }
                    }
                } header: {
                    Text("Account")
                }
                .listRowBackground(theme.card)

                Section {
                    HStack {
                        Text("State")
                        Spacer()
                        HStack(spacing: 7) {
                            if model.machine.alwaysOn, !model.machine.held, model.machineBusy {
                                ProgressView().controlSize(.mini).tint(theme.muted)
                                Text("Restarting…").foregroundStyle(theme.muted)
                            } else {
                                Circle().fill(machineDotColor(model.machine.state, theme: theme)).frame(width: 8, height: 8)
                                Text(model.machine.label).foregroundStyle(theme.muted)
                            }
                        }
                    }
                    if let since = model.machine.since, since > 0 {
                        LabeledContent("Since", value: SettingsView.relative(since))
                    }
                    LabeledContent("Spec") {
                        Text(model.machine.spec.isEmpty ? (model.machineError ?? "Not reported yet") : model.machine.spec)
                            .multilineTextAlignment(.trailing)
                    }
                    if model.machine.reported {
                        if model.machine.alwaysOn {
                            LabeledContent("Machine time today") {
                                VStack(alignment: .trailing, spacing: 2) {
                                    Text(String(format: "$%.2f", model.machine.machineTodayUsd))
                                    Text("Not taken from credits").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                                }
                            }
                        } else {
                            LabeledContent("Spend today", value: String(format: "$%.2f of $%.0f", model.machine.spendTodayUsd, model.machine.spendCapUsd))
                        }
                        LabeledContent("Workspaces", value: "\(model.machine.workspaces.values.filter { $0 == .active }.count) running · \(model.machine.workspaces.count) total")
                        if let id = model.machine.sandboxId, !id.isEmpty {
                            LabeledContent("Sandbox") {
                                Text(id).font(.system(size: 12, design: .monospaced)).lineLimit(1).truncationMode(.middle)
                            }
                        }
                    }
                    if model.machine.alwaysOn {
                        if model.machine.held {
                            Button("Start machine") { model.startMachine() }
                                .disabled(model.machineBusy)
                        } else {
                            Button("Restart machine") { model.restartMachine() }
                                .disabled(model.machineBusy)
                        }
                    } else {
                        switch model.machine.state {
                        case .awake, .waking:
                            Button("Stop machine", role: .destructive) { model.stopMachine() }
                                .disabled(model.machineBusy)
                        case .hibernatedLight, .hibernatedDeep, .stoppedByUser, .error:
                            Button("Start machine") { model.startMachine() }
                                .disabled(model.machineBusy)
                        }
                    }
                } header: {
                    Text("Machine")
                } footer: {
                    Text(model.machine.alwaysOn
                         ? "Your one cloud computer, always on. Projects are workspaces on it. Machine time is billed separately from your credits."
                         : "Your one cloud computer. Projects are workspaces on it; a message to any project wakes it.")
                }
                .listRowBackground(theme.card)

                Section {
                    Button {
                        dismiss()
                        DispatchQueue.main.asyncAfter(deadline: .now() + 0.45) { FeedbackCenter.shared.open(screen: "settings") }
                    } label: {
                        Label("Send feedback", systemImage: "exclamationmark.bubble").foregroundStyle(theme.text)
                    }
                } header: {
                    Text("Help")
                } footer: {
                    Text("A screenshot of the screen you were on plus a note. You can also shake the phone.")
                }
                .listRowBackground(theme.card)

                Section("Appearance") {
                    Picker("Theme", selection: $model.appearanceSetting) {
                        ForEach(Appearance.allCases) { a in Text(a.label).tag(a.rawValue) }
                    }
                    Picker("Surface", selection: $model.surfaceSetting) {
                        ForEach(Surface.allCases) { s in Text(s.label).tag(s.rawValue) }
                    }
                    .pickerStyle(.segmented)
                }
                .listRowBackground(theme.card)

                if model.credentials != nil {
                    Section {
                        ForEach(googleProviders) { p in
                            connectorRow(p)
                        }
                    } header: {
                        Text("Connectors")
                    } footer: {
                        Text("Connect Google so your agents can read Gmail and Calendar. Tokens stay on the relay; the agent asks before it sends.")
                    }
                    .listRowBackground(theme.card)
                }

                if !model.isFixture {
                    Section {
                        LabeledContent("On this iPhone", value: storageLine)
                    } header: {
                        Text("Storage")
                    } footer: {
                        Text("Chats and agents are kept on this iPhone so the app opens instantly and works offline. Signing out removes them.")
                    }
                    .listRowBackground(theme.card)
                }

                #if DEBUG
                Section {
                    TextField("https://relay.example.com", text: $relayUrl)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .onSubmit(applyRelay)
                    if RelayEndpoint(relayUrl).httpUrl != model.relayUrl {
                        Button("Switch relay", action: applyRelay)
                    }
                    LabeledContent("Device") {
                        Text(model.deviceId)
                            .font(.system(size: 12, design: .monospaced))
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    LabeledContent("Link", value: linkLabel)
                } header: {
                    Text("Advanced · Relay")
                } footer: {
                    Text("Self-hosters point the app at their own relay here. Switching relays signs this device out of the current one; sessions stay in the Keychain.")
                }
                .listRowBackground(theme.card)
                #endif

                if model.tokens == nil {
                    Section {
                        Button("Sign out this device", role: .destructive) { confirmUnpair = true }
                    }
                    .listRowBackground(theme.card)
                }

                Section("About") {
                    LabeledContent("Version", value: SettingsView.versionLine)
                    LabeledContent("Protocol", value: "room proto \(protoVersion) · Loro")
                }
                .listRowBackground(theme.card)
            }
            .scrollContentBackground(.hidden)
            .background(theme.bg)
            .tint(theme.text)
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
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
                #if DEBUG
                relayUrl = model.relayUrl
                #endif
                model.refreshConnectors()
            }
            // After Google OAuth in Safari, refresh so "Needs sign-in" flips to Connected.
            .onChange(of: scenePhase) { _, phase in
                if phase == .active { model.refreshConnectors() }
            }
            // Steal connector notices so they appear above this sheet, not under it.
            .onChange(of: model.notice) { _, n in
                guard let text = n?.text else { return }
                let isConnector = text.hasPrefix("Connect ")
                    || text.hasPrefix("Connectors:")
                    || text.hasPrefix("Disconnect:")
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
        }
        // Form headers/footers/LabeledContent/segmented pickers use system
        // label colours. Without this, Light appearance paints theme.card white
        // while the sheet stays on dark-mode system text (white-on-white).
        .preferredColorScheme(theme.isDark ? .dark : .light)
        .presentationBackground(theme.bg)
    }

    /// Google (Gmail + Calendar) from the catalog, or a local stand-in before load.
    private var googleProviders: [ConnectorProvider] {
        let fromRelay = model.connectors.providers.filter { $0.provider == "google" }
        if !fromRelay.isEmpty { return fromRelay }
        if model.connectors.loaded {
            return []
        }
        return [ConnectorProvider(
            provider: "google",
            label: "Google (Gmail, Calendar)",
            connect: "oauth",
            oauthConfigured: true
        )]
    }

    @ViewBuilder
    private func connectorRow(_ p: ConnectorProvider) -> some View {
        let cn = model.connectors.connector(for: p.provider)
        let connected = cn?.status == .connected
        HStack(alignment: .center, spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text(p.label).foregroundStyle(theme.text)
                Text(connectorSubline(p, cn: cn))
                    .font(.system(size: Theme.tiny))
                    .foregroundStyle(theme.muted)
            }
            Spacer(minLength: 8)
            if connected, let id = cn?.id {
                Button("Disconnect") {
                    model.disconnectConnector(id: id)
                }
                .buttonStyle(.borderless)
                .disabled(model.connectorsBusy)
            } else if p.hasOauth {
                Button("Connect") {
                    model.connectConnector(provider: p.provider)
                }
                .buttonStyle(.borderless)
                .foregroundStyle(theme.accent)
                .disabled(model.connectorsBusy || model.isFixture)
            }
        }
    }

    private func connectorSubline(_ p: ConnectorProvider, cn: Connector?) -> String {
        if let cn, cn.status == .connected {
            return cn.label.isEmpty ? ConnectorStatus.connected.label : cn.label
        }
        // Prefer the real reason over a stuck "Needs sign-in" from a failed start.
        if model.connectors.loaded, !p.oauthConfigured, !model.connectors.mock, p.hasOauth {
            return "OAuth client not configured on the relay"
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

    /// "12.4 MB · 23 chats" from the on-device store (#929).
    private var storageLine: String {
        let u = model.storeUsage
        let size = ByteCountFormatter.string(fromByteCount: u.bytes, countStyle: .file)
        return u.docs == 0 ? "Nothing saved yet" : "\(size) · \(u.docs) chat\(u.docs == 1 ? "" : "s")"
    }

    #if DEBUG
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
    #endif

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
