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
    @State private var showChats = false
    @State private var showVoice = false
    @State private var showShare = false
    @FocusState private var nameFocused: Bool

    private var project: Project? { model.project(projectId) }
    private var chatCount: Int { model.chats[projectId]?.count ?? 0 }

    static let colorHexes = ["#1084FE", "#10B981", "#FF7A00", "#FF3B5C", "#FF2D9B", "#8B5CF6", "#8E5A3C", "#8E8E93", "#FFB000", "#14B8A6"]

    var body: some View {
        NavigationStack {
            ScrollView {
                if let project {
                    VStack(spacing: 0) {
                        hero(project)
                        if !TreeAvatars.shared.isEmpty {
                            card {
                                VStack(alignment: .leading, spacing: 12) {
                                    if TreeAvatars.shared.colorsTintable, let slug = model.avatarSlug(for: project) {
                                        Text("Colour").font(.system(size: Theme.body, weight: .medium)).foregroundStyle(theme.text)
                                        TreeColorGrid(slug: slug, current: model.avatarColor(for: project)) { c in
                                            guard !project.shared else { return }
                                            model.pickAvatarColor(projectId, name: c.name)
                                        }
                                        .disabled(project.shared)
                                        Rectangle().fill(theme.separator).frame(height: 0.5).padding(.vertical, 4)
                                    }
                                    Text("Character").font(.system(size: Theme.body, weight: .medium)).foregroundStyle(theme.text)
                                    TreePickerGrid(current: model.avatarSlug(for: project), color: model.avatarColor(for: project)) { slug in
                                        guard !project.shared else { return }
                                        model.pickAvatar(projectId, slug: slug)
                                    }
                                    .disabled(project.shared)
                                }
                                .padding(.horizontal, 14)
                                .padding(.vertical, 12)
                            }
                            .padding(.horizontal, 16)
                            .padding(.top, 26)
                        } else {
                            card {
                                colourRow(project)
                            }
                            .padding(.horizontal, 16)
                            .padding(.top, 26)
                        }

                        card {
                            Button { showModel = true } label: {
                                row(icon: "cpu", title: "Model", value: "\(project.shortModel) · \(project.effort.label)")
                            }
                            .buttonStyle(.plain)
                            divider
                            Button { showVoice = true } label: {
                                row(icon: "waveform", title: "Voice", value: voiceLabel(project))
                            }
                            .buttonStyle(.plain)
                            divider
                            if model.multiChatEnabled {
                                Button { showChats = true } label: {
                                    row(icon: "bubble.left.and.bubble.right", title: "Chats", value: chatCount == 1 ? "1 chat" : "\(chatCount) chats")
                                }
                                .buttonStyle(.plain)
                                divider
                            }
                            Button { showShare = true } label: {
                                row(icon: "person.2", title: project.shared ? "Members" : "Share", value: project.shared ? "Owned by @\(project.ownerHandle ?? "?")" : "Invite by @handle")
                            }
                            .buttonStyle(.plain)
                        }
                        .padding(.horizontal, 16)
                        .padding(.top, 14)

                        card {
                            row(icon: "desktopcomputer", title: "Workspace", value: workspaceLabel(project), chevron: false)
                            if project.spendCapUsd > 0 {
                                divider
                                row(icon: "dollarsign.circle", title: "Spend today", value: String(format: "$%.2f of $%.0f", project.spendTodayUsd, project.spendCapUsd), chevron: false)
                            }
                        }
                        .padding(.horizontal, 16)
                        .padding(.top, 14)

                        Text(project.shared
                             ? "A shared agent. Its owner pays for it and can remove you any time."
                             : "Mention @\(mentionSlug(project.name)) in a chat with a person to bring this agent in.")
                            .font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
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
                        Button("Save") { commitName() }.font(.system(size: Theme.body, weight: .semibold))
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
            .sheet(isPresented: $showChats) {
                ChatListView(projectId: projectId).environmentObject(model).environment(\.theme, theme)
            }
            .sheet(isPresented: $showShare) {
                if let project { ProjectMembersSheet(project: project).environmentObject(model).environment(\.theme, theme) }
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
            AgentAvatar(project: project, theme: theme, size: 116, dot: machineDotColor(model.machine.state, theme: theme), working: model.isStreaming(project: project.id))
                .padding(.top, 18)
                .id("\(model.avatarSlug(for: project) ?? project.color)|\(model.avatarColor(for: project)?.name ?? "mono")")
                .animation(.snappy, value: project.color)
            if editingName {
                TextField("", text: $name, prompt: Text("Agent name").foregroundStyle(theme.placeholder))
                    .font(.system(size: 24, weight: .semibold))
                    .foregroundStyle(theme.text)
                    .multilineTextAlignment(.center)
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
                            .font(.system(size: 24, weight: .semibold))
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
                .font(.system(size: Theme.small)).foregroundStyle(theme.muted)
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

    private func colourRow(_ project: Project) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Colour").font(.system(size: Theme.body, weight: .medium)).foregroundStyle(theme.text)
            HStack(spacing: 0) {
                let current = AgentEditSheet.currentHex(project)
                ForEach(AgentEditSheet.colorHexes, id: \.self) { hex in
                    let selected = hex.lowercased() == current
                    Button {
                        guard !project.shared else { return }
                        model.updateProject(projectId, color: hex)
                    } label: {
                        ZStack {
                            Circle().fill(Color(hex: hex) ?? .gray).frame(width: 26, height: 26)
                            if selected {
                                Circle().strokeBorder(theme.text, lineWidth: 2).frame(width: 34, height: 34)
                            }
                        }
                        .frame(maxWidth: .infinity)
                        .frame(height: 36)
                    }
                    .buttonStyle(.plain)
                    .disabled(project.shared)
                }
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
    }

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
        HStack(spacing: 14) {
            Image(systemName: icon)
                .font(.system(size: 16, weight: .medium))
                .foregroundStyle(theme.text)
                .frame(width: 28)
            Text(title).font(.system(size: Theme.body)).foregroundStyle(theme.text)
            Spacer()
            Text(value)
                .font(.system(size: Theme.small))
                .foregroundStyle(theme.muted)
                .lineLimit(1)
                .truncationMode(.middle)
            if chevron {
                Image(systemName: "chevron.right").font(.system(size: 13, weight: .semibold)).foregroundStyle(theme.muted.opacity(0.7))
            }
        }
        .padding(.horizontal, 14)
        .frame(height: 50)
        .contentShape(Rectangle())
    }

    private func card<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        VStack(spacing: 0) { content() }
            .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(theme.card))
    }

    private var divider: some View {
        Rectangle().fill(theme.separator).frame(height: 0.5).padding(.leading, 56)
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
    @State private var tab = "Media"
    @State private var showEdit = false
    @State private var showVoice = false
    @State private var showDescription = false
    @State private var descriptionDraft = ""
    @State private var muted = false

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
                        Color.clear.frame(height: 70)
                        ProfileHeader(name: project.name, status: statusLine(project), statusOnline: model.isStreaming(project: project.id)) {
                            AgentAvatar(project: project, theme: theme, size: 112, working: model.isStreaming(project: project.id))
                        }
                        ProfileActionRow(actions: [
                            // Calls start audio-only; camera is toggled on the call screen.
                            ProfileAction(icon: "phone.fill", label: "call") { call(project, .voice) },
                            ProfileAction(icon: muted ? "bell.slash.fill" : "bell.fill", label: muted ? "unmute" : "mute") { muted.toggle() },
                            ProfileAction(icon: "magnifyingglass", label: "search") { search(project) },
                            ProfileAction(icon: "ellipsis", label: "more") { showEdit = true },
                        ])
                        ProfileCard {
                            ProfileInfoRow(label: "agent", value: "@\(mentionSlug(project.name))", tint: Color(hex: "#1084FE")!)
                                .onTapGesture { UIPasteboard.general.string = "@\(mentionSlug(project.name))" }
                            ProfileRowDivider()
                            ownerRow(project)
                            ProfileRowDivider()
                            ProfileInfoRow(label: "machine", value: machineLine(project))
                            ProfileRowDivider()
                            ProfileInfoRow(label: "model", value: "\(project.shortModel) · \(project.effort.label)")
                            ProfileRowDivider()
                            ProfileInfoRow(label: "voice", value: voiceLine(project))
                                .onTapGesture { if isMine { showVoice = true } }
                            ProfileRowDivider()
                            if project.description.isEmpty {
                                if isMine {
                                    ProfileLinkRow(title: "Add a description") { descriptionDraft = ""; showDescription = true }
                                } else {
                                    ProfileInfoRow(label: "description", value: "—")
                                }
                            } else {
                                ProfileInfoRow(label: "description", value: project.description)
                                    .onTapGesture { if isMine { descriptionDraft = project.description; showDescription = true } }
                            }
                        }
                        ProfileTabs(tabs: ["Media", "Files", "Links"], selected: $tab)
                        switch tab {
                        case "Media": ChatItemsList(items: items, kind: .media, empty: "No photos or videos in \(project.name)'s chats yet.")
                        case "Files": ChatItemsList(items: items, kind: .file, empty: "No files in \(project.name)'s chats yet.")
                        default: ChatItemsList(items: items, kind: .link, empty: "No links in \(project.name)'s chats yet.")
                        }
                        Color.clear.frame(height: 40)
                    }
                } else {
                    Text("This agent is gone.").foregroundStyle(theme.muted).padding(.top, 120)
                }
            }
            ProfileTopBar {
                if isMine {
                    Button("Edit") { showEdit = true }
                        .font(.system(size: Theme.body, weight: .medium)).foregroundStyle(theme.text)
                        .padding(.horizontal, 14).frame(height: Theme.control)
                        .background(Capsule().fill(theme.field))
                }
            }
        }
        .task { await model.loadVoices() }
        .sheet(isPresented: $showVoice) {
            if let project = model.project(projectId) { VoicePickerView(project: project).environmentObject(model).environment(\.theme, theme) }
        }
        .sheet(isPresented: $showEdit) {
            AgentEditSheet(projectId: projectId).environmentObject(model).environment(\.theme, theme)
        }
        .alert("Description", isPresented: $showDescription) {
            TextField("One paragraph about this agent", text: $descriptionDraft)
            Button("Save") { model.updateProject(projectId, description: descriptionDraft.trimmingCharacters(in: .whitespacesAndNewlines)) }
            Button("Cancel", role: .cancel) { }
        } message: { Text("Shown to everyone who can see this agent (up to 500 characters).") }
    }

    /// Calls Phase 2: a call in the agent's chat; the voice worker joins as the agent.
    private func call(_ project: Project, _ kind: CallKind) {
        guard let chat = model.primaryChat(of: project.id) ?? model.chats(of: project.id).first else { return }
        dismiss()
        Task { await CallManager.shared.start(chatId: chat.id, projectId: project.id, kind: kind, title: project.name) }
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
        let person = ProfilePerson(id: p.ownerHandle == nil ? (model.user?.id ?? handle) : handle, handle: handle, name: p.ownerName ?? (p.ownerHandle == nil ? model.user?.name : nil),
                                   avatarUrl: p.ownerAvatarUrl ?? (p.ownerHandle == nil ? model.user?.avatarUrl : nil),
                                   lastSeenAt: p.ownerLastSeenAt, online: p.ownerOnline)
        let status = p.ownerHandle == nil ? ("you", false) : lastSeenLine(person.lastSeenAt, online: person.online)
        return Button { if p.ownerHandle != nil { model.showProfile(.user(person)) } } label: {
            HStack(spacing: 12) {
                PersonAvatar(url: person.avatarUrl, handle: handle, size: 36)
                VStack(alignment: .leading, spacing: 2) {
                    Text("owner").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                    HStack(spacing: 6) {
                        Text("@\(handle)").font(.system(size: Theme.body)).foregroundStyle(theme.text)
                        Text(status.0).font(.system(size: Theme.tiny)).foregroundStyle(status.1 ? Color(hex: "#1084FE")! : theme.muted)
                    }
                }
                Spacer()
                if p.ownerHandle != nil { Image(systemName: "chevron.right").font(.system(size: 13, weight: .semibold)).foregroundStyle(theme.muted.opacity(0.7)) }
            }
            .padding(.horizontal, 16).padding(.vertical, 10).contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    private func search(_ p: Project) {
        dismiss()
        if let chat = model.mainChat(of: p.id) {
            model.pendingSearchChat = chat.id
            model.open(chat: chat)
        }
    }
}
