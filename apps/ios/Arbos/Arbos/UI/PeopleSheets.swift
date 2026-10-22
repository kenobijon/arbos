// People-chat sheets (D72): New Message (Telegram-style: search, New agent,
// New group, your agents, every Arbos user), the New group / Add members
// picker (chips, search, checkmarks) and project members (invite by handle).

import SwiftUI

/// "+" on home — Telegram's "New Message" sheet in Grok's dark skin: rises
/// from the bottom, search on top, then New agent / New group, then your
/// agents, then every other Arbos user. Typing filters agents and the
/// directory and also asks `GET /users/search`, so a GitHub handle not on
/// Arbos yet can be invited.
struct NewChatSheet: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var remote: [Person] = []
    @State private var searching = false
    @State private var opening = false
    @State private var loadingDirectory = false
    /// `ARBOS_SHOW=newagent` lands on the New agent page (screenshot rig).
    @State private var showNewAgent = DebugHooks.show == "newagent"
    @FocusState private var focused: Bool

    private var trimmed: String {
        query.trimmingCharacters(in: .whitespaces).trimmingCharacters(in: CharacterSet(charactersIn: "@"))
    }

    /// Own + shared agents (home list order by name). Empty query hides archived;
    /// a search finds them again, same as the home list.
    private var agents: [Project] {
        let q = trimmed.lowercased()
        let base = model.projects.filter { q.isEmpty ? !model.isArchived($0.id) : true }
        let filtered = q.isEmpty ? base : base.filter {
            $0.name.lowercased().contains(q) || mentionSlug($0.name).contains(q)
        }
        return filtered.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    /// Directory hits first (alphabetical), then remote hits not already listed.
    private var people: [Person] {
        let q = trimmed.lowercased()
        let local = q.isEmpty ? model.directory : model.directory.filter {
            $0.handle.lowercased().contains(q) || ($0.name ?? "").lowercased().contains(q)
        }
        var seen = Set(local.map(\.id))
        var out = local
        for p in remote where !seen.contains(p.id) && p.id != model.user?.id {
            seen.insert(p.id)
            out.append(p)
        }
        return out
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    searchField
                        .padding(.horizontal, SheetSearchField.inset)
                        .padding(.top, 6)
                        .padding(.bottom, 14)

                    if trimmed.isEmpty {
                        card {
                            NavigationLink { NewAgentView(onDone: { dismiss() }) } label: {
                                actionRow(icon: "sparkles", title: "New agent", subtitle: "Its own workspace on your machine")
                            }
                            .buttonStyle(.plain)
                            rowDivider
                            NavigationLink { NewGroupView(onDone: { dismiss() }) } label: {
                                actionRow(icon: "person.2.fill", title: "New group", subtitle: "People and agents in one chat")
                            }
                            .buttonStyle(.plain)
                            if model.serversAvailable {
                                // D88 rule 8: a server of your own, or one you were invited to.
                                rowDivider
                                NavigationLink { NewServerView(onDone: { dismiss() }) } label: {
                                    actionRow(icon: "number", title: "New server", subtitle: "Channels for a group of people")
                                }
                                .buttonStyle(.plain)
                                rowDivider
                                NavigationLink { JoinServerView(onDone: { dismiss() }) } label: {
                                    actionRow(icon: "link", title: "Join a server", subtitle: "Paste an invite link")
                                }
                                .buttonStyle(.plain)
                            }
                        }
                        .padding(.horizontal, 16)
                        .padding(.bottom, 22)
                    }

                    if !agents.isEmpty {
                        sectionHeader(trimmed.isEmpty ? "Your agents" : "Agents")
                        card {
                            ForEach(Array(agents.enumerated()), id: \.element.id) { i, p in
                                Button { openAgent(p) } label: { AgentPickRow(project: p) }
                                    .buttonStyle(.plain)
                                if i < agents.count - 1 { rowDivider }
                            }
                        }
                        .padding(.horizontal, 16)
                        .padding(.bottom, 22)
                    }

                    // Keep the people block when the directory is empty (invite copy),
                    // but skip a redundant "no match" under Agents when a search hit only agents.
                    if !people.isEmpty || agents.isEmpty || trimmed.isEmpty {
                        sectionHeader(trimmed.isEmpty ? "Arbos users" : "People")
                        if people.isEmpty {
                            peopleEmpty
                        } else {
                            card {
                                ForEach(Array(people.enumerated()), id: \.element.id) { i, p in
                                    Button { open(p) } label: { PersonRow(person: p) }
                                        .buttonStyle(.plain)
                                    if i < people.count - 1 { rowDivider }
                                }
                            }
                            .padding(.horizontal, 16)
                        }
                    }
                    Color.clear.frame(height: 40)
                }
            }
            .scrollDismissesKeyboard(.interactively)
            .background(theme.bg)
            .navigationTitle("New Message")
            .navigationBarTitleDisplayMode(.inline)
            .navigationDestination(isPresented: $showNewAgent) { NewAgentView(onDone: { dismiss() }) }
            .toolbar {
                ToolbarItem(placement: .topBarLeading) { Button("Cancel") { dismiss() }.foregroundStyle(theme.text) }
            }
            .toolbarBackground(theme.bg, for: .navigationBar)
            .toolbarBackground(.visible, for: .navigationBar)
            .task {
                loadingDirectory = model.directory.isEmpty
                await model.loadDirectory()
                loadingDirectory = false
            }
            .onChange(of: query) { _, _ in search() }
        }
        .tint(theme.text)
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
        .presentationBackground(theme.bg)
        .presentationCornerRadius(28)
        .preferredColorScheme(theme.isDark ? .dark : .light)
    }

    // MARK: Pieces

    private var searchField: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass").font(.system(size: SheetSearchField.iconSize, weight: .medium)).foregroundStyle(theme.muted)
            TextField("", text: $query, prompt: Text("Search people or agents").foregroundStyle(theme.placeholder))
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .font(theme.font(Theme.body))
                .foregroundStyle(theme.text)
                .focused($focused)
                .submitLabel(.go)
                .onSubmit {
                    if let first = agents.first { openAgent(first) }
                    else if let first = people.first { open(first) }
                    else if !trimmed.isEmpty { openHandle(trimmed) }
                }
            if searching {
                ProgressView().controlSize(.small).tint(theme.muted)
            } else if !query.isEmpty {
                Button { query = ""; remote = [] } label: {
                    Image(systemName: "xmark.circle.fill").foregroundStyle(theme.muted)
                }
                .buttonStyle(.plain)
            }
        }
        .sheetSearchFieldChrome(theme)
    }

    private func sectionHeader(_ text: String) -> some View {
        Text(text.uppercased())
            .font(theme.font(12, weight: .medium))
            .kerning(0.4)
            .foregroundStyle(theme.muted)
            .padding(.horizontal, 32)
            .padding(.bottom, 8)
    }

    private func card<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        VStack(spacing: 0) { content() }
            .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(theme.card))
    }

    private var rowDivider: some View {
        Rectangle().fill(theme.separator).frame(height: 0.5).padding(.leading, 72)
    }

    private func actionRow(icon: String, title: String, subtitle: String) -> some View {
        HStack(spacing: 14) {
            GlassCircle(size: 44) {
                Image(systemName: icon).font(.system(size: 17, weight: .semibold)).foregroundStyle(theme.text)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(theme.font(Theme.body, weight: .medium)).foregroundStyle(theme.text)
                Text(subtitle).font(theme.font(Theme.tiny)).foregroundStyle(theme.muted)
            }
            Spacer()
            Image(systemName: "chevron.right").font(.system(size: 13, weight: .semibold)).foregroundStyle(theme.muted.opacity(0.7))
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .contentShape(Rectangle())
    }

    private var peopleEmpty: some View {
        Group {
            if loadingDirectory || searching {
                HStack { Spacer(); ProgressView().tint(theme.muted); Spacer() }.padding(.top, 28)
            } else if trimmed.isEmpty {
                Text(model.credentials == nil
                     ? "Sign in to see who else is on Arbos."
                     : "No one else is on Arbos yet. Search a GitHub username to invite them; the chat opens for both of you when they sign in.")
                    .font(theme.font(Theme.small)).foregroundStyle(theme.muted)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: .infinity)
                    .padding(.horizontal, 36).padding(.top, 20)
            } else {
                VStack(spacing: 10) {
                    Text("No one on Arbos or GitHub matches @\(trimmed).")
                        .font(theme.font(Theme.small)).foregroundStyle(theme.muted)
                    if trimmed.count >= 2 {
                        Button { openHandle(trimmed) } label: {
                            Text("Invite @\(trimmed)")
                                .font(theme.font(Theme.small, weight: .semibold)).foregroundStyle(theme.onPrimary)
                                .padding(.horizontal, 18).frame(height: 38)
                                .background(Capsule().fill(theme.primary))
                        }
                        .buttonStyle(.plain)
                    }
                }
                .frame(maxWidth: .infinity)
                .padding(.top, 20)
            }
        }
    }

    // MARK: Actions

    private func search() {
        let q = trimmed
        guard q.count >= 2 else { remote = []; searching = false; return }
        searching = true
        Task {
            try? await Task.sleep(nanoseconds: 250_000_000)
            guard q == trimmed else { return }
            remote = await model.searchPeople(q)
            if q == trimmed { searching = false }
        }
    }

    private func open(_ p: Person) { openHandle(p.handle) }

    private func openAgent(_ p: Project) {
        guard !opening else { return }
        opening = true
        model.open(project: p)
        dismiss()
        opening = false
    }

    private func openHandle(_ handle: String) {
        guard !opening else { return }
        opening = true
        Task {
            if await model.openDM(handle: handle) != nil { dismiss() }
            opening = false
        }
    }
}

/// One of my agents in the New Message / New group lists: tree avatar, name, @slug.
struct AgentPickRow: View {
    @Environment(\.theme) private var theme
    let project: Project
    var trailing: AnyView? = nil

    var body: some View {
        HStack(spacing: 14) {
            AgentAvatar(project: project, theme: theme, size: 44, still: true)
            VStack(alignment: .leading, spacing: 2) {
                Text(project.name).font(theme.font(Theme.body, weight: .medium)).foregroundStyle(theme.text).lineLimit(1)
                Text("@\(mentionSlug(project.name))").font(theme.font(Theme.tiny)).foregroundStyle(theme.muted).lineLimit(1)
            }
            Spacer()
            if let trailing { trailing }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 9)
        .contentShape(Rectangle())
    }
}

/// One person: avatar, name, @handle and their Arbos status.
struct PersonRow: View {
    @Environment(\.theme) private var theme
    let person: Person
    var trailing: AnyView? = nil

    var body: some View {
        HStack(spacing: 14) {
            PersonAvatar(url: person.avatarUrl, handle: person.handle, size: 44, name: person.name, id: person.id, colorIndex: person.avatarColorIndex)
            VStack(alignment: .leading, spacing: 2) {
                Text(person.displayName).font(theme.font(Theme.body, weight: .medium)).foregroundStyle(theme.text).lineLimit(1)
                Text("@\(person.handle)").font(theme.font(Theme.tiny)).foregroundStyle(theme.muted).lineLimit(1)
            }
            Spacer()
            if let trailing {
                trailing
            } else {
                Text(PersonRow.statusLabel(person.status))
                    .font(theme.font(11, weight: .medium))
                    .foregroundStyle(person.status == "member" ? theme.online : theme.muted)
                    .padding(.horizontal, 9).padding(.vertical, 4)
                    .background(Capsule().fill(theme.field))
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 9)
        .contentShape(Rectangle())
    }

    static func statusLabel(_ s: String) -> String {
        switch s {
        case "member": return "On Arbos"
        case "invited": return "Invited"
        default: return "Invite"
        }
    }
}

/// New agent: a name and an avatar colour, then `POST /projects`. The relay
/// sets up its workspace. The preview is the Telegram-style initials circle
/// (D79): Automatic until a swatch is picked.
struct NewAgentView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    /// Closes the whole sheet (a pushed view's own dismiss only pops).
    let onDone: () -> Void
    @State private var name = ""
    /// Explicit swatch pick; nil = Automatic (the relay derives the colour from the new id).
    @State private var pickedColorIndex: Int? = nil
    @FocusState private var focused: Bool

    private var clean: String { name.trimmingCharacters(in: .whitespaces) }
    /// Before the project exists there is no id: preview the name's hash.
    private var previewIndex: Int { pickedColorIndex ?? AvatarPalette.index(for: clean.isEmpty ? "new" : clean) }

    var body: some View {
        VStack(spacing: 18) {
            InitialsAvatar(initials: AvatarPalette.initials(name: clean.isEmpty ? "Agent" : clean, handle: nil), colorIndex: previewIndex, size: 92)
                .padding(.top, 28)
                .animation(.snappy, value: "\(clean)|\(pickedColorIndex ?? -1)")
            Text(clean.isEmpty ? "Name your agent" : "@\(mentionSlug(clean))")
                .font(theme.font(Theme.small)).foregroundStyle(theme.muted)
            TextField("", text: $name, prompt: Text("Agent name").foregroundStyle(theme.placeholder))
                .font(theme.font(Theme.body)).foregroundStyle(theme.text)
                .multilineTextAlignment(.center)
                .autocorrectionDisabled()
                .focused($focused)
                .submitLabel(.done)
                .onSubmit(create)
                .padding(.horizontal, 16).frame(height: 50)
                .background(RoundedRectangle(cornerRadius: 25, style: .continuous).fill(theme.field))
                .overlay(RoundedRectangle(cornerRadius: 25, style: .continuous).strokeBorder(theme.fieldBorder, lineWidth: 1))
                .padding(.horizontal, 24)
            AvatarColorPicker(name: clean, id: clean.isEmpty ? "new" : clean, current: pickedColorIndex) { pickedColorIndex = $0 }
                .padding(.horizontal, 24)
            Button(action: create) {
                Text("Create agent")
                    .font(theme.font(Theme.body, weight: .semibold)).foregroundStyle(theme.onPrimary)
                    .frame(maxWidth: .infinity).frame(height: 50)
                    .background(Capsule().fill(theme.primary))
            }
            .buttonStyle(.plain)
            .disabled(clean.isEmpty)
            .opacity(clean.isEmpty ? 0.45 : 1)
            .padding(.horizontal, 24)
            Text("A new agent with its own workspace on your machine. You can rename it and pick its model from its profile.")
                .font(theme.font(Theme.tiny)).foregroundStyle(theme.muted)
                .multilineTextAlignment(.center).padding(.horizontal, 36)
                .fixedSize(horizontal: false, vertical: true)
            Spacer()
        }
        .background(theme.bg)
        .navigationTitle("New agent")
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(theme.bg, for: .navigationBar)
        .toolbarBackground(.visible, for: .navigationBar)
        .onAppear { focused = true }
    }

    private func create() {
        guard !clean.isEmpty else { return }
        model.createProject(name: clean, avatarColorIndex: pickedColorIndex)
        onDone()
    }
}

/// New group: pick people (and your own agents), then create. With `addTo`
/// (the group pane's Add members) the same picker is titled for that group,
/// shows its members as already in, and Add puts the picks into it.
struct NewGroupView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    let onDone: () -> Void
    /// People-chat id to add to; nil creates a new group.
    var addTo: String? = nil
    @State private var query = ""
    @State private var remote: [Person] = []
    @State private var picked: [Person] = []
    @State private var pickedAgents: [Project] = []
    @State private var creating = false
    /// Shown on this screen — the root notice alert sits under the sheet and looks like a silent fail.
    @State private var errorText: String?

    private var trimmed: String { query.trimmingCharacters(in: .whitespaces).trimmingCharacters(in: CharacterSet(charactersIn: "@")) }

    /// Own agents only (same as Members → Add agent); shared ones cannot be re-shared here.
    /// Empty query hides archived agents; a search finds them again — archived agents still
    /// exist and can join groups unless deleted (home archive is hide-only).
    private var agentCandidates: [Project] {
        let q = trimmed.lowercased()
        let base = model.projects.filter { !$0.shared && (q.isEmpty ? !model.isArchived($0.id) : true) }
            .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
        if q.isEmpty { return base }
        return base.filter { $0.name.lowercased().contains(q) || mentionSlug($0.name).contains(q) }
    }

    private var candidates: [Person] {
        let q = trimmed.lowercased()
        var out = q.isEmpty ? model.directory : model.directory.filter { $0.handle.lowercased().contains(q) || ($0.name ?? "").lowercased().contains(q) }
        var seen = Set(out.map(\.id))
        for p in remote where !seen.contains(p.id) && p.id != model.user?.id { seen.insert(p.id); out.append(p) }
        return out
    }

    private var addToChat: PeopleChat? { addTo.flatMap { model.peopleChat($0) } }
    private var adding: Bool { addTo != nil }
    private var title: String { addToChat.map { "Add members to \($0.title)" } ?? (adding ? "Add members" : "New group") }
    /// Create needs a person (a group of one is a DM); Add takes agents alone too.
    private var canSubmit: Bool { adding ? !(picked.isEmpty && pickedAgents.isEmpty) : !picked.isEmpty }

    private func isIn(_ p: Person) -> Bool { addToChat?.hasPerson(p, meId: model.user?.id) ?? false }
    private func isIn(_ p: Project) -> Bool { addToChat?.hasAgent(p.id) ?? false }

    private func pickMark(selected: Bool, already: Bool) -> AnyView {
        AnyView(
            Image(systemName: selected || already ? "checkmark.circle.fill" : "circle")
                .font(theme.font(22))
                .foregroundStyle(already ? theme.muted : (selected ? theme.text : theme.muted.opacity(0.5)))
        )
    }

    var body: some View {
        VStack(spacing: 0) {
            if !picked.isEmpty || !pickedAgents.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(pickedAgents) { p in
                            MemberChip(label: "@\(mentionSlug(p.name))", onRemove: { toggleAgent(p) }) {
                                AgentAvatar(project: p, theme: theme, size: MemberChip.avatar, still: true)
                            }
                        }
                        ForEach(picked) { p in
                            MemberChip(label: "@\(p.handle)", onRemove: { toggle(p) }) {
                                PersonAvatar(url: p.avatarUrl, handle: p.handle, size: MemberChip.avatar, name: p.name, id: p.id, colorIndex: p.avatarColorIndex)
                            }
                        }
                    }
                    .padding(.horizontal, 16)
                }
                .frame(height: 44)
            }
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass").font(.system(size: SheetSearchField.iconSize, weight: .medium)).foregroundStyle(theme.muted)
                TextField("", text: $query, prompt: Text("Add people or agents").foregroundStyle(theme.placeholder))
                    .textInputAutocapitalization(.never).autocorrectionDisabled()
                    .font(theme.font(Theme.body)).foregroundStyle(theme.text)
            }
            .sheetSearchFieldChrome(theme)
            .padding(.horizontal, SheetSearchField.inset).padding(.vertical, 10)
            .onChange(of: query) { _, _ in search() }

            if let errorText {
                Text(errorText)
                    .font(theme.font(Theme.small))
                    .foregroundStyle(theme.danger)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 20)
                    .padding(.bottom, 8)
            }

            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    if !agentCandidates.isEmpty {
                        Text("YOUR AGENTS")
                            .font(theme.font(12, weight: .medium))
                            .kerning(0.4)
                            .foregroundStyle(theme.muted)
                            .padding(.horizontal, 16)
                            .padding(.bottom, 8)
                        VStack(spacing: 0) {
                            ForEach(Array(agentCandidates.enumerated()), id: \.element.id) { i, p in
                                let already = isIn(p)
                                Button { toggleAgent(p) } label: {
                                    AgentPickRow(project: p, trailing: pickMark(selected: pickedAgents.contains(where: { $0.id == p.id }), already: already))
                                }
                                .buttonStyle(.plain)
                                .disabled(already)
                                .opacity(already ? 0.5 : 1)
                                if i < agentCandidates.count - 1 { Rectangle().fill(theme.separator).frame(height: 0.5).padding(.leading, 72) }
                            }
                        }
                        .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(theme.card))
                        .padding(.horizontal, 16)
                        .padding(.bottom, 18)
                    }

                    if !candidates.isEmpty || agentCandidates.isEmpty {
                        if !agentCandidates.isEmpty {
                            Text("PEOPLE")
                                .font(theme.font(12, weight: .medium))
                                .kerning(0.4)
                                .foregroundStyle(theme.muted)
                                .padding(.horizontal, 16)
                                .padding(.bottom, 8)
                        }
                        VStack(spacing: 0) {
                            ForEach(Array(candidates.enumerated()), id: \.element.id) { i, p in
                                let already = isIn(p)
                                Button { toggle(p) } label: {
                                    PersonRow(person: p, trailing: pickMark(selected: picked.contains(p), already: already))
                                }
                                .buttonStyle(.plain)
                                .disabled(already)
                                .opacity(already ? 0.5 : 1)
                                if i < candidates.count - 1 { Rectangle().fill(theme.separator).frame(height: 0.5).padding(.leading, 72) }
                            }
                            if candidates.isEmpty {
                                Text(trimmed.isEmpty ? "No one else on Arbos yet. Search a GitHub username." : "No one matches @\(trimmed).")
                                    .font(theme.font(Theme.small)).foregroundStyle(theme.muted)
                                    .multilineTextAlignment(.center).padding(.horizontal, 36).padding(.vertical, 24)
                            }
                        }
                        .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(theme.card))
                        .padding(.horizontal, 16)
                    }
                    Color.clear.frame(height: 40)
                }
            }
        }
        .background(theme.bg)
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(theme.bg, for: .navigationBar)
        .toolbarBackground(.visible, for: .navigationBar)
        .toolbar {
            if adding {
                ToolbarItem(placement: .topBarLeading) { Button("Cancel") { onDone() }.foregroundStyle(theme.text) }
            }
            ToolbarItem(placement: .topBarTrailing) {
                Button { if adding { add() } else { create() } } label: {
                    Text(adding ? (creating ? "Adding…" : "Add") : (creating ? "Creating…" : "Create"))
                        .font(theme.font(Theme.small, weight: .semibold))
                        .foregroundStyle(theme.onPrimary)
                        .padding(.horizontal, 12)
                        .frame(height: 32)
                        .background(Capsule().fill(theme.primary))
                }
                .buttonStyle(.plain)
                .disabled(!canSubmit || creating)
                .opacity(!canSubmit || creating ? 0.45 : 1)
            }
        }
        .task { await model.loadDirectory() }
    }

    private func toggle(_ p: Person) {
        guard !isIn(p) else { return }
        if let i = picked.firstIndex(of: p) { picked.remove(at: i) } else { picked.append(p) }
        errorText = nil
    }

    private func toggleAgent(_ p: Project) {
        guard !isIn(p) else { return }
        if let i = pickedAgents.firstIndex(where: { $0.id == p.id }) { pickedAgents.remove(at: i) } else { pickedAgents.append(p) }
        errorText = nil
    }

    /// Add members: every pick goes to the group this picker was opened for.
    private func add() {
        guard let chat = addToChat, canSubmit, !creating else { return }
        let plan = AddMembersPlan.make(chat: chat, meId: model.user?.id, picked: picked, pickedAgents: pickedAgents)
        guard !plan.isEmpty else { errorText = "Pick someone who is not in the group yet"; return }
        creating = true
        errorText = nil
        Task {
            if await model.addMembers(chatId: plan.chatId, handles: plan.handles, agentIds: plan.agentIds) {
                onDone()
            } else {
                errorText = model.notice?.text ?? "Could not add them."
                model.notice = nil
            }
            creating = false
        }
    }

    private func search() {
        let q = trimmed
        guard q.count >= 2 else { remote = []; return }
        Task {
            try? await Task.sleep(nanoseconds: 250_000_000)
            guard q == trimmed else { return }
            remote = await model.searchPeople(q)
        }
    }

    private func create() {
        guard !picked.isEmpty, !creating else { return }
        creating = true
        errorText = nil
        let agentIds = pickedAgents.map(\.id)
        Task {
            if await model.createGroup(handles: picked.map(\.handle), agentIds: agentIds) {
                onDone()
            } else {
                // Copy onto this screen: RootView's alert is behind the sheet.
                errorText = model.notice?.text ?? "Could not create the group."
                model.notice = nil
            }
            creating = false
        }
    }
}

/// Project members (D72 step 3): invite by @handle, list, remove (owner).
struct ProjectMembersSheet: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    let project: Project
    @State private var members: [ProjectMember] = []
    @State private var handle = ""
    @State private var busy = false
    @State private var remote: [Person] = []
    @State private var searching = false

    private var trimmed: String {
        handle.trimmingCharacters(in: .whitespaces).trimmingCharacters(in: CharacterSet(charactersIn: "@"))
    }

    private var memberIds: Set<String> { Set(members.map(\.id)) }

    /// Directory + remote matches for the typed handle (excludes current members).
    private var suggestions: [Person] {
        let q = trimmed.lowercased()
        guard !q.isEmpty else { return [] }
        let local = model.directory.filter {
            $0.handle.lowercased().contains(q) || ($0.name ?? "").lowercased().contains(q)
        }.filter { !memberIds.contains($0.id) && $0.id != model.user?.id }
        var seen = Set(local.map(\.id))
        var out = local
        for p in remote where !seen.contains(p.id) && !memberIds.contains(p.id) && p.id != model.user?.id {
            seen.insert(p.id)
            out.append(p)
        }
        return out
    }

    var body: some View {
        NavigationStack {
            List {
                if !project.shared {
                    Section {
                        HStack(spacing: 8) {
                            Text("@").foregroundStyle(theme.muted)
                            TextField("", text: $handle, prompt: Text("GitHub username").foregroundStyle(theme.placeholder))
                                .textInputAutocapitalization(.never).autocorrectionDisabled()
                                .onSubmit { invite() }
                            if searching {
                                ProgressView().controlSize(.small).tint(theme.muted)
                            }
                            Button(busy ? "…" : "Invite") { invite() }
                                .font(theme.font(Theme.small, weight: .semibold))
                                .disabled(busy || trimmed.isEmpty)
                        }
                        if !trimmed.isEmpty {
                            ForEach(suggestions) { p in
                                Button { invite(p.handle) } label: {
                                    HStack(spacing: 12) {
                                        PersonAvatar(url: p.avatarUrl, handle: p.handle, size: 40, name: p.name, id: p.id, colorIndex: p.avatarColorIndex)
                                        VStack(alignment: .leading, spacing: 2) {
                                            Text(p.displayName).foregroundStyle(theme.text)
                                            Text("@\(p.handle)").font(theme.font(Theme.tiny)).foregroundStyle(theme.muted)
                                        }
                                        Spacer()
                                        Text(PersonRow.statusLabel(p.status))
                                            .font(theme.font(11, weight: .medium))
                                            .foregroundStyle(p.status == "member" ? theme.online : theme.muted)
                                    }
                                }
                                .buttonStyle(.plain)
                                .disabled(busy)
                            }
                            if suggestions.isEmpty && !searching && trimmed.count >= 2 {
                                Text("No one matches @\(trimmed).")
                                    .font(theme.font(Theme.small)).foregroundStyle(theme.muted)
                            }
                        }
                    } footer: {
                        Text("A member drives this agent with you, sees its chats and files, and spends your credits. You can remove them any time.")
                    }
                    .listRowBackground(theme.card)
                }
                Section("Members") {
                    ForEach(members) { m in
                        HStack(spacing: 12) {
                            Button { model.showProfile(.user(ProfilePerson(m))) } label: { PersonAvatar(url: m.avatarUrl, handle: m.handle, size: 40, name: m.name, id: m.id, colorIndex: m.avatarColorIndex) }.buttonStyle(.plain)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(m.name?.isEmpty == false ? m.name! : m.handle).foregroundStyle(theme.text)
                                Text("@\(m.handle) · \(m.role)\(m.status == "invited" ? " · invited" : "")").font(theme.font(Theme.tiny)).foregroundStyle(theme.muted)
                            }
                        }
                        .swipeActions {
                            if !project.shared && m.role != "owner" {
                                Button("Remove", role: .destructive) {
                                    Task { await model.removeProjectMember(project.id, userId: m.id); await load() }
                                }
                            }
                        }
                    }
                    if members.isEmpty { Text("Just you so far.").foregroundStyle(theme.muted) }
                }
                .listRowBackground(theme.card)
            }
            .scrollContentBackground(.hidden)
            .background(theme.bg)
            .navigationTitle("Share \(project.name)")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } } }
            .task {
                await load()
                await model.loadDirectory()
            }
            .onChange(of: handle) { _, _ in search() }
        }
        .presentationDetents([.medium, .large])
    }

    private func load() async { members = await model.projectMembers(project.id) }

    private func search() {
        let q = trimmed
        guard q.count >= 2 else { remote = []; searching = false; return }
        searching = true
        Task {
            try? await Task.sleep(nanoseconds: 250_000_000)
            guard q == trimmed else { return }
            remote = await model.searchPeople(q)
            if q == trimmed { searching = false }
        }
    }

    private func invite(_ raw: String? = nil) {
        let h = (raw ?? trimmed)
            .trimmingCharacters(in: .whitespaces)
            .trimmingCharacters(in: CharacterSet(charactersIn: "@"))
        guard !h.isEmpty, !busy else { return }
        busy = true
        Task {
            if let list = await model.inviteToProject(project.id, handle: h) {
                members = list
                handle = ""
                remote = []
            }
            busy = false
        }
    }
}


// MARK: - Shared sheet pieces

/// The search field of the New Message / New group / Add person sheets. Grok's
/// sheet field (iOS Plugins sheet; Mac ref 57): a 40 pt capsule with a hairline
/// rim, a 16 pt magnifier 14 pt in, body-size placeholder, and side edges that
/// line up with the row content below it (card inset 16 + row padding 14).
enum SheetSearchField {
    static let height: CGFloat = 40
    static let inset: CGFloat = 30
    static let iconSize: CGFloat = 16
}

private struct SheetSearchFieldChrome: ViewModifier {
    let theme: Theme
    func body(content: Content) -> some View {
        content
            .padding(.horizontal, 14)
            .frame(height: SheetSearchField.height)
            .background(Capsule().fill(theme.field))
            .overlay(Capsule().strokeBorder(theme.fieldBorder, lineWidth: 1))
    }
}

extension View {
    func sheetSearchFieldChrome(_ theme: Theme) -> some View { modifier(SheetSearchFieldChrome(theme: theme)) }
}

/// One selected member in a picker (New group, Add members): the member's
/// picture, `@handle`, and an × that removes it. People and agents share it,
/// so an agent chip carries the agent's avatar exactly like a person chip.
struct MemberChip<Picture: View>: View {
    @Environment(\.theme) private var theme
    static var avatar: CGFloat { 22 }
    let label: String
    let onRemove: () -> Void
    let picture: () -> Picture

    init(label: String, onRemove: @escaping () -> Void, @ViewBuilder picture: @escaping () -> Picture) {
        self.label = label
        self.onRemove = onRemove
        self.picture = picture
    }

    var body: some View {
        HStack(spacing: 6) {
            picture()
            Text(label).font(theme.font(Theme.tiny, weight: .medium)).foregroundStyle(theme.text)
            Button(action: onRemove) { Image(systemName: "xmark").font(.system(size: 10, weight: .bold)).foregroundStyle(theme.muted) }
                .buttonStyle(.plain)
        }
        .padding(.leading, 4).padding(.trailing, 10).frame(height: 32)
        .background(Capsule().fill(theme.field))
    }
}
