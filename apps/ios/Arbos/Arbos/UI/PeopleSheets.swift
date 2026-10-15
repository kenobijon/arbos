// People-chat sheets (D72): new chat (@handle search or new project),
// members (people, agents, add-agent picker, block, leave/delete) and
// project members (invite by handle).

import SwiftUI

/// "+" on home: Person (search @handle → DM) or Project (create).
struct NewChatSheet: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    @State private var mode = 0
    @State private var query = ""
    @State private var results: [Person] = []
    @State private var searching = false
    @State private var opening = false
    @State private var projectName = ""
    @FocusState private var focused: Bool

    var body: some View {
        NavigationStack {
            VStack(spacing: 14) {
                Picker("", selection: $mode) {
                    Text("Person").tag(0)
                    Text("Project").tag(1)
                }
                .pickerStyle(.segmented)
                .padding(.horizontal, 16)
                if mode == 0 {
                    HStack(spacing: 10) {
                        Text("@").font(.system(size: Theme.body, weight: .medium)).foregroundStyle(theme.muted)
                        TextField("", text: $query, prompt: Text("GitHub username").foregroundStyle(theme.placeholder))
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .font(.system(size: Theme.body))
                            .foregroundStyle(theme.text)
                            .focused($focused)
                            .submitLabel(.go)
                            .onSubmit { if let first = results.first { open(first) } else if !query.isEmpty { openHandle(query) } }
                        if searching { ProgressView().controlSize(.small).tint(theme.muted) }
                    }
                    .padding(.horizontal, 16)
                    .frame(height: 50)
                    .background(RoundedRectangle(cornerRadius: 25, style: .continuous).fill(theme.field))
                    .overlay(RoundedRectangle(cornerRadius: 25, style: .continuous).strokeBorder(theme.fieldBorder, lineWidth: 1))
                    .padding(.horizontal, 16)
                    .onChange(of: query) { _, q in search(q) }

                    ScrollView {
                        LazyVStack(spacing: 0) {
                            ForEach(results) { p in
                                Button { open(p) } label: {
                                    HStack(spacing: 12) {
                                        PersonAvatar(url: p.avatarUrl, handle: p.handle, size: 44)
                                        VStack(alignment: .leading, spacing: 2) {
                                            Text(p.displayName).font(.system(size: Theme.body, weight: .semibold)).foregroundStyle(theme.text)
                                            Text("@\(p.handle)").font(.system(size: Theme.small)).foregroundStyle(theme.muted)
                                        }
                                        Spacer()
                                        Text(statusLabel(p.status))
                                            .font(.system(size: Theme.tiny, weight: .medium))
                                            .foregroundStyle(p.status == "member" ? theme.online : theme.muted)
                                            .padding(.horizontal, 10).padding(.vertical, 5)
                                            .background(Capsule().fill(theme.field))
                                    }
                                    .padding(.horizontal, 16).padding(.vertical, 10)
                                    .contentShape(Rectangle())
                                }
                                .buttonStyle(.plain)
                            }
                            if results.isEmpty && !query.isEmpty && !searching {
                                Text("No one on Arbos or GitHub matches @\(query) yet.")
                                    .font(.system(size: Theme.small)).foregroundStyle(theme.muted).padding(.top, 24)
                            } else if query.isEmpty {
                                Text("Anyone with a GitHub account. The chat opens for both of you at once; people not on Arbos yet see it when they sign in.")
                                    .font(.system(size: Theme.small)).foregroundStyle(theme.muted)
                                    .multilineTextAlignment(.center).padding(.horizontal, 32).padding(.top, 24)
                            }
                        }
                    }
                } else {
                    HStack(spacing: 10) {
                        TextField("", text: $projectName, prompt: Text("Project name").foregroundStyle(theme.placeholder))
                            .font(.system(size: Theme.body)).foregroundStyle(theme.text)
                            .submitLabel(.done)
                            .onSubmit(createProject)
                    }
                    .padding(.horizontal, 16).frame(height: 50)
                    .background(RoundedRectangle(cornerRadius: 25, style: .continuous).fill(theme.field))
                    .overlay(RoundedRectangle(cornerRadius: 25, style: .continuous).strokeBorder(theme.fieldBorder, lineWidth: 1))
                    .padding(.horizontal, 16)
                    Button(action: createProject) {
                        Text("Create project")
                            .font(.system(size: Theme.body, weight: .semibold)).foregroundStyle(theme.onPrimary)
                            .frame(maxWidth: .infinity).frame(height: 50)
                            .background(Capsule().fill(theme.primary))
                    }
                    .buttonStyle(.plain)
                    .disabled(projectName.trimmingCharacters(in: .whitespaces).isEmpty)
                    .opacity(projectName.trimmingCharacters(in: .whitespaces).isEmpty ? 0.45 : 1)
                    .padding(.horizontal, 16)
                    Text("A new agent with its own workspace on your machine.")
                        .font(.system(size: Theme.small)).foregroundStyle(theme.muted)
                    Spacer()
                }
            }
            .padding(.top, 8)
            .background(theme.bg)
            .navigationTitle("New chat")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarLeading) { Button("Cancel") { dismiss() } } }
            .onAppear { focused = true }
        }
        .presentationDetents([.large])
    }

    private func statusLabel(_ s: String) -> String {
        switch s {
        case "member": return "On Arbos"
        case "invited": return "Invited"
        default: return "Invite"
        }
    }

    private func search(_ q: String) {
        let trimmed = q.trimmingCharacters(in: .whitespaces)
        guard trimmed.count >= 2 else { results = []; return }
        searching = true
        Task {
            try? await Task.sleep(nanoseconds: 250_000_000)
            guard trimmed == query.trimmingCharacters(in: .whitespaces) else { return }
            results = await model.searchPeople(trimmed)
            searching = false
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

    private func createProject() {
        let name = projectName.trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty else { return }
        model.createProject(name: name)
        dismiss()
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
                                if let p = a.project { BlobAvatar(project: p, theme: theme, size: 40) }
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
                        BlobAvatar(project: p, theme: theme, size: 40)
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
