// The "Chats" bottom sheet of one agent (chat icon beside the agent name):
// every chat the user has with the agent — title, last line, time, unread —
// plus its agent-hosted groups (group icon). "+" starts a new chat. Swipe a
// row to archive it: that hides it from this list only; history stays in the
// agent's context/memory tree and searchable history. An "Archived" row at
// the bottom lists archived chats, where swipe unarchives.

import SwiftUI

struct ChatListView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    let projectId: Id
    @State private var creating = false

    private var project: Project? { model.project(projectId) }
    private var active: [Chat] { model.chats(of: projectId).filter { !model.isChatArchived($0.id) } }
    private var archived: [Chat] { model.chats(of: projectId).filter { model.isChatArchived($0.id) } }

    var body: some View {
        NavigationStack {
            List {
                ForEach(active) { chat in
                    row(chat)
                        .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                            Button { withAnimation { model.archiveChat(chat.id) } } label: {
                                Label("Archive", systemImage: "archivebox")
                            }
                            .tint(theme.muted)
                        }
                }
                if !archived.isEmpty {
                    NavigationLink {
                        ArchivedChatsView(projectId: projectId)
                    } label: {
                        HStack(spacing: 12) {
                            ZStack {
                                Circle().fill(theme.card).frame(width: 40, height: 40)
                                Image(systemName: "archivebox")
                                    .font(.system(size: 16, weight: .medium))
                                    .foregroundStyle(theme.muted)
                            }
                            Text("Archived")
                                .font(.system(size: Theme.body))
                                .foregroundStyle(theme.text)
                            Spacer()
                            Text("\(archived.count)")
                                .font(.system(size: Theme.small))
                                .foregroundStyle(theme.muted)
                        }
                        .padding(.vertical, 4)
                    }
                    .listRowBackground(theme.bg)
                    .listRowSeparatorTint(theme.separator)
                }
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .background(theme.bg)
            .navigationTitle("Chats")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) { Button("Done") { dismiss() } }
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        guard !creating else { return }
                        creating = true
                        Task {
                            if let chat = await model.newChat(projectId: projectId) {
                                model.open(chat: chat)
                                dismiss()
                            }
                            creating = false
                        }
                    } label: {
                        if creating { ProgressView() } else { Image(systemName: "plus") }
                    }
                    .accessibilityLabel("New chat")
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
    }

    private func row(_ chat: Chat) -> some View {
        Button {
            model.open(chat: chat)
            dismiss()
        } label: {
            ChatRow(chat: chat, project: project, preview: ChatListView.preview(of: chat, in: model),
                    time: ChatListView.time(of: chat, in: model), unread: model.chatHasUnread(chat), streaming: model.isStreaming(chat.id))
        }
        .listRowBackground(theme.bg)
        .listRowSeparatorTint(theme.separator)
    }

    static func preview(of chat: Chat, in model: AppModel) -> String {
        let msgs = model.messages(of: chat.id)
        guard let last = msgs.last(where: { !$0.plainText.isEmpty }) else {
            return msgs.isEmpty ? "" : "…"
        }
        let line = last.plainText.split(separator: "\n").first.map(String.init) ?? ""
        return (last.role == .user ? "You: " : "") + HomeView.plain(line)
    }

    static func time(of chat: Chat, in model: AppModel) -> String {
        guard let ms = model.messages(of: chat.id).last?.createdAt, ms > 0 else { return "" }
        let date = Date(timeIntervalSince1970: TimeInterval(ms) / 1000)
        if Calendar.current.isDateInToday(date) {
            return date.formatted(date: .omitted, time: .shortened)
        }
        if let week = Calendar.current.date(byAdding: .day, value: -6, to: Date()), date > week {
            return date.formatted(.dateTime.weekday(.abbreviated))
        }
        return date.formatted(.dateTime.day().month(.abbreviated))
    }
}

/// Archived chats of one agent. Swipe (or full swipe) a row to unarchive it.
struct ArchivedChatsView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    let projectId: Id

    private var project: Project? { model.project(projectId) }
    private var archived: [Chat] { model.chats(of: projectId).filter { model.isChatArchived($0.id) } }

    var body: some View {
        Group {
            if archived.isEmpty {
                VStack(spacing: 8) {
                    Image(systemName: "archivebox").font(.system(size: 28)).foregroundStyle(theme.muted)
                    Text("No archived chats").font(.system(size: Theme.body)).foregroundStyle(theme.muted)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                List {
                    ForEach(archived) { chat in
                        Button {
                            model.open(chat: chat)
                        } label: {
                            ChatRow(chat: chat, project: project, preview: ChatListView.preview(of: chat, in: model),
                                    time: ChatListView.time(of: chat, in: model), unread: false, streaming: model.isStreaming(chat.id))
                        }
                        .listRowBackground(theme.bg)
                        .listRowSeparatorTint(theme.separator)
                        .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                            Button { withAnimation { model.unarchiveChat(chat.id) } } label: {
                                Label("Unarchive", systemImage: "arrow.uturn.backward")
                            }
                            .tint(Color(hex: "#1084FE")!)
                        }
                    }
                }
                .listStyle(.plain)
                .scrollContentBackground(.hidden)
            }
        }
        .background(theme.bg)
        .navigationTitle("Archived")
        .navigationBarTitleDisplayMode(.inline)
    }
}

extension Chat: Hashable {
    func hash(into hasher: inout Hasher) { hasher.combine(id) }
}

struct ChatRow: View {
    @Environment(\.theme) private var theme
    let chat: Chat
    let project: Project?
    let preview: String
    var time: String = ""
    var unread: Bool = false
    let streaming: Bool

    private var title: String {
        if chat.isGroup { return chat.title.isEmpty ? "Group" : chat.title }
        if chat.isSystem { return "Projects" }
        if chat.isPinned { return chat.title.isEmpty ? (project?.name ?? "Chat") : chat.title }
        return chat.title.isEmpty ? "New chat" : chat.title
    }

    var body: some View {
        HStack(spacing: 12) {
            if chat.isGroup {
                ZStack {
                    Circle().fill(theme.card).frame(width: 40, height: 40)
                    Image(systemName: "person.2.fill")
                        .font(.system(size: 15, weight: .medium))
                        .foregroundStyle(theme.muted)
                }
            } else if chat.isSystem {
                ZStack {
                    Circle().fill(theme.card).frame(width: 40, height: 40)
                    Image(systemName: "square.grid.2x2")
                        .font(.system(size: 15, weight: .medium))
                        .foregroundStyle(theme.muted)
                }
            } else if let project {
                AgentAvatar(project: project, theme: theme, size: 40, still: true)
            }
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(title)
                        .font(.system(size: Theme.body, weight: unread ? .semibold : .regular))
                        .foregroundStyle(theme.text)
                        .lineLimit(1)
                    if streaming { PulseDot(color: theme.warn) }
                    Spacer(minLength: 8)
                    if !time.isEmpty {
                        Text(time)
                            .font(.system(size: Theme.small))
                            .foregroundStyle(unread ? Color(hex: "#1084FE")! : theme.muted)
                    }
                }
                HStack(spacing: 8) {
                    Text(preview.isEmpty ? " " : preview)
                        .font(.system(size: Theme.small))
                        .foregroundStyle(theme.muted)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                    if unread {
                        Circle().fill(Color(hex: "#1084FE")!).frame(width: 9, height: 9)
                    }
                }
            }
        }
        .padding(.vertical, 4)
    }
}
