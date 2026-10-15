// People-chat sheets (D72): New Message (Telegram-style: search, New agent,
// New group, every Arbos user), members (people, agents, add-agent picker, block, leave/delete) and
// project members (invite by handle).

import SwiftUI

/// "+" on home — Telegram's "New Message" sheet in Grok's dark skin: rises
/// from the bottom, search on top, then New agent / New group, then every
/// other Arbos user. Typing filters the directory and also asks
/// `GET /users/search`, so a GitHub handle not on Arbos yet can be invited.
struct NewChatSheet: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var remote: [Person] = []
    @State private var searching = false
    @State private var opening = false
    @State private var loadingDirectory = false
    @FocusState private var focused: Bool

    private var trimmed: String {
        query.trimmingCharacters(in: .whitespaces).trimmingCharacters(in: CharacterSet(charactersIn: "@"))
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
                        .padding(.horizontal, 16)
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
                        }
                        .padding(.horizontal, 16)
                        .padding(.bottom, 22)
                    }

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
                    Color.clear.frame(height: 40)
                }
            }
            .scrollDismissesKeyboard(.interactively)
            .background(theme.bg)
            .navigationTitle("New Message")
            .navigationBarTitleDisplayMode(.inline)
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
            Image(systemName: "magnifyingglass").font(.system(size: 15, weight: .medium)).foregroundStyle(theme.muted)
            TextField("", text: $query, prompt: Text("Search people").foregroundStyle(theme.placeholder))
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .font(.system(size: Theme.body))
                .foregroundStyle(theme.text)
                .focused($focused)
                .submitLabel(.go)
                .onSubmit { if let first = people.first { open(first) } else if !trimmed.isEmpty { openHandle(trimmed) } }
            if searching {
                ProgressView().controlSize(.small).tint(theme.muted)
            } else if !query.isEmpty {
                Button { query = ""; remote = [] } label: {
                    Image(systemName: "xmark.circle.fill").foregroundStyle(theme.muted)
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.horizontal, 12)
        .frame(height: 38)
        .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(theme.field))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(theme.fieldBorder, lineWidth: 1))
    }

    private func sectionHeader(_ text: String) -> some View {
        Text(text.uppercased())
            .font(.system(size: 12, weight: .medium))
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
                Text(title).font(.system(size: Theme.body, weight: .medium)).foregroundStyle(theme.text)
                Text(subtitle).font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
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
                    .font(.system(size: Theme.small)).foregroundStyle(theme.muted)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: .infinity)
                    .padding(.horizontal, 36).padding(.top, 20)
            } else {
                VStack(spacing: 10) {
                    Text("No one on Arbos or GitHub matches @\(trimmed).")
                        .font(.system(size: Theme.small)).foregroundStyle(theme.muted)
                    if trimmed.count >= 2 {
                        Button { openHandle(trimmed) } label: {
                            Text("Invite @\(trimmed)")
                                .font(.system(size: Theme.small, weight: .semibold)).foregroundStyle(theme.onPrimary)
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

    private func openHandle(_ handle: String) {
        guard !opening else { return }
        opening = true
        Task {
            if await model.openDM(handle: handle) != nil { dismiss() }
            opening = false
        }
    }
}

/// One person: avatar, name, @handle and their Arbos status.
struct PersonRow: View {
    @Environment(\.theme) private var theme
    let person: Person
    var trailing: AnyView? = nil

    var body: some View {
        HStack(spacing: 14) {
            PersonAvatar(url: person.avatarUrl, handle: person.handle, size: 44)
            VStack(alignment: .leading, spacing: 2) {
                Text(person.displayName).font(.system(size: Theme.body, weight: .medium)).foregroundStyle(theme.text).lineLimit(1)
                Text("@\(person.handle)").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted).lineLimit(1)
            }
            Spacer()
            if let trailing {
                trailing
            } else {
                Text(PersonRow.statusLabel(person.status))
                    .font(.system(size: 11, weight: .medium))
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

/// New agent: a name, then `POST /projects`. The relay sets up its workspace.
struct NewAgentView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    /// Closes the whole sheet (a pushed view's own dismiss only pops).
    let onDone: () -> Void
    @State private var name = ""
    @FocusState private var focused: Bool

    private var clean: String { name.trimmingCharacters(in: .whitespaces) }

    var body: some View {
        VStack(spacing: 18) {
            Group {
                let trees = TreeAvatars.shared.characters
                if trees.isEmpty {
                    BlobAvatar(color: Theme.palette[NewAgentView.stableIndex(clean, Theme.palette.count)], shape: BlobShape.pick(for: clean), size: 92)
                } else {
                    let slug = trees[NewAgentView.stableIndex(clean, trees.count)].slug
                    TreeCharacterView(slug: slug, size: 92).id(slug)
                }
            }
            .padding(.top, 28)
            .animation(.snappy, value: clean)
            Text(clean.isEmpty ? "Name your agent" : "@\(mentionSlug(clean))")
                .font(.system(size: Theme.small)).foregroundStyle(theme.muted)
            TextField("", text: $name, prompt: Text("Agent name").foregroundStyle(theme.placeholder))
                .font(.system(size: Theme.body)).foregroundStyle(theme.text)
                .multilineTextAlignment(.center)
                .focused($focused)
                .submitLabel(.done)
                .onSubmit(create)
                .padding(.horizontal, 16).frame(height: 50)
                .background(RoundedRectangle(cornerRadius: 25, style: .continuous).fill(theme.field))
                .overlay(RoundedRectangle(cornerRadius: 25, style: .continuous).strokeBorder(theme.fieldBorder, lineWidth: 1))
                .padding(.horizontal, 24)
            Button(action: create) {
                Text("Create agent")
                    .font(.system(size: Theme.body, weight: .semibold)).foregroundStyle(theme.onPrimary)
                    .frame(maxWidth: .infinity).frame(height: 50)
                    .background(Capsule().fill(theme.primary))
            }
            .buttonStyle(.plain)
            .disabled(clean.isEmpty)
            .opacity(clean.isEmpty ? 0.45 : 1)
            .padding(.horizontal, 24)
            Text("A new agent with its own workspace on your machine. You can rename it, pick its colour and model from its profile.")
                .font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                .multilineTextAlignment(.center).padding(.horizontal, 36)
            Spacer()
        }
        .background(theme.bg)
        .navigationTitle("New agent")
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(theme.bg, for: .navigationBar)
        .toolbarBackground(.visible, for: .navigationBar)
        .onAppear { focused = true }
    }

    /// FNV-1a so the preview colour is the same on every launch (Swift's
    /// `hashValue` is seeded per process).
    static func stableIndex(_ text: String, _ count: Int) -> Int {
        var h: UInt32 = 2166136261
        for b in text.utf8 { h = (h ^ UInt32(b)) &* 16777619 }
        return Int(h % UInt32(max(count, 1)))
    }

    private func create() {
        guard !clean.isEmpty else { return }
        model.createProject(name: clean)
        onDone()
    }
}

/// New group: pick people from the directory (or search), then create.
/// Groups are v2 on the relay; the model turns a 404/405 into a notice.
struct NewGroupView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    let onDone: () -> Void
    @State private var query = ""
    @State private var remote: [Person] = []
    @State private var picked: [Person] = []
    @State private var creating = false

    private var trimmed: String { query.trimmingCharacters(in: .whitespaces).trimmingCharacters(in: CharacterSet(charactersIn: "@")) }

    private var candidates: [Person] {
        let q = trimmed.lowercased()
        var out = q.isEmpty ? model.directory : model.directory.filter { $0.handle.lowercased().contains(q) || ($0.name ?? "").lowercased().contains(q) }
        var seen = Set(out.map(\.id))
        for p in remote where !seen.contains(p.id) && p.id != model.user?.id { seen.insert(p.id); out.append(p) }
        return out
    }

    var body: some View {
        VStack(spacing: 0) {
            if !picked.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(picked) { p in
                            HStack(spacing: 6) {
                                PersonAvatar(url: p.avatarUrl, handle: p.handle, size: 22)
                                Text("@\(p.handle)").font(.system(size: Theme.tiny, weight: .medium)).foregroundStyle(theme.text)
                                Button { toggle(p) } label: { Image(systemName: "xmark").font(.system(size: 10, weight: .bold)).foregroundStyle(theme.muted) }
                                    .buttonStyle(.plain)
                            }
                            .padding(.leading, 4).padding(.trailing, 10).frame(height: 32)
                            .background(Capsule().fill(theme.field))
                        }
                    }
                    .padding(.horizontal, 16)
                }
                .frame(height: 44)
            }
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass").font(.system(size: 15, weight: .medium)).foregroundStyle(theme.muted)
                TextField("", text: $query, prompt: Text("Add people").foregroundStyle(theme.placeholder))
                    .textInputAutocapitalization(.never).autocorrectionDisabled()
                    .font(.system(size: Theme.body)).foregroundStyle(theme.text)
            }
            .padding(.horizontal, 12).frame(height: 38)
            .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(theme.field))
            .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(theme.fieldBorder, lineWidth: 1))
            .padding(.horizontal, 16).padding(.vertical, 10)
            .onChange(of: query) { _, _ in search() }

            ScrollView {
                LazyVStack(spacing: 0) {
                    ForEach(Array(candidates.enumerated()), id: \.element.id) { i, p in
                        Button { toggle(p) } label: {
                            PersonRow(person: p, trailing: AnyView(
                                Image(systemName: picked.contains(p) ? "checkmark.circle.fill" : "circle")
                                    .font(.system(size: 22))
                                    .foregroundStyle(picked.contains(p) ? theme.text : theme.muted.opacity(0.5))
                            ))
                        }
                        .buttonStyle(.plain)
                        if i < candidates.count - 1 { Rectangle().fill(theme.separator).frame(height: 0.5).padding(.leading, 72) }
                    }
                    if candidates.isEmpty {
                        Text(trimmed.isEmpty ? "No one else on Arbos yet. Search a GitHub username." : "No one matches @\(trimmed).")
                            .font(.system(size: Theme.small)).foregroundStyle(theme.muted)
                            .multilineTextAlignment(.center).padding(.horizontal, 36).padding(.top, 24)
                    }
                }
                .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(theme.card))
                .padding(.horizontal, 16)
                Color.clear.frame(height: 40)
            }
        }
        .background(theme.bg)
        .navigationTitle("New group")
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(theme.bg, for: .navigationBar)
        .toolbarBackground(.visible, for: .navigationBar)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button(creating ? "Creating…" : "Create") { create() }
                    .font(.system(size: Theme.body, weight: .semibold))
                    .disabled(picked.isEmpty || creating)
            }
        }
        .task { await model.loadDirectory() }
    }

    private func toggle(_ p: Person) {
        if let i = picked.firstIndex(of: p) { picked.remove(at: i) } else { picked.append(p) }
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
        Task {
            if await model.createGroup(handles: picked.map(\.handle)) { onDone() }
            creating = false
        }
    }
}

/// Members of a people chat: people, shared agents (with owner), lock state,
/// Add agent (own projects), reply policy, remove, block, leave, delete.
struct MembersSheet: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    let chatId: String
    @State private var showAddAgent = false
    @State private var confirmDelete = false
    @State private var confirmLeave = false

    private var chat: PeopleChat? { model.peopleChat(chatId) }
    private var myProjects: [Project] { model.projects.filter { !$0.shared } }

    var body: some View {
        NavigationStack {
            List {
                if let chat {
                    Section {
                        ForEach(chat.humans) { m in
                            HStack(spacing: 12) {
                                PersonAvatar(url: m.avatarUrl, handle: m.handle, size: 40)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(m.displayName).foregroundStyle(theme.text)
                                    Text("@\(m.handle)\(m.status == "invited" ? " · invited" : "")").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                                }
                                Spacer()
                                if m.id == model.user?.id { Text("You").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted) }
                            }
                            .swipeActions {
                                if m.id != model.user?.id {
                                    Button("Block") { model.block(m) }.tint(theme.danger)
                                }
                            }
                        }
                    } header: { Text("People") } footer: { Text(chat.lockLabel) }
                    .listRowBackground(theme.card)

                    Section {
                        ForEach(chat.agents) { a in
                            HStack(spacing: 12) {
                                if let p = a.project { AgentAvatar(project: p, theme: theme, size: 40) }
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(a.displayName).foregroundStyle(theme.text)
                                    Text("@\(mentionSlug(a.project?.name ?? a.handle)) · owned by @\(a.ownerHandle ?? "?")\(a.replyPolicy == "every_message" ? " · replies to everything" : "")")
                                        .font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                                }
                                Spacer()
                                if a.ownerId == model.user?.id {
                                    Menu {
                                        Button("Reply when mentioned") { model.setReplyPolicy(chatId: chatId, projectId: a.id, policy: "mention") }
                                        Button("Reply to every message") { model.setReplyPolicy(chatId: chatId, projectId: a.id, policy: "every_message") }
                                        Divider()
                                        Button("Remove agent", role: .destructive) { model.removeAgent(chatId: chatId, projectId: a.id) }
                                    } label: {
                                        Image(systemName: "ellipsis.circle").foregroundStyle(theme.muted)
                                    }
                                }
                            }
                        }
                        Button { showAddAgent = true } label: {
                            Label("Add agent", systemImage: "plus.circle").foregroundStyle(theme.text)
                        }
                        .disabled(myProjects.isEmpty)
                    } header: { Text("Agents") } footer: {
                        Text(chat.agents.isEmpty ? "Add one of your agents so both of you can talk to it. Mention it with @ to get a reply." : "Guests can ask a shared agent to do anything its owner can. The owner pays and can remove it any time.")
                    }
                    .listRowBackground(theme.card)

                    Section {
                        Button("Leave chat", role: .destructive) { confirmLeave = true }
                        if chat.myRole == "owner" {
                            Button("Delete chat for everyone", role: .destructive) { confirmDelete = true }
                        }
                    }
                    .listRowBackground(theme.card)
                }
            }
            .scrollContentBackground(.hidden)
            .background(theme.bg)
            .navigationTitle("Members")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } } }
            .sheet(isPresented: $showAddAgent) {
                AgentPicker(projects: myProjects) { p in
                    model.addAgent(chatId: chatId, projectId: p.id)
                }
                .environment(\.theme, theme)
            }
            .confirmationDialog("Leave this chat?", isPresented: $confirmLeave, titleVisibility: .visible) {
                Button("Leave", role: .destructive) { model.leavePeopleChat(chatId); dismiss() }
            }
            .confirmationDialog("Delete for everyone?", isPresented: $confirmDelete, titleVisibility: .visible) {
                Button("Delete chat", role: .destructive) { model.deletePeopleChat(chatId); dismiss() }
            } message: { Text("Messages, files and the sync log are removed for every member.") }
        }
        .presentationDetents([.medium, .large])
    }
}

/// Pick one of my projects to share into a chat.
struct AgentPicker: View {
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    let projects: [Project]
    let onPick: (Project) -> Void

    var body: some View {
        NavigationStack {
            List(projects) { p in
                Button { onPick(p); dismiss() } label: {
                    HStack(spacing: 12) {
                        AgentAvatar(project: p, theme: theme, size: 40)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(p.name).foregroundStyle(theme.text)
                            Text("@\(mentionSlug(p.name))").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                        }
                    }
                }
                .listRowBackground(theme.card)
            }
            .scrollContentBackground(.hidden)
            .background(theme.bg)
            .navigationTitle("Add agent")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarLeading) { Button("Cancel") { dismiss() } } }
        }
        .presentationDetents([.medium])
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

    var body: some View {
        NavigationStack {
            List {
                if !project.shared {
                    Section {
                        HStack(spacing: 8) {
                            Text("@").foregroundStyle(theme.muted)
                            TextField("", text: $handle, prompt: Text("GitHub username").foregroundStyle(theme.placeholder))
                                .textInputAutocapitalization(.never).autocorrectionDisabled()
                                .onSubmit(invite)
                            Button(busy ? "…" : "Invite", action: invite)
                                .font(.system(size: Theme.small, weight: .semibold))
                                .disabled(busy || handle.trimmingCharacters(in: .whitespaces).isEmpty)
                        }
                    } footer: {
                        Text("A member drives this agent with you, sees its chats and files, and spends your credits. You can remove them any time.")
                    }
                    .listRowBackground(theme.card)
                }
                Section("Members") {
                    ForEach(members) { m in
                        HStack(spacing: 12) {
                            PersonAvatar(url: m.avatarUrl, handle: m.handle, size: 40)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(m.name?.isEmpty == false ? m.name! : m.handle).foregroundStyle(theme.text)
                                Text("@\(m.handle) · \(m.role)\(m.status == "invited" ? " · invited" : "")").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
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
            .task { await load() }
        }
        .presentationDetents([.medium, .large])
    }

    private func load() async { members = await model.projectMembers(project.id) }

    private func invite() {
        let h = handle.trimmingCharacters(in: .whitespaces)
        guard !h.isEmpty, !busy else { return }
        busy = true
        Task {
            if let list = await model.inviteToProject(project.id, handle: h) { members = list; handle = "" }
            busy = false
        }
    }
}
