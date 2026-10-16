// Telegram-style profile pages in Arbos/Grok Bot dress: a big avatar, name,
// status line, a row of glass action tiles, an info card, a pill of tabs,
// then the tab's list. Three pages share the scaffold: a person
// (`UserProfileView`), a group chat (`GroupProfileView`, people groups and
// shared project chats), and an agent (`AgentProfileView`, its own file).
// `AppModel.showProfile(_:)` opens one from anywhere; the root presents it.

import SwiftUI

/// Who a user page is about — from a chat member, a search result or a sender row.
struct ProfilePerson: Identifiable, Equatable, Hashable {
    var id: String
    var handle: String
    var name: String?
    var avatarUrl: String?
    /// §9 presence as last reported by the relay for this row.
    var lastSeenAt: Int64? = nil
    var online: Bool = false

    var displayName: String { name?.isEmpty == false ? name! : "@\(handle)" }

    init(id: String, handle: String, name: String?, avatarUrl: String?, lastSeenAt: Int64? = nil, online: Bool = false) {
        self.id = id; self.handle = handle; self.name = name; self.avatarUrl = avatarUrl; self.lastSeenAt = lastSeenAt; self.online = online
    }
    init(_ m: ChatMember) { self.init(id: m.id, handle: m.handle, name: m.name, avatarUrl: m.avatarUrl, lastSeenAt: m.lastSeenAt, online: m.online) }
    init(_ p: Person) { self.init(id: p.id, handle: p.handle, name: p.name, avatarUrl: p.avatarUrl, lastSeenAt: p.lastSeenAt, online: p.online) }
    init(_ p: ProjectMember) { self.init(id: p.id, handle: p.handle, name: p.name, avatarUrl: p.avatarUrl, lastSeenAt: p.lastSeenAt, online: p.online) }
}

/// What the root view presents.
enum ProfileTarget: Identifiable, Equatable {
    case user(ProfilePerson)
    case group(chatId: String, projectId: String?)
    case agent(projectId: String)

    var id: String {
        switch self {
        case .user(let p): return "user:\(p.id)"
        case .group(let c, _): return "group:\(c)"
        case .agent(let p): return "agent:\(p)"
        }
    }
}

// MARK: - Scaffold pieces

/// Back button + optional trailing button over the header.
struct ProfileTopBar<Trailing: View>: View {
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    @ViewBuilder var trailing: () -> Trailing

    var body: some View {
        HStack {
            Button { dismiss() } label: {
                GlassCircle { Image(systemName: "chevron.left").font(.system(size: 17, weight: .semibold)).foregroundStyle(theme.text) }
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Back")
            Spacer()
            trailing()
        }
        .padding(.horizontal, 16)
        .padding(.top, 6)
    }
}

struct ProfileHeader<Avatar: View>: View {
    @Environment(\.theme) private var theme
    let name: String
    let status: String
    var statusOnline = false
    @ViewBuilder var avatar: () -> Avatar

    var body: some View {
        VStack(spacing: 10) {
            avatar().frame(width: 112, height: 112)
            Text(name)
                .font(.system(size: 28, weight: .semibold))
                .foregroundStyle(theme.text)
                .lineLimit(1)
                .padding(.horizontal, 24)
            Text(status)
                .font(.system(size: Theme.small))
                .foregroundStyle(statusOnline ? Color(hex: "#1084FE")! : theme.muted)
        }
        .padding(.top, -24)
        .frame(maxWidth: .infinity)
    }
}

struct ProfileAction: Identifiable {
    var id: String { label }
    let icon: String
    let label: String
    var enabled = true
    var soon = false
    var destructive = false
    let action: () -> Void
}

/// Telegram's row of square tiles (equal widths, blue icon + label).
struct ProfileActionRow: View {
    @Environment(\.theme) private var theme
    let actions: [ProfileAction]

    private var accent: Color { Color(hex: "#1084FE")! }

    var body: some View {
        HStack(spacing: 10) {
            ForEach(actions) { a in
                Button(action: a.action) {
                    VStack(spacing: 6) {
                        ZStack(alignment: .topTrailing) {
                            Image(systemName: a.icon).font(.system(size: 20, weight: .medium))
                            if a.soon {
                                Text("soon").font(.system(size: 8, weight: .bold)).foregroundStyle(theme.bg)
                                    .padding(.horizontal, 4).padding(.vertical, 1)
                                    .background(Capsule().fill(theme.muted))
                                    .offset(x: 14, y: -8)
                            }
                        }
                        Text(a.label).font(.system(size: 12))
                    }
                    .foregroundStyle(a.destructive ? theme.danger : (a.enabled ? accent : theme.muted))
                    .frame(maxWidth: .infinity).frame(height: 64)
                    .background(RoundedRectangle(cornerRadius: 14, style: .continuous).fill(theme.card))
                }
                .buttonStyle(.plain)
                .disabled(!a.enabled)
                .accessibilityLabel(a.label + (a.soon ? ", coming soon" : ""))
            }
        }
        .padding(.horizontal, 16)
    }
}

struct ProfileCard<Content: View>: View {
    @Environment(\.theme) private var theme
    @ViewBuilder var content: () -> Content

    var body: some View {
        VStack(spacing: 0) { content() }
            .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(theme.card))
            .padding(.horizontal, 16)
    }
}

struct ProfileRowDivider: View {
    @Environment(\.theme) private var theme
    var inset: CGFloat = 16
    var body: some View { Rectangle().fill(theme.separator).frame(height: 0.5).padding(.leading, inset) }
}

/// Label-over-value row ("username" / "@handle"), optional trailing glyph.
struct ProfileInfoRow: View {
    @Environment(\.theme) private var theme
    let label: String
    let value: String
    var tint: Color? = nil
    var trailingIcon: String? = nil

    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 3) {
                Text(label).font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                Text(value).font(.system(size: Theme.body)).foregroundStyle(tint ?? theme.text).textSelection(.enabled)
            }
            Spacer()
            if let trailingIcon { Image(systemName: trailingIcon).font(.system(size: 18)).foregroundStyle(Color(hex: "#1084FE")!) }
        }
        .padding(.horizontal, 16).padding(.vertical, 12)
    }
}

/// Blue (or red) one-line action row inside a card.
struct ProfileLinkRow: View {
    @Environment(\.theme) private var theme
    let title: String
    var destructive = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack {
                Text(title).font(.system(size: Theme.body)).foregroundStyle(destructive ? theme.danger : Color(hex: "#1084FE")!)
                Spacer()
            }
            .padding(.horizontal, 16).frame(height: 48)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

/// Telegram's pill of tabs (dark capsule, light capsule on the selected one).
struct ProfileTabs: View {
    @Environment(\.theme) private var theme
    let tabs: [String]
    @Binding var selected: String

    var body: some View {
        HStack(spacing: 2) {
            ForEach(tabs, id: \.self) { t in
                Button { withAnimation(.snappy(duration: 0.2)) { selected = t } } label: {
                    Text(t)
                        .font(.system(size: Theme.small, weight: .medium))
                        .foregroundStyle(theme.text)
                        .padding(.horizontal, 16).frame(height: 36)
                        .background(Capsule().fill(selected == t ? theme.field : .clear))
                        .overlay(Capsule().strokeBorder(selected == t ? theme.fieldBorder : .clear, lineWidth: 1))
                }
                .buttonStyle(.plain)
            }
        }
        .padding(4)
        .background(Capsule().fill(theme.card))
        .overlay(Capsule().strokeBorder(theme.fieldBorder, lineWidth: 1))
    }
}

/// Last-seen text: the relay's `online`/`last_seen_at` (§9) merged with the
/// presence frames seen this session; "last seen recently" when unknown.
func lastSeenLine(_ at: Int64?, online: Bool = false, now: Int64 = nowMs()) -> (text: String, online: Bool) {
    if online { return ("online", true) }
    guard let at, at > 0 else { return ("last seen recently", false) }
    let age = max(0, now - at) / 1000
    if age < 90 { return ("online", true) }
    if age < 3600 { return ("last seen \(age / 60) min ago", false) }
    if age < 86_400 {
        let d = Date(timeIntervalSince1970: Double(at) / 1000)
        return ("last seen today at \(d.formatted(date: .omitted, time: .shortened))", false)
    }
    if age < 7 * 86_400 { return ("last seen \(age / 86_400) day\(age / 86_400 == 1 ? "" : "s") ago", false) }
    return ("last seen a long time ago", false)
}

// MARK: - Media / files / voice / links from a chat's messages

struct ChatItem: Identifiable, Equatable {
    enum Kind { case media, file, voice, link }
    var id: String
    var kind: Kind
    var title: String
    var subtitle: String
    var url: URL?
    var at: Int64
    var fromHandle: String?
    var mime: String
}

enum ChatMedia {
    private static let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue)

    @MainActor
    static func items(messages: [Message], slot: String, model: AppModel) -> [ChatItem] {
        var out: [ChatItem] = []
        for m in messages {
            for part in m.parts {
                switch part {
                case .attachment(let id, let a):
                    let kind: ChatItem.Kind = a.mime.hasPrefix("audio/") || a.kind == "audio" ? .voice
                        : (a.mime.hasPrefix("image/") || a.mime.hasPrefix("video/") || a.kind == "image") ? .media : .file
                    let url = a.blobRef.flatMap { model.blobURL(projectId: slot, key: $0) }
                    out.append(ChatItem(id: "\(m.id)#\(id)", kind: kind, title: a.name,
                                        subtitle: a.mime, url: url, at: m.createdAt, fromHandle: m.senderHandle, mime: a.mime))
                case .text(let id, let text):
                    guard let detector else { continue }
                    let ns = text as NSString
                    for match in detector.matches(in: text, range: NSRange(location: 0, length: ns.length)) {
                        guard let url = match.url, url.scheme?.hasPrefix("http") == true else { continue }
                        out.append(ChatItem(id: "\(m.id)#\(id)#\(match.range.location)", kind: .link, title: url.host() ?? url.absoluteString,
                                            subtitle: url.absoluteString, url: url, at: m.createdAt, fromHandle: m.senderHandle, mime: "text/uri-list"))
                    }
                default: continue
                }
            }
        }
        return out.sorted { $0.at > $1.at }
    }
}

struct ChatItemsList: View {
    @Environment(\.theme) private var theme
    let items: [ChatItem]
    let kind: ChatItem.Kind
    let empty: String

    private var filtered: [ChatItem] { items.filter { $0.kind == kind } }

    var body: some View {
        if filtered.isEmpty {
            Text(empty).font(.system(size: Theme.small)).foregroundStyle(theme.muted)
                .frame(maxWidth: .infinity).padding(.top, 36).padding(.horizontal, 32).multilineTextAlignment(.center)
        } else if kind == .media {
            LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 2), count: 3), spacing: 2) {
                ForEach(filtered) { item in
                    ZStack {
                        Rectangle().fill(theme.card)
                        if let url = item.url, item.mime.hasPrefix("image/") {
                            AsyncImage(url: url) { img in img.resizable().scaledToFill() } placeholder: { ProgressView().tint(theme.muted) }
                        } else {
                            Image(systemName: "film").font(.system(size: 22)).foregroundStyle(theme.muted)
                        }
                    }
                    .aspectRatio(1, contentMode: .fill)
                    .clipped()
                }
            }
            .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
            .padding(.horizontal, 16)
        } else {
            ProfileCard {
                ForEach(Array(filtered.enumerated()), id: \.element.id) { i, item in
                    rowView(item)
                    if i < filtered.count - 1 { ProfileRowDivider(inset: 68) }
                }
            }
        }
    }

    private func rowView(_ item: ChatItem) -> some View {
        Group {
            if let url = item.url {
                SwiftUI.Link(destination: url) { rowLabel(item) }
            } else {
                rowLabel(item)
            }
        }
        .buttonStyle(.plain)
    }

    private func rowLabel(_ item: ChatItem) -> some View {
        HStack(spacing: 12) {
            GlassCircle(size: 40) {
                Image(systemName: item.kind == .voice ? "waveform" : item.kind == .link ? "link" : "doc.fill")
                    .font(.system(size: 16, weight: .medium)).foregroundStyle(theme.text)
            }
            VStack(alignment: .leading, spacing: 3) {
                Text(item.title).font(.system(size: Theme.body)).foregroundStyle(theme.text).lineLimit(1)
                Text(item.kind == .link ? item.subtitle : "\(item.fromHandle.map { "@\($0) · " } ?? "")\(Date(timeIntervalSince1970: Double(item.at) / 1000).formatted(date: .abbreviated, time: .omitted))")
                    .font(.system(size: Theme.tiny)).foregroundStyle(item.kind == .link ? Color(hex: "#1084FE")! : theme.muted).lineLimit(1)
            }
            Spacer()
        }
        .padding(.horizontal, 14).padding(.vertical, 10)
        .contentShape(Rectangle())
    }
}

/// A member row: avatar, name, online/last seen, owner/admin badge.
struct MemberStatusRow: View {
    @Environment(\.theme) private var theme
    let person: ProfilePerson
    let status: (text: String, online: Bool)
    var badge: String? = nil
    var isMe = false
    var agent: Project? = nil

    var body: some View {
        HStack(spacing: 12) {
            if let agent {
                AgentAvatar(project: agent, theme: theme, size: 40, still: true)
            } else {
                PersonAvatar(url: person.avatarUrl, handle: person.handle, size: 40)
            }
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(agent?.name ?? person.displayName).font(.system(size: Theme.body)).foregroundStyle(theme.text).lineLimit(1)
                    if isMe { Text("you").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted) }
                }
                Text(status.text).font(.system(size: Theme.tiny)).foregroundStyle(status.online ? Color(hex: "#1084FE")! : theme.muted)
            }
            Spacer()
            if let badge {
                Text(badge).font(.system(size: 12, weight: .medium))
                    .foregroundStyle(badge == "owner" ? Color(hex: "#8B5CF6")! : theme.online)
                    .padding(.horizontal, 8).padding(.vertical, 3)
                    .background(Capsule().fill((badge == "owner" ? Color(hex: "#8B5CF6")! : theme.online).opacity(0.15)))
            }
        }
        .padding(.horizontal, 14).padding(.vertical, 9)
        .contentShape(Rectangle())
    }
}

// MARK: - User page

struct UserProfileView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    let person: ProfilePerson
    @State private var tab = "Links"
    @State private var muted = false
    @State private var sharedProjects: [Project] = []
    @State private var confirmBlock = false

    private var dm: PeopleChat? {
        model.peopleChats.first { $0.kind == "dm" && $0.humans.contains { $0.id == person.id } }
    }
    @State private var fresh: Person?
    private var status: (text: String, online: Bool) {
        lastSeenLine(max(fresh?.lastSeenAt ?? 0, person.lastSeenAt ?? 0, model.lastSeen[person.id] ?? 0), online: fresh?.online ?? person.online)
    }
    private var isMe: Bool { person.id == model.user?.id }
    private var items: [ChatItem] {
        guard let dm else { return [] }
        return ChatMedia.items(messages: model.messages(of: dm.id), slot: "\(peopleSlot):\(dm.id)", model: model)
    }
    private var sharedGroups: [PeopleChat] {
        model.peopleChats.filter { $0.kind != "dm" && $0.humans.contains { $0.id == person.id } }
    }

    var body: some View {
        ZStack(alignment: .top) {
            theme.bg.ignoresSafeArea()
            ScrollView {
                VStack(spacing: 18) {
                    Color.clear.frame(height: 70)
                    ProfileHeader(name: person.displayName, status: isMe ? "@\(person.handle)" : status.text, statusOnline: status.online) {
                        PersonAvatar(url: person.avatarUrl, handle: person.handle, size: 112)
                    }
                    if !isMe {
                        ProfileActionRow(actions: [
                            ProfileAction(icon: "phone.fill", label: "call") { call(.voice) },
                            ProfileAction(icon: "video.fill", label: "video") { call(.video) },
                            ProfileAction(icon: muted ? "bell.slash.fill" : "bell.fill", label: muted ? "unmute" : "mute") { toggleMute() },
                            ProfileAction(icon: "magnifyingglass", label: "search") { openDM(search: true) },
                            ProfileAction(icon: "ellipsis", label: "more") { },
                        ])
                    }
                    ProfileCard {
                        ProfileInfoRow(label: "username", value: "@\(person.handle)", tint: Color(hex: "#1084FE")!, trailingIcon: "qrcode")
                            .onTapGesture { UIPasteboard.general.string = "@\(person.handle)" }
                        if !isMe {
                            ProfileRowDivider()
                            ProfileLinkRow(title: dm == nil ? "Start a chat" : "Open chat") { openDM(search: false) }
                            ProfileRowDivider()
                            ProfileLinkRow(title: "Block User", destructive: true) { confirmBlock = true }
                        }
                    }
                    ProfileTabs(tabs: ["Links", "Groups"], selected: $tab)
                    if tab == "Links" {
                        ChatItemsList(items: items, kind: .link, empty: dm == nil ? "No chat with \(person.displayName) yet." : "No links shared yet.")
                    } else {
                        groupsList
                    }
                    Color.clear.frame(height: 40)
                }
            }
            ProfileTopBar { EmptyView() }
        }
        .confirmationDialog("Block @\(person.handle)?", isPresented: $confirmBlock, titleVisibility: .visible) {
            Button("Block", role: .destructive) {
                if let m = dm?.humans.first(where: { $0.id == person.id }) { model.block(m) } else { model.block(handle: person.handle) }
                dismiss()
            }
        } message: { Text("They cannot message you or add you to chats.") }
        .task {
            muted = dm?.muted ?? false
            fresh = await model.fetchUser(person.id)
            sharedProjects = await model.sharedProjects(with: person)
        }
    }

    private var groupsList: some View {
        let groups = sharedGroups
        return Group {
            if groups.isEmpty && sharedProjects.isEmpty {
                Text("No groups in common yet.").font(.system(size: Theme.small)).foregroundStyle(theme.muted).padding(.top, 36)
            } else {
                ProfileCard {
                    ForEach(groups) { g in
                        Button { dismiss(); model.open(peopleChat: g) } label: {
                            HStack(spacing: 12) {
                                GlassCircle(size: 40) { Image(systemName: "person.2.fill").font(.system(size: 15)).foregroundStyle(theme.text) }
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(g.title).font(.system(size: Theme.body)).foregroundStyle(theme.text)
                                    Text("\(g.humans.count) members").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                                }
                                Spacer()
                            }
                            .padding(.horizontal, 14).padding(.vertical, 9).contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        ProfileRowDivider(inset: 68)
                    }
                    ForEach(sharedProjects) { p in
                        Button { dismiss(); model.open(project: p) } label: {
                            HStack(spacing: 12) {
                                AgentAvatar(project: p, theme: theme, size: 40, still: true)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(p.name).font(.system(size: Theme.body)).foregroundStyle(theme.text)
                                    Text("shared project · @\(p.ownerHandle ?? model.user?.login ?? "")").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                                }
                                Spacer()
                            }
                            .padding(.horizontal, 14).padding(.vertical, 9).contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
        }
    }

    private func call(_ kind: CallKind) {
        Task {
            var chat = dm
            if chat == nil { chat = await model.openDM(handle: person.handle, navigate: false) }
            guard let chat else { return }
            dismiss()
            await CallManager.shared.start(chatId: chat.id, projectId: nil, kind: kind, title: person.displayName)
        }
    }

    private func toggleMute() {
        guard let dm else { return }
        muted.toggle()
        model.setMuted(chatId: dm.id, muted: muted)
    }

    private func openDM(search: Bool) {
        Task {
            var chat = dm
            if chat == nil { chat = await model.openDM(handle: person.handle, navigate: false) }
            if let chat {
                dismiss()
                if search { model.pendingSearchChat = chat.id }
                model.open(peopleChat: chat)
            }
        }
    }
}

// MARK: - Group page

struct GroupProfileView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    let chatId: String
    /// nil for people groups; the project for a shared project chat.
    let projectId: String?
    @State private var tab = "Members"
    @State private var muted = false
    @State private var projectMembers: [ProjectMember] = []
    @State private var showAdd = false
    @State private var confirmLeave = false

    private var people: PeopleChat? { model.peopleChat(chatId) }
    private var project: Project? { projectId.flatMap { model.project($0) } }
    private var title: String { project?.name ?? people?.title ?? "Group" }
    private var slot: String { projectId ?? "\(peopleSlot):\(chatId)" }
    private var items: [ChatItem] { ChatMedia.items(messages: model.messages(of: chatId), slot: slot, model: model) }

    private struct Row: Identifiable {
        var id: String
        var person: ProfilePerson
        var role: String
        var agent: Project?
    }

    private var rows: [Row] {
        if let people {
            return people.members.map { m in
                Row(id: m.id, person: ProfilePerson(m), role: m.role, agent: m.isAgent ? m.project : nil)
            }
        }
        var out = projectMembers.map { Row(id: $0.id, person: ProfilePerson($0), role: $0.role, agent: nil) }
        if let project { out.insert(Row(id: project.id, person: ProfilePerson(id: project.id, handle: mentionSlug(project.name), name: project.name, avatarUrl: nil), role: "agent", agent: project), at: 0) }
        return out
    }

    private var memberCount: Int { rows.filter { $0.agent == nil }.count }

    var body: some View {
        ZStack(alignment: .top) {
            theme.bg.ignoresSafeArea()
            ScrollView {
                VStack(spacing: 18) {
                    Color.clear.frame(height: 70)
                    ProfileHeader(name: title, status: "\(memberCount) member\(memberCount == 1 ? "" : "s")") {
                        if let project {
                            AgentAvatar(project: project, theme: theme, size: 112)
                        } else {
                            GlassCircle(size: 112) { Image(systemName: "person.2.fill").font(.system(size: 44)).foregroundStyle(theme.text) }
                        }
                    }
                    ProfileActionRow(actions: [
                        ProfileAction(icon: muted ? "bell.slash.fill" : "bell.fill", label: muted ? "unmute" : "mute") { toggleMute() },
                        ProfileAction(icon: "magnifyingglass", label: "search") { dismiss(); model.pendingSearchChat = chatId },
                        ProfileAction(icon: "rectangle.portrait.and.arrow.right", label: "leave", enabled: people != nil || project?.shared == true) { confirmLeave = true },
                        ProfileAction(icon: "ellipsis", label: "more") { },
                    ])
                    ProfileTabs(tabs: ["Members", "Media", "Files", "Voice", "Links"], selected: $tab)
                    switch tab {
                    case "Members": membersList
                    case "Media": ChatItemsList(items: items, kind: .media, empty: "No photos or videos shared yet.")
                    case "Files": ChatItemsList(items: items, kind: .file, empty: "No files shared yet.")
                    case "Voice": ChatItemsList(items: items, kind: .voice, empty: "No voice messages yet.")
                    default: ChatItemsList(items: items, kind: .link, empty: "No links shared yet.")
                    }
                    Color.clear.frame(height: 40)
                }
            }
            ProfileTopBar {
                if project != nil, project?.shared != true {
                    Button("Edit") { model.showProfile(.agent(projectId: projectId!)) }
                        .font(.system(size: Theme.body, weight: .medium)).foregroundStyle(theme.text)
                        .padding(.horizontal, 14).frame(height: Theme.control)
                        .background(Capsule().fill(theme.field))
                }
            }
        }
        .confirmationDialog("Leave \(title)?", isPresented: $confirmLeave, titleVisibility: .visible) {
            Button("Leave", role: .destructive) {
                if people != nil { model.leavePeopleChat(chatId) }
                dismiss()
            }
        }
        .sheet(isPresented: $showAdd) {
            if let project {
                ProjectMembersSheet(project: project).environmentObject(model).environment(\.theme, theme)
            } else {
                MembersSheet(chatId: chatId).environmentObject(model).environment(\.theme, theme)
            }
        }
        .task {
            muted = people?.muted ?? false
            if let projectId { projectMembers = await model.projectMembers(projectId) }
        }
    }

    private var membersList: some View {
        ProfileCard {
            Button { showAdd = true } label: {
                HStack(spacing: 12) {
                    GlassCircle(size: 40) { Image(systemName: "person.badge.plus").font(.system(size: 16, weight: .medium)).foregroundStyle(Color(hex: "#1084FE")!) }
                    Text("Add Members").font(.system(size: Theme.body)).foregroundStyle(Color(hex: "#1084FE")!)
                    Spacer()
                }
                .padding(.horizontal, 14).padding(.vertical, 9).contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            ForEach(rows) { r in
                ProfileRowDivider(inset: 68)
                Button {
                    if let a = r.agent { model.showProfile(.agent(projectId: a.id)) } else { model.showProfile(.user(r.person)) }
                } label: {
                    MemberStatusRow(person: r.person,
                                    status: r.agent != nil ? (model.isStreaming(project: r.agent!.id) ? "working" : "idle", false)
                                        : (r.person.id == model.user?.id ? ("online", true)
                                           : lastSeenLine(max(r.person.lastSeenAt ?? 0, model.lastSeen[r.person.id] ?? 0), online: r.person.online)),
                                    badge: r.role == "owner" ? "owner" : (r.role == "admin" ? "admin" : nil),
                                    isMe: r.person.id == model.user?.id, agent: r.agent)
                }
                .buttonStyle(.plain)
            }
        }
    }

    private func toggleMute() {
        muted.toggle()
        model.setMuted(chatId: chatId, muted: muted)
    }
}
