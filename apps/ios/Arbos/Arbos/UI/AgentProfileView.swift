// Agent pages. `AgentProfileView` is the Telegram-style profile (avatar,
// name, status, action tiles, info card, Media/Files/Links) opened from
// anywhere an agent shows; its Edit button opens `AgentEditSheet` — the
// original editor: tap-to-rename, colour, character picker, model, chats,
// sharing. Name/colour/avatar/description save with `PATCH /projects/{id}`.

import SwiftUI

struct AgentEditSheet: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    let projectId: Id
    @State private var name = ""
    @State private var editingName = false
    @State private var showModel = false
    @State private var showVoice = false
    @FocusState private var nameFocused: Bool

    private var project: Project? { model.project(projectId) }

    static let colorHexes = ["#1084FE", "#10B981", "#FF7A00", "#FF3B5C", "#FF2D9B", "#8B5CF6", "#8E5A3C", "#8E8E93", "#FFB000", "#14B8A6"]

    var body: some View {
        NavigationStack {
            ScrollView {
                if let project {
                    VStack(spacing: 0) {
                        hero(project)
                        card {
                            VStack(alignment: .leading, spacing: 12) {
                                if LeafAvatarsFlag.shared.on {
                                    // D92: leaf + colour; the hero above is the live preview.
                                    Text("Avatar").font(theme.font(Theme.body, weight: .medium)).foregroundStyle(theme.text)
                                    LeafPicker(project: project) { leaf, color in
                                        guard !project.shared else { return }
                                        model.pickAvatarLeaf(projectId, leaf: leaf, color: color)
                                    }
                                    .disabled(project.shared)
                                } else {
                                    Text("Avatar colour").font(theme.font(Theme.body, weight: .medium)).foregroundStyle(theme.text)
                                    AvatarColorPicker(name: project.name, id: project.id, current: project.effectiveAvatarColorIndex == project.avatarColorIndex ? project.avatarColorIndex : project.avatarColorIndex) { index in
                                        guard !project.shared else { return }
                                        model.pickAvatarColorIndex(projectId, index: index)
                                    }
                                    .disabled(project.shared)
                                }
                            }
                            .padding(.horizontal, 14)
                            .padding(.vertical, 12)
                        }
                        .padding(.horizontal, 16)
                        .padding(.top, 26)

                        card {
                            Button { showModel = true } label: {
                                row(icon: "cpu", title: "Model", value: "\(project.shortModel) · \(project.effort.label)")
                            }
                            .buttonStyle(.plain)
                            Button { showVoice = true } label: {
                                row(icon: "waveform", title: "Voice", value: voiceLabel(project))
                            }
                            .buttonStyle(.plain)
                            PaneRow(
                                leading: .tile("eye"),
                                title: "Camera vision",
                                subtitle: "On video calls \(project.name) can see your camera while it is on.",
                                trailing: AnyView(
                                    Toggle("", isOn: Binding(get: { project.visionEnabled }, set: { model.setVision(projectId, enabled: $0) }))
                                        .labelsHidden()
                                        .tint(theme.online)
                                        .disabled(project.shared)
                                        .accessibilityLabel("Camera vision")
                                )
                            )
                        }
                        .padding(.horizontal, 16)
                        .padding(.top, 14)

                        card {
                            row(icon: "desktopcomputer", title: "Workspace", value: workspaceLabel(project), chevron: false)
                            if project.spendCapUsd > 0 {
                                row(icon: "dollarsign.circle", title: "Spend today", value: String(format: "$%.2f of $%.0f", project.spendTodayUsd, project.spendCapUsd), chevron: false)
                            }
                        }
                        .padding(.horizontal, 16)
                        .padding(.top, 14)

                        Text(project.shared
                             ? "A shared agent. Its owner pays for it and can remove you any time."
                             : "Mention @\(mentionSlug(project.name)) in a chat with a person to bring this agent in.")
                            .font(theme.font(Theme.tiny)).foregroundStyle(theme.muted)
                            .multilineTextAlignment(.center)
                            .padding(.horizontal, 36).padding(.top, 18)
                        Color.clear.frame(height: 40)
                    }
                } else {
                    Text("This agent is gone.").foregroundStyle(theme.muted).padding(.top, 60)
                }
            }
            .scrollDismissesKeyboard(.interactively)
            .background(theme.bg)
            .navigationBarTitleDisplayMode(.inline)
            .toolbarBackground(theme.bg, for: .navigationBar)
            .toolbarBackground(.visible, for: .navigationBar)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    if editingName {
                        Button("Save") { commitName() }.font(theme.font(Theme.body, weight: .semibold))
                    } else {
                        Button("Done") { dismiss() }
                    }
                }
            }
            .sheet(isPresented: $showModel) {
                if let project { ModelPickerView(project: project).environmentObject(model).environment(\.theme, theme) }
            }
            .sheet(isPresented: $showVoice) {
                if let project { VoicePickerView(project: project).environmentObject(model).environment(\.theme, theme) }
            }
            .onAppear { name = project?.name ?? "" }
        }
        .tint(theme.text)
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
        .presentationBackground(theme.bg)
        .presentationCornerRadius(28)
        .preferredColorScheme(theme.isDark ? .dark : .light)
    }

    // MARK: Hero

    private func hero(_ project: Project) -> some View {
        VStack(spacing: 12) {
            AgentAvatar(project: project, theme: theme, size: 116, dot: machineDotColor(model.machine.state, theme: theme), working: model.isStreaming(project: project.id), idle: true)
                .padding(.top, 18)
                .id("\(project.effectiveAvatarColorIndex.map(String.init) ?? "auto")")
                .animation(.snappy, value: project.color)
            if editingName {
                TextField("", text: $name, prompt: Text("Agent name").foregroundStyle(theme.placeholder))
                    .font(theme.font(24, weight: .semibold))
                    .foregroundStyle(theme.text)
                    .multilineTextAlignment(.center)
                    .autocorrectionDisabled()
                    .focused($nameFocused)
                    .submitLabel(.done)
                    .onSubmit(commitName)
                    .padding(.horizontal, 16).frame(height: 46)
                    .background(RoundedRectangle(cornerRadius: 14, style: .continuous).fill(theme.field))
                    .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(theme.fieldBorder, lineWidth: 1))
                    .padding(.horizontal, 48)
            } else {
                Button {
                    guard !project.shared else { return }
                    name = project.name
                    editingName = true
                    nameFocused = true
                } label: {
                    HStack(spacing: 6) {
                        Text(project.name)
                            .font(theme.font(24, weight: .semibold))
                            .foregroundStyle(theme.text)
                            .lineLimit(1)
                        if !project.shared {
                            Image(systemName: "pencil").font(.system(size: 13, weight: .semibold)).foregroundStyle(theme.muted)
                        }
                    }
                }
                .buttonStyle(.plain)
            }
            Text("@\(mentionSlug(editingName ? name : project.name))")
                .font(theme.font(Theme.small)).foregroundStyle(theme.muted)
        }
        .frame(maxWidth: .infinity)
    }

    private func commitName() {
        let clean = name.trimmingCharacters(in: .whitespaces)
        editingName = false
        nameFocused = false
        guard let project, !clean.isEmpty, clean != project.name else { return }
        model.updateProject(projectId, name: clean)
    }

    // MARK: Rows


    /// The hex the avatar really shows: the project's own colour, or — for the
    /// relay default — the palette entry `Theme.projectColor` hashes to
    /// (same FNV-1a over the id, same palette order).
    static func currentHex(_ project: Project) -> String {
        let hex = project.color.lowercased()
        if hex == "#8b7cf6" || hex.isEmpty {
            var h: UInt32 = 2166136261
            for b in project.id.utf8 { h = (h ^ UInt32(b)) &* 16777619 }
            return colorHexes[Int(h % UInt32(colorHexes.count))].lowercased()
        }
        return hex
    }

    /// "Default (Marin)" or the chosen voice's name (§10.3).
    private func voiceLabel(_ project: Project) -> String {
        let catalog = model.voiceCatalog
        if let id = project.voice {
            return catalog?.voices.first { $0.id == id }?.name ?? id.capitalized
        }
        if let d = catalog?.defaultId, let v = catalog?.voices.first(where: { $0.id == d }) { return "Default (\(v.name))" }
        return "Default"
    }

    private func row(icon: String, title: String, value: String, chevron: Bool = true) -> some View {
        PaneRow(leading: .tile(icon), title: title, value: value, chevron: chevron)
    }

    private func card<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        let rows = content()
        return PaneCard(separatorInset: PaneKit.separatorInset(.tile(""))) { rows }
    }

    private func workspaceLabel(_ project: Project) -> String {
        switch model.machine.workspaces[project.id] ?? project.workspace {
        case .active: return "Running"
        case .idle: return "Idle"
        case .error: return "Error"
        case .unknown: return model.machine.reported ? "Not set up yet" : "—"
        }
    }
}


// MARK: - Telegram-style agent profile

struct AgentProfileView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    let projectId: Id
    /// For an agent that is not in my project list (someone else's agent in a DM).
    var fallback: Project? = nil
    /// Jacob's rule (2026-10-22): one tab set per entity, identical on Mac
    /// and iPhone. Agents: Profile | Settings | Library (the Mac's `agent.rs`).
    @State private var tab = "Profile"
    @State private var showEdit = false
    @State private var showModel = false
    @State private var showVoice = false
    @State private var showDescription = false
    @State private var descriptionDraft = ""
    @State private var showSecrets = false

    private var project: Project? { model.project(projectId) ?? fallback }
    private var isMine: Bool { model.project(projectId) != nil && project?.shared != true }
    private var items: [ChatItem] {
        let msgs = model.chats(of: projectId).flatMap { model.messages(of: $0.id) }
        return ChatMedia.items(messages: msgs, slot: projectId, model: model)
    }

    var body: some View {
        ZStack(alignment: .top) {
            theme.bg.ignoresSafeArea()
            ScrollView {
                if let project {
                    VStack(spacing: 18) {
                        Color.clear.frame(height: 80)
                        // The shared pane layout (panels-unify, 2026-10-21): picture
                        // (crayon for my own agent → the edit sheet), name, status,
                        // @slug; no quick-action row — call and search live in the chat.
                        PaneHeader(name: project.name, subtitle: statusLine(project)) {
                            if isMine {
                                Button { showEdit = true } label: {
                                    AgentAvatar(project: project, theme: theme, size: PaneHeader<EmptyView>.avatarSize, working: model.isStreaming(project: project.id), idle: true)
                                        .paneCrayon()
                                        .contentShape(Rectangle())
                                }
                                .buttonStyle(.plain)
                                .accessibilityLabel("Edit agent")
                            } else {
                                AgentAvatar(project: project, theme: theme, size: PaneHeader<EmptyView>.avatarSize, working: model.isStreaming(project: project.id), idle: true)
                            }
                        }
                        .overlay(alignment: .bottom) {
                            if model.isStreaming(project: project.id) {
                                Text(statusLine(project)).font(theme.font(Theme.small)).foregroundStyle(Color(hex: "#1084FE")!)
                                    .background(theme.bg)
                            }
                        }
                        Text("@\(mentionSlug(project.name))")
                            .font(theme.font(Theme.small))
                            .foregroundStyle(theme.placeholder)
                            .padding(.top, -10)
                            .onTapGesture { UIPasteboard.general.string = "@\(mentionSlug(project.name))" }
                        ProfileTabs(tabs: ["Profile", "Settings", "Library"], selected: $tab)
                        switch tab {
                        case "Profile":
                            // Who the agent is: owner, machine, description.
                            ProfileCard {
                                ownerRow(project)
                                PaneRow(title: "Machine", value: machineLine(project))
                                if project.description.isEmpty {
                                    if isMine {
                                        ProfileLinkRow(title: "Add a description") { descriptionDraft = ""; showDescription = true }
                                    } else {
                                        PaneRow(title: "Description", value: "—")
                                    }
                                } else {
                                    Button { if isMine { descriptionDraft = project.description; showDescription = true } } label: {
                                        PaneRow(title: "Description", value: project.description, chevron: isMine)
                                    }
                                    .buttonStyle(.plain)
                                    .disabled(!isMine)
                                }
                            }
                        case "Settings":
                            // How the agent runs: model, camera vision, secrets.
                            // (Voice left the pane on 2026-10-22: spoken replies sit behind a kill switch.)
                            ProfileCard {
                                Button { if isMine { showModel = true } } label: {
                                    PaneRow(title: "Model", value: "\(project.shortModel) · \(project.effort.label)", chevron: isMine)
                                }
                                .buttonStyle(.plain)
                                .disabled(!isMine)
                                if isMine {
                                    PaneRow(
                                        title: "Camera vision",
                                        trailing: AnyView(
                                            Toggle("", isOn: Binding(get: { project.visionEnabled }, set: { model.setVision(projectId, enabled: $0) }))
                                                .labelsHidden()
                                                .tint(theme.online)
                                                .accessibilityLabel("Camera vision")
                                        )
                                    )
                                }
                                Button { showSecrets = true } label: {
                                    PaneRow(title: "Secrets", value: secretsLine(project), chevron: true)
                                }
                                .buttonStyle(.plain)
                            }
                        default:
                            library(project)
                        }
                        Color.clear.frame(height: 40)
                    }
                } else {
                    Text("This agent is gone.").foregroundStyle(theme.muted).padding(.top, 120)
                }
            }
            ProfileTopBar { EmptyView() }
        }
        .task { await model.loadVoices() }
        .sheet(isPresented: $showModel) {
            if let project = model.project(projectId) { ModelPickerView(project: project).environmentObject(model).environment(\.theme, theme) }
        }
        .sheet(isPresented: $showVoice) {
            if let project = model.project(projectId) { VoicePickerView(project: project).environmentObject(model).environment(\.theme, theme) }
        }
        .sheet(isPresented: $showEdit) {
            AgentEditSheet(projectId: projectId).environmentObject(model).environment(\.theme, theme)
        }
        .sheet(isPresented: $showSecrets) {
            NavigationStack { AgentSecretsPage(projectId: projectId) }
                .environmentObject(model).environment(\.theme, theme)
                .presentationBackground(theme.bg)
                .preferredColorScheme(theme.isDark ? .dark : .light)
        }
        .task { await model.refreshProjectSecrets(projectId) }
        .alert("Description", isPresented: $showDescription) {
            TextField("One paragraph about this agent", text: $descriptionDraft)
            Button("Save") { model.updateProject(projectId, description: descriptionDraft.trimmingCharacters(in: .whitespacesAndNewlines)) }
            Button("Cancel", role: .cancel) { }
        } message: { Text("Shown to everyone who can see this agent (up to 500 characters).") }
    }

    /// Library: everything the agent's chats hold — images, files, voice
    /// memos, links — in sections (the group pane's Library).
    @ViewBuilder
    private func library(_ project: Project) -> some View {
        let media = items.filter { $0.kind == .media }
        let files = items.filter { $0.kind == .file }
        let voice = items.filter { $0.kind == .voice }
        let links = items.filter { $0.kind == .link }
        if media.isEmpty && files.isEmpty && voice.isEmpty && links.isEmpty {
            Text("Nothing shared in \(project.name)'s chats yet.").font(theme.font(Theme.small)).foregroundStyle(theme.muted)
                .frame(maxWidth: .infinity).padding(.top, 36)
        } else {
            if !media.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    PaneSectionTitle("Images · \(media.count)")
                    ChatItemsList(items: media, kind: .media, empty: "")
                }
            }
            if !files.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    PaneSectionTitle("Files · \(files.count)")
                    ChatItemsList(items: files, kind: .file, empty: "")
                }
            }
            if !voice.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    PaneSectionTitle("Voice · \(voice.count)")
                    ChatItemsList(items: voice, kind: .voice, empty: "")
                }
            }
            if !links.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    PaneSectionTitle("Links · \(links.count)")
                    ChatItemsList(items: links, kind: .link, empty: "")
                }
            }
        }
    }

    private func statusLine(_ p: Project) -> String {
        if model.isStreaming(project: p.id) { return "working" }
        switch p.machineState ?? "" {
        case "awake": return "idle · machine awake"
        case "waking": return "machine waking"
        case "asleep": return "machine asleep"
        case "error": return "machine needs attention"
        default: return p.workspace == .active ? "idle · running" : "idle"
        }
    }

    /// "Marin (default)" or the chosen voice (§10.3).
    private func voiceLine(_ project: Project) -> String {
        let catalog = model.voiceCatalog
        if let id = project.voice { return catalog?.voices.first { $0.id == id }?.name ?? id.capitalized }
        if let d = catalog?.defaultId, let v = catalog?.voices.first(where: { $0.id == d }) { return "\(v.name) (default)" }
        return "Default"
    }

    /// "3 · STRIPE_KEY, Arbos — Res…" from the agent's Access rows; "none" before any.
    private func secretsLine(_ p: Project) -> String {
        guard let d = model.projectSecrets[p.id] else { return "—" }
        let on = d.items.filter(\.enabled)
        if on.isEmpty { return d.requests.contains(where: \.isPending) ? "waiting for you" : "none" }
        let names = on.map(\.title).joined(separator: ", ")
        return "\(on.count) · \(names)"
    }

    private func machineLine(_ p: Project) -> String {
        var parts: [String] = []
        if let s = p.machineState { parts.append(s) }
        if let c = p.machineClass, c != "none" { parts.append(c) }
        if let snap = p.machineSnapshot, !snap.isEmpty { parts.append(snap) }
        if p.machineAlwaysOn { parts.append("always on") }
        if parts.isEmpty { return model.machine.reported ? model.machine.label.lowercased() : "—" }
        return parts.joined(separator: " · ")
    }

    private func ownerRow(_ p: Project) -> some View {
        let handle = p.ownerHandle ?? model.user?.login ?? ""
        // Prefer the relay's owner id; never use the handle as id (that broke "groups in common").
        let ownerId = p.ownerId ?? model.personId(handle: handle) ?? handle
        let person = ProfilePerson(id: p.ownerHandle == nil ? (model.user?.id ?? handle) : ownerId, handle: handle, name: p.ownerName ?? (p.ownerHandle == nil ? model.user?.name : nil),
                                   avatarUrl: p.ownerAvatarUrl ?? (p.ownerHandle == nil ? model.user?.avatarUrl : nil),
                                   lastSeenAt: p.ownerLastSeenAt, online: p.ownerOnline)
        let status = p.ownerHandle == nil ? ("you", false) : lastSeenLine(person.lastSeenAt, online: person.online)
        return Button { if p.ownerHandle != nil { model.showProfile(.user(person)) } } label: {
            PaneRow(
                leading: .avatar(AnyView(PersonAvatar(url: person.avatarUrl, handle: handle, size: PaneKit.avatar, name: person.name, id: person.id, colorIndex: person.avatarColorIndex))),
                title: "@\(handle)",
                subtitle: p.ownerHandle == nil ? "Owner · you" : "Owner · \(status.0)",
                chevron: p.ownerHandle != nil
            )
        }
        .buttonStyle(.plain)
        .disabled(p.ownerHandle == nil)
    }
}
