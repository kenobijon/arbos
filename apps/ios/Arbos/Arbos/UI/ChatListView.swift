// Threads of one project ("Chats" in the … menu): the pinned main chat first,
// then the others, Grok list styling. Swipe to close a non-pinned chat.

import SwiftUI

struct ChatListView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    let projectId: Id
    @State private var creating = false
    @State private var search = ""

    private var project: Project? { model.project(projectId) }

    var body: some View {
        NavigationStack {
            List {
                if !search.trimmingCharacters(in: .whitespaces).isEmpty {
                    let hits = model.mergedChatHits(search).filter { $0.projectId == projectId }
                    if let note = model.chatSearchNote, hits.isEmpty || note == "Waking…" {
                        Text(note)
                            .font(.system(size: Theme.small))
                            .foregroundStyle(theme.muted)
                            .listRowBackground(theme.bg)
                    }
                    ForEach(hits) { hit in
                        Button {
                            model.openHit(hit)
                            dismiss()
                        } label: {
                            ChatHitRow(hit: hit)
                        }
                        .listRowBackground(theme.bg)
                    }
                }
                ForEach(model.chats(of: projectId)) { chat in
                    Button {
                        model.open(chat: chat)
                        dismiss()
                    } label: {
                        ChatRow(chat: chat, project: project, preview: preview(of: chat), streaming: model.isStreaming(chat.id))
                    }
                    .listRowBackground(theme.bg)
                    .listRowSeparatorTint(theme.separator)
                    .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                        if !chat.isPinned {
                            Button(role: .destructive) { model.closeChat(chat) } label: {
                                Label("Close", systemImage: "xmark")
                            }
                        }
                    }
                }
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .background(theme.bg)
            .searchable(text: $search, prompt: "Search messages")
            .onChange(of: search) { _, q in model.searchChats(q, only: projectId) }
            .navigationTitle(project?.name ?? "Chats")
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
                        if creating { ProgressView() } else { Image(systemName: "square.and.pencil") }
                    }
                }
            }
        }
        .presentationDetents([.medium, .large])
    }

    private func preview(of chat: Chat) -> String {
        let msgs = model.messages(of: chat.id)
        guard let last = msgs.last(where: { !$0.plainText.isEmpty }) else {
            return msgs.isEmpty ? "" : "…"
        }
        let line = last.plainText.split(separator: "\n").first.map(String.init) ?? ""
        return (last.role == .user ? "You: " : "") + HomeView.plain(line)
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
    let streaming: Bool

    var body: some View {
        HStack(spacing: 12) {
            if let project {
                AgentAvatar(project: project, theme: theme, size: 40, still: true)
                    .opacity(chat.isPinned ? 1 : 0.55)
            }
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(chat.isPinned ? "Main" : chat.title)
                        .font(.system(size: Theme.body, weight: chat.unread ? .semibold : .regular))
                        .foregroundStyle(theme.text)
                        .lineLimit(1)
                    if streaming { PulseDot(color: theme.warn) }
                }
                if !preview.isEmpty {
                    Text(preview)
                        .font(.system(size: Theme.small))
                        .foregroundStyle(theme.muted)
                        .lineLimit(1)
                }
            }
            Spacer()
            if chat.unread {
                Circle().fill(Color(hex: "#1084FE")!).frame(width: 8, height: 8)
            }
        }
        .padding(.vertical, 4)
    }
}
