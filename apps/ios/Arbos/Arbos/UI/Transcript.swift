// The message stream, Grok Bot style: assistant bubbles (#202020) left, user
// bubbles (#5C5C5C) right, centred muted system lines between them, file and
// voice-memo cards, one "<Project> is working" line while a turn runs.
// Prose only: tool calls, status noise, usage and JSON never render.

import AVFoundation
import SwiftUI
import UIKit

enum Block: Equatable, Hashable {
    case paragraph(String)
    case heading(level: Int, text: String)
    case bullets([String])
    case numbered([String])
    case code(lang: String, code: String)
    case table([String])
}

enum RowKind: Equatable {
    case user(text: String, pending: Bool)
    /// Another person's message in a people chat: left bubble with handle + avatar.
    case other(text: String, handle: String, avatar: String?)
    case assistant(blocks: [Block], live: Bool, caption: String?)
    /// Shared agent in a people chat: same shape as `.other` (avatar + handle + bubble),
    /// with an optional reply chip for the message it answers.
    case member(blocks: [Block], live: Bool, handle: String, avatar: String?, replyHandle: String?, replyPreview: String?)
    /// Centred muted line: hand-offs ("Messaged ◉ Affine"), stops, dates.
    case system(icon: String?, text: String, avatarProject: Id?)
    case question(text: String)
    case file(name: String, subtitle: String, projectId: Id, ref: String?)
    case voiceMemo(name: String, projectId: Id, ref: String?, fromUser: Bool)
    case image(projectId: Id, ref: String?, fromUser: Bool)
    case error(code: String, message: String)
}

struct Row: Identifiable, Equatable {
    var id: String
    var messageId: Id
    var role: Role
    var kind: RowKind
}

enum Transcript {
    /// An agent (or person) who joined a people chat — rendered as a centred "@handle joined" line.
    struct Join: Equatable {
        var id: String
        var handle: String
        var at: Int64
    }

    /// `me` = my `user:<id>` in a people chat; nil in project chats (every user row is mine).
    /// `joins` = people-chat membership events (agents added mid-chat) merged by time.
    static func rows(_ messages: [Message], projectId: Id, subagentNames: [String: String] = [:], me: String? = nil, people: Bool = false, joins: [Join] = []) -> [Row] {
        var rows: [Row] = []
        var lastAt: Int64 = 0
        var pendingJoins = joins.filter { $0.at > 0 && !$0.handle.isEmpty }.sorted { $0.at < $1.at }

        /// Emit join lines whose time is at or before `before` (or all remaining when `before` is nil).
        func flushJoins(before: Int64?) {
            while let j = pendingJoins.first {
                if let before, j.at > before { break }
                pendingJoins.removeFirst()
                if j.at > 0 && (lastAt == 0 || j.at - lastAt > 3_600_000) {
                    rows.append(Row(id: "join-\(j.id)#date", messageId: "join-\(j.id)", role: .system,
                                    kind: .system(icon: nil, text: dateLabel(j.at), avatarProject: nil)))
                }
                rows.append(Row(id: "join-\(j.id)", messageId: "join-\(j.id)", role: .system,
                                kind: .system(icon: nil, text: "@\(j.handle) joined", avatarProject: nil)))
                if j.at > 0 { lastAt = j.at }
            }
        }

        for message in messages {
            // Build this message's visible rows first. Date separators only land when
            // something actually renders — otherwise an empty/hidden-only message leaves
            // a lone "Today 5:45 PM" in a blank chat.
            var chunk: [Row] = []
            let streaming = message.isStreaming
            switch message.role {
            case .user:
                var hasImage = false
                for part in message.parts {
                    if case .attachment(let id, let a) = part {
                        if a.isImage { hasImage = true }
                        chunk.append(attachmentRow(id: "\(message.id)#\(id)", message: message, a: a, projectId: projectId, fromUser: true))
                    }
                }
                let text = message.plainText
                // Generic labels ("Image", "Photo", …) are not prose — the attachment
                // row already carries the media. Showing them as a bubble is noise.
                if !text.isEmpty, !(hasImage && Self.isGenericMediaLabel(text)) {
                    let mine = !people || me == nil || message.sender == me || message.sender == "user"
                    if mine {
                        chunk.append(Row(id: "\(message.id)#user", messageId: message.id, role: .user,
                                         kind: .user(text: text, pending: message.pending)))
                    } else {
                        chunk.append(Row(id: "\(message.id)#other", messageId: message.id, role: .project,
                                         kind: .other(text: text, handle: message.senderHandle ?? "someone", avatar: message.senderAvatar)))
                    }
                }
            case .system:
                // Call lines and other system notes: one muted centred line.
                let text = message.plainText.trimmingCharacters(in: .whitespacesAndNewlines)
                if !text.isEmpty {
                    let icon = text.hasPrefix("Call ended") ? "phone.down.fill" : (text.contains(" call") ? "phone.fill" : nil)
                    chunk.append(Row(id: "\(message.id)#system", messageId: message.id, role: .system,
                                     kind: .system(icon: icon, text: text, avatarProject: nil)))
                }
            case .assistant, .project:
                // Hand-off ack: Grok shows "Messaged ◉ <bot>" as a system line.
                if message.turn == .new, let name = message.subagentName ?? subagentNames[message.subagentId ?? ""] {
                    chunk.append(Row(id: "\(message.id)#handoff", messageId: message.id, role: .system,
                                     kind: .system(icon: nil, text: "Messaged \u{FFFC}\(name)", avatarProject: projectId)))
                }
                let lastTextPart = message.parts.lastIndex { $0.textValue != nil }
                let firstTextPart = message.parts.firstIndex { $0.textValue != nil }
                let parent = message.inReplyTo.flatMap { id in messages.first { $0.id == id } }
                let replyHandle = message.askedByHandle ?? parent?.senderHandle
                let replyPreview: String? = {
                    guard let parent else { return nil }
                    let t = Self.stripAskedPrefix(parent.plainText)
                        .trimmingCharacters(in: .whitespacesAndNewlines)
                    guard !t.isEmpty else { return nil }
                    if t.count <= 80 { return t }
                    return String(t.prefix(80)) + "…"
                }()
                for (pix, part) in message.parts.enumerated() {
                    switch part {
                    case .text(let id, let text):
                        let body = people ? Self.stripAskedPrefix(text) : text
                        let blocks = Markdown.blocks(body).filter { !Markdown.isJSON($0) }
                        if blocks.isEmpty { continue }
                        let live = streaming && pix == lastTextPart
                        if people, let h = message.senderHandle {
                            let showReply = pix == firstTextPart
                            chunk.append(Row(id: "\(message.id)#\(id)", messageId: message.id, role: .assistant,
                                             kind: .member(blocks: blocks, live: live, handle: h,
                                                           avatar: message.senderAvatar,
                                                           replyHandle: showReply ? replyHandle : nil,
                                                           replyPreview: showReply ? replyPreview : nil)))
                        } else {
                            chunk.append(Row(id: "\(message.id)#\(id)", messageId: message.id, role: .assistant,
                                             kind: .assistant(blocks: blocks, live: live, caption: nil)))
                        }
                    case .status(let id, .askUser, let text):
                        chunk.append(Row(id: "\(message.id)#\(id)", messageId: message.id, role: .assistant, kind: .question(text: text)))
                    case .status(let id, .handoff, let text):
                        chunk.append(Row(id: "\(message.id)#\(id)", messageId: message.id, role: .system,
                                         kind: .system(icon: nil, text: handoffText(text), avatarProject: nil)))
                    case .status(let id, .stopped, _):
                        chunk.append(Row(id: "\(message.id)#\(id)", messageId: message.id, role: .system,
                                         kind: .system(icon: "stop.circle", text: "Stopped", avatarProject: nil)))
                    case .attachment(let id, let a):
                        chunk.append(attachmentRow(id: "\(message.id)#\(id)", message: message, a: a, projectId: projectId, fromUser: false))
                    case .error(let id, let code, let text):
                        chunk.append(Row(id: "\(message.id)#\(id)", messageId: message.id, role: .assistant, kind: .error(code: code, message: text)))
                    case .status, .tool, .subagentResult, .reasoning, .unknown:
                        break
                    }
                }
                if message.status == .error && !message.hasErrorPart {
                    chunk.append(Row(id: "\(message.id)#failed", messageId: message.id, role: .assistant,
                                     kind: .error(code: "turn_failed", message: "The agent could not finish this turn.")))
                }
            }
            guard !chunk.isEmpty else { continue }
            // Joins that happened before this message land first (Grok: "@bob joined").
            if message.createdAt > 0 { flushJoins(before: message.createdAt) }
            // Date separators when the gap is over an hour (Grok: "Today 8:00 AM").
            if message.createdAt > 0 && (lastAt == 0 || message.createdAt - lastAt > 3_600_000) {
                rows.append(Row(id: "\(message.id)#date", messageId: message.id, role: .system,
                                kind: .system(icon: nil, text: dateLabel(message.createdAt), avatarProject: nil)))
            }
            if message.createdAt > 0 { lastAt = message.createdAt }
            rows.append(contentsOf: chunk)
        }
        // Agent added after the latest message (the usual "I just added Bob" case).
        flushJoins(before: nil)
        return rows
    }

    private static func attachmentRow(id: String, message: Message, a: Attachment, projectId: Id, fromUser: Bool) -> Row {
        if a.mime.lowercased().hasPrefix("audio/") || a.kind?.lowercased() == "audio" {
            return Row(id: id, messageId: message.id, role: fromUser ? .user : .assistant,
                       kind: .voiceMemo(name: a.name, projectId: projectId, ref: a.blobRef, fromUser: fromUser))
        }
        if a.isImage {
            return Row(id: id, messageId: message.id, role: fromUser ? .user : .assistant,
                       kind: .image(projectId: projectId, ref: a.blobRef, fromUser: fromUser))
        }
        return Row(id: id, messageId: message.id, role: fromUser ? .user : .assistant,
                   kind: .file(name: a.name, subtitle: subtitle(for: a), projectId: projectId, ref: a.blobRef))
    }

    /// UTType-style labels some clients leave as the only text on a media-only send.
    private static func isGenericMediaLabel(_ text: String) -> Bool {
        let t = text.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return t == "image" || t == "photo" || t == "video" || t == "attachment" || t == "(attachment)"
    }

    private static func subtitle(for a: Attachment) -> String {
        if let dot = a.name.lastIndex(of: ".") {
            return String(a.name[a.name.index(after: dot)...]).uppercased() + " file"
        }
        return a.mime
    }

    /// "Handed to subagent itsdangerous-tests" → "Messaged ◉ itsdangerous-tests".
    private static func handoffText(_ raw: String) -> String {
        let t = raw.trimmingCharacters(in: .whitespaces)
        for prefix in ["Handed to subagent ", "Handed to ", "Handed off to "] where t.hasPrefix(prefix) {
            return "Messaged \u{FFFC}" + t.dropFirst(prefix.count)
        }
        return t
    }

    /// Drop a legacy leading "@handle asked: …" line that older agentd builds wrote into prose.
    static func stripAskedPrefix(_ text: String) -> String {
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false)
        guard let first = lines.first,
              first.hasPrefix("@"),
              first.contains(" asked:") else { return text }
        var i = 1
        while i < lines.count && lines[i].isEmpty { i += 1 }
        return lines[i...].joined(separator: "\n")
    }

    static func dateLabel(_ ms: Int64) -> String {
        let date = Date(timeIntervalSince1970: Double(ms) / 1000)
        let cal = Calendar.current
        let time = date.formatted(date: .omitted, time: .shortened)
        if cal.isDateInToday(date) { return "Today \(time)" }
        if cal.isDateInYesterday(date) { return "Yesterday \(time)" }
        if let week = cal.date(byAdding: .day, value: -6, to: Date()), date > week {
            return date.formatted(.dateTime.weekday(.wide)) + " \(time)"
        }
        return date.formatted(date: .abbreviated, time: .shortened)
    }
}

/// Minimal block splitter: fences, headings, bullets, numbered lists, tables, paragraphs.
enum Markdown {
    static func blocks(_ text: String) -> [Block] {
        var out: [Block] = []
        var paragraph: [String] = []
        var bullets: [String] = []
        var numbered: [String] = []
        var table: [String] = []
        var code: [String]? = nil
        var lang = ""

        func flush() {
            if !paragraph.isEmpty { out.append(.paragraph(paragraph.joined(separator: " "))); paragraph = [] }
            if !bullets.isEmpty { out.append(.bullets(bullets)); bullets = [] }
            if !numbered.isEmpty { out.append(.numbered(numbered)); numbered = [] }
            if !table.isEmpty { out.append(.table(table)); table = [] }
        }

        for rawLine in text.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = String(rawLine)
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if let c = code {
                if trimmed.hasPrefix("```") {
                    out.append(.code(lang: lang, code: c.joined(separator: "\n")))
                    code = nil
                } else {
                    code!.append(line)
                }
                continue
            }
            if trimmed.hasPrefix("```") {
                flush()
                lang = String(trimmed.dropFirst(3)).trimmingCharacters(in: .whitespaces)
                code = []
                continue
            }
            if trimmed.isEmpty { flush(); continue }
            if trimmed.hasPrefix("#") {
                flush()
                let level = trimmed.prefix { $0 == "#" }.count
                out.append(.heading(level: min(level, 3), text: trimmed.drop { $0 == "#" }.trimmingCharacters(in: .whitespaces)))
                continue
            }
            if trimmed.hasPrefix("- ") || trimmed.hasPrefix("* ") || trimmed.hasPrefix("• ") {
                if !paragraph.isEmpty || !numbered.isEmpty || !table.isEmpty { flush() }
                bullets.append(String(trimmed.dropFirst(2)))
                continue
            }
            if let dot = trimmed.firstIndex(of: "."), dot > trimmed.startIndex, trimmed[trimmed.startIndex..<dot].allSatisfy(\.isNumber),
               trimmed.distance(from: trimmed.startIndex, to: dot) <= 3, trimmed[dot...].hasPrefix(". ") {
                if !paragraph.isEmpty || !bullets.isEmpty || !table.isEmpty { flush() }
                numbered.append(trimmed[trimmed.index(dot, offsetBy: 2)...].trimmingCharacters(in: .whitespaces))
                continue
            }
            if trimmed.hasPrefix("|") {
                if !paragraph.isEmpty || !bullets.isEmpty || !numbered.isEmpty { flush() }
                if !trimmed.replacingOccurrences(of: "|", with: "").replacingOccurrences(of: "-", with: "").replacingOccurrences(of: ":", with: "").trimmingCharacters(in: .whitespaces).isEmpty {
                    table.append(trimmed)
                }
                continue
            }
            if !bullets.isEmpty || !numbered.isEmpty || !table.isEmpty { flush() }
            paragraph.append(trimmed)
        }
        if let c = code { out.append(.code(lang: lang, code: c.joined(separator: "\n"))) }
        flush()
        return out
    }

    /// A block that is really machine output (a JSON object/array) is not prose.
    static func isJSON(_ block: Block) -> Bool {
        let text: String
        switch block {
        case .paragraph(let t): text = t
        case .code(let lang, let c) where lang.isEmpty || lang.lowercased() == "json": text = c
        default: return false
        }
        let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard t.hasPrefix("{") || t.hasPrefix("["), let data = t.data(using: .utf8) else { return false }
        return (try? JSONSerialization.jsonObject(with: data)) != nil
    }

    static func inline(_ text: String) -> AttributedString {
        (try? AttributedString(markdown: text, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)))
            ?? AttributedString(text)
    }
}

// MARK: - Row views

struct RowView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    let row: Row
    let chatId: Id
    let width: CGFloat

    private var maxBubble: CGFloat { max(200, width - Theme.bubbleInset) }

    var body: some View {
        switch row.kind {
        case .user(let text, let pending):
            HStack {
                Spacer(minLength: 0)
                Text(Markdown.inline(text))
                    .font(.system(size: Theme.body))
                    .lineSpacing(Theme.bodyLineSpacing)
                    .foregroundStyle(theme.userText)
                    .textSelection(.enabled)
                    .padding(.horizontal, Theme.bubbleHPad)
                    .padding(.vertical, Theme.bubbleVPad)
                    .background(RoundedRectangle(cornerRadius: Theme.bubbleRadius, style: .continuous).fill(theme.bubbleUser))
                    .frame(maxWidth: maxBubble, alignment: .trailing)
                    .opacity(pending ? 0.6 : 1)
            }
        case .other(let text, let handle, let avatar):
            HStack(alignment: .bottom, spacing: 8) {
                PersonAvatar(url: avatar, handle: handle, size: 26)
                VStack(alignment: .leading, spacing: 3) {
                    Text("@\(handle)")
                        .font(.system(size: Theme.tiny, weight: .medium))
                        .foregroundStyle(theme.muted)
                        .padding(.leading, 4)
                    Text(Markdown.inline(text))
                        .font(.system(size: Theme.body))
                        .lineSpacing(Theme.bodyLineSpacing)
                        .foregroundStyle(theme.text)
                        .textSelection(.enabled)
                        .padding(.horizontal, Theme.bubbleHPad)
                        .padding(.vertical, Theme.bubbleVPad)
                        .background(RoundedRectangle(cornerRadius: Theme.bubbleRadius, style: .continuous).fill(theme.bubbleAssistant))
                }
                .frame(maxWidth: maxBubble - 34, alignment: .leading)
                Spacer(minLength: 0)
            }
        case .member(let blocks, let live, let handle, let avatar, let replyHandle, let replyPreview):
            HStack(alignment: .bottom, spacing: 8) {
                peopleAvatar(handle: handle, avatar: avatar)
                VStack(alignment: .leading, spacing: 3) {
                    Text("@\(handle)")
                        .font(.system(size: Theme.tiny, weight: .medium))
                        .foregroundStyle(theme.muted)
                        .padding(.leading, 4)
                    VStack(alignment: .leading, spacing: 8) {
                        if let replyHandle {
                            replyChip(handle: replyHandle, preview: replyPreview)
                        }
                        ForEach(Array(blocks.enumerated()), id: \.offset) { i, block in
                            BlockView(block: block, live: live && i == blocks.count - 1)
                        }
                    }
                    .padding(.horizontal, Theme.bubbleHPad)
                    .padding(.vertical, Theme.bubbleVPad)
                    .background(RoundedRectangle(cornerRadius: Theme.bubbleRadius, style: .continuous).fill(theme.bubbleAssistant))
                }
                .frame(maxWidth: maxBubble - 34, alignment: .leading)
                Spacer(minLength: 0)
            }
        case .assistant(let blocks, let live, let caption):
            HStack {
                VStack(alignment: .leading, spacing: 8) {
                    if let caption {
                        Text(caption)
                            .font(.system(size: Theme.tiny, weight: .medium))
                            .foregroundStyle(theme.muted)
                    }
                    ForEach(Array(blocks.enumerated()), id: \.offset) { i, block in
                        BlockView(block: block, live: live && i == blocks.count - 1)
                    }
                }
                .padding(.horizontal, Theme.bubbleHPad)
                .padding(.vertical, Theme.bubbleVPad)
                .background(RoundedRectangle(cornerRadius: Theme.bubbleRadius, style: .continuous).fill(theme.bubbleAssistant))
                .frame(maxWidth: maxBubble, alignment: .leading)
                Spacer(minLength: 0)
            }
        case .system(let icon, let text, let avatarProject):
            HStack(spacing: 5) {
                if let icon {
                    Image(systemName: icon).font(.system(size: 13, weight: .regular))
                }
                systemText(text, avatarProject: avatarProject)
            }
            .font(.system(size: Theme.small))
            .foregroundStyle(theme.muted)
            .frame(maxWidth: .infinity)
            .multilineTextAlignment(.center)
        case .question(let text):
            HStack {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Question")
                        .font(.system(size: Theme.tiny, weight: .semibold))
                        .foregroundStyle(theme.muted)
                    Text(Markdown.inline(text))
                        .font(.system(size: Theme.body))
                        .foregroundStyle(theme.text)
                }
                .padding(.horizontal, Theme.bubbleHPad)
                .padding(.vertical, Theme.bubbleVPad + 2)
                .background(RoundedRectangle(cornerRadius: Theme.bubbleRadius, style: .continuous).fill(theme.bubbleAssistant))
                .overlay(RoundedRectangle(cornerRadius: Theme.bubbleRadius, style: .continuous).strokeBorder(theme.warn.opacity(0.5), lineWidth: 1))
                .frame(maxWidth: maxBubble, alignment: .leading)
                Spacer(minLength: 0)
            }
        case .file(let name, let subtitle, let projectId, let ref):
            HStack {
                if row.role == .user { Spacer(minLength: 0) }
                FileCard(name: name, subtitle: subtitle, projectId: projectId, blobRef: ref, fromUser: row.role == .user)
                    .frame(maxWidth: maxBubble, alignment: row.role == .user ? .trailing : .leading)
                if row.role != .user { Spacer(minLength: 0) }
            }
        case .voiceMemo(let name, let projectId, let ref, let fromUser):
            HStack {
                if fromUser { Spacer(minLength: 0) }
                VoiceMemoBubble(name: name, projectId: projectId, blobRef: ref, fromUser: fromUser)
                    .frame(maxWidth: maxBubble, alignment: fromUser ? .trailing : .leading)
                if !fromUser { Spacer(minLength: 0) }
            }
        case .image(let projectId, let ref, let fromUser):
            HStack {
                if fromUser { Spacer(minLength: 0) }
                ImageBubble(projectId: projectId, blobRef: ref, fromUser: fromUser, maxWidth: maxBubble)
                if !fromUser { Spacer(minLength: 0) }
            }
        case .error(let code, let message):
            HStack(spacing: 10) {
                Image(systemName: "exclamationmark.circle").foregroundStyle(theme.danger)
                Text(message).font(.system(size: Theme.small)).foregroundStyle(theme.text)
                Spacer(minLength: 0)
                Text(code).font(.system(size: 11)).foregroundStyle(theme.muted)
                Button("Retry") { model.retry(chatId: chatId, messageId: row.messageId) }
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(theme.text)
            }
            .padding(.horizontal, Theme.bubbleHPad).padding(.vertical, 10)
            .background(RoundedRectangle(cornerRadius: Theme.bubbleRadius, style: .continuous).fill(theme.dangerSoft))
        }
    }

    /// Avatar for a people-chat agent: project tree when we know it, else sender slug / initials.
    @ViewBuilder
    private func peopleAvatar(handle: String, avatar: String?) -> some View {
        if let project = model.projects.first(where: {
            mentionSlug($0.name).caseInsensitiveCompare(handle) == .orderedSame
        }) {
            AgentAvatar(project: project, theme: theme, size: 26, still: true)
        } else if let avatar, avatar.contains("://") {
            PersonAvatar(url: avatar, handle: handle, size: 26)
        } else if let avatar, !avatar.isEmpty, TreeAvatars.shared.has(avatar) {
            TreeCharacterView(slug: avatar, size: 26,
                              color: TreeAvatars.shared.defaultColor(forProjectId: handle), still: true)
        } else {
            PersonAvatar(url: avatar, handle: handle, size: 26)
        }
    }

    /// Compact reply reference above the prose (iMessage-style), not inline "@user asked:".
    private func replyChip(handle: String, preview: String?) -> some View {
        HStack(alignment: .top, spacing: 8) {
            RoundedRectangle(cornerRadius: 1, style: .continuous)
                .fill(theme.muted.opacity(0.55))
                .frame(width: 2)
            VStack(alignment: .leading, spacing: 2) {
                Text("@\(handle)")
                    .font(.system(size: Theme.tiny, weight: .semibold))
                    .foregroundStyle(theme.muted)
                if let preview, !preview.isEmpty {
                    Text(preview)
                        .font(.system(size: Theme.tiny))
                        .foregroundStyle(theme.muted)
                        .lineLimit(2)
                }
            }
            Spacer(minLength: 0)
        }
    }

    /// "Messaged \u{FFFC}Affine" draws a small avatar where the object marker is.
    @ViewBuilder
    private func systemText(_ text: String, avatarProject: Id?) -> some View {
        if let range = text.range(of: "\u{FFFC}") {
            let before = String(text[..<range.lowerBound])
            let after = String(text[range.upperBound...])
            let target = model.projects.first { $0.name.caseInsensitiveCompare(after) == .orderedSame }
            HStack(spacing: 5) {
                Text(before)
                if let target {
                    AgentAvatar(project: target, theme: theme, size: 16, still: true)
                } else if let slug = TreeAvatars.shared.relayDefault(after), TreeAvatars.shared.has(slug) {
                    // An agent we do not list yet still shows as a coloured tree (D74).
                    TreeCharacterView(slug: slug, size: 16, color: TreeAvatars.shared.defaultColor(forProjectId: after), still: true)
                } else {
                    BlobAvatar(color: theme.muted, shape: BlobShape.pick(for: after), size: 16)
                }
                Text(after)
            }
        } else {
            Text(text)
        }
    }
}

struct BlockView: View {
    @Environment(\.theme) private var theme
    let block: Block
    let live: Bool

    var body: some View {
        switch block {
        case .paragraph(let text):
            if live {
                StreamingText(text: text, font: .system(size: Theme.body), color: theme.text)
                    .lineSpacing(Theme.bodyLineSpacing)
            } else {
                Text(Markdown.inline(text))
                    .font(.system(size: Theme.body))
                    .lineSpacing(Theme.bodyLineSpacing)
                    .foregroundStyle(theme.text)
                    .textSelection(.enabled)
            }
        case .heading(_, let text):
            Text(Markdown.inline(text))
                .font(.system(size: Theme.body, weight: .semibold))
                .foregroundStyle(theme.text)
        case .bullets(let items), .numbered(let items):
            let numbered: Bool = { if case .numbered = block { return true } else { return false } }()
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(items.enumerated()), id: \.offset) { i, item in
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Text(numbered ? "\(i + 1)." : "•")
                            .font(.system(size: Theme.body))
                            .foregroundStyle(theme.text)
                            .frame(minWidth: 16, alignment: .trailing)
                        if live && i == items.count - 1 {
                            StreamingText(text: item, font: .system(size: Theme.body), color: theme.text)
                        } else {
                            Text(Markdown.inline(item))
                                .font(.system(size: Theme.body))
                                .foregroundStyle(theme.text)
                        }
                    }
                }
            }
        case .table(let lines):
            ScrollView(.horizontal, showsIndicators: false) {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(Array(lines.enumerated()), id: \.offset) { i, line in
                        Text(line.split(separator: "|").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }.joined(separator: "    "))
                            .font(.system(size: 14, weight: i == 0 ? .semibold : .regular, design: .monospaced))
                            .foregroundStyle(theme.text)
                    }
                }
                .padding(10)
            }
            .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(theme.bg.opacity(0.6)))
        case .code(_, let code):
            ScrollView(.horizontal, showsIndicators: false) {
                Text(code)
                    .font(.system(size: 14, design: .monospaced))
                    .foregroundStyle(theme.text)
                    .textSelection(.enabled)
                    .padding(10)
            }
            .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(theme.bg.opacity(0.6)))
        }
    }
}

/// Round avatar for a person (GitHub picture, initials fallback).
struct PersonAvatar: View {
    @Environment(\.theme) private var theme
    let url: String?
    let handle: String
    var size: CGFloat = 40

    var body: some View {
        ZStack {
            Circle().fill(Color(hex: "#2A2A2A")!)
            if let url, let u = URL(string: url) {
                AsyncImage(url: u) { img in img.resizable().scaledToFill() } placeholder: { initials }
            } else {
                initials
            }
        }
        .frame(width: size, height: size)
        .clipShape(Circle())
    }

    private var initials: some View {
        Text(String(handle.prefix(1)).uppercased())
            .font(.system(size: size * 0.42, weight: .semibold))
            .foregroundStyle(theme.text)
    }
}

/// Inline image attachment: loads with the device bearer (same as voice memos),
/// tappable for a full-screen look. AsyncImage alone cannot send `Authorization`
/// and often stays blank on people-chat blob URLs.
struct ImageBubble: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    let projectId: Id
    let blobRef: String?
    let fromUser: Bool
    let maxWidth: CGFloat

    @State private var image: UIImage?
    @State private var failed = false
    @State private var showFull = false

    private var width: CGFloat { min(240, maxWidth) }
    private var fill: Color { fromUser ? theme.bubbleUser : theme.bubbleAssistant }

    var body: some View {
        Button { if image != nil { showFull = true } } label: {
            ZStack {
                fill
                if let image {
                    Image(uiImage: image)
                        .resizable()
                        .scaledToFill()
                } else if failed || blobRef == nil {
                    Image(systemName: "photo")
                        .font(.system(size: 28))
                        .foregroundStyle(fromUser ? theme.userText.opacity(0.7) : theme.muted)
                } else {
                    ProgressView().tint(fromUser ? theme.userText.opacity(0.7) : theme.muted)
                }
            }
            .frame(width: width, height: 180)
            .clipShape(RoundedRectangle(cornerRadius: Theme.bubbleRadius, style: .continuous))
        }
        .buttonStyle(.plain)
        .disabled(image == nil)
        .accessibilityLabel(image == nil ? "Image" : "Image, tap to view")
        .task(id: blobRef) { await load() }
        .fullScreenCover(isPresented: $showFull) {
            if let image {
                ZStack(alignment: .topTrailing) {
                    Color.black.ignoresSafeArea()
                    Image(uiImage: image)
                        .resizable()
                        .scaledToFit()
                        .ignoresSafeArea()
                    Button { showFull = false } label: {
                        Image(systemName: "xmark")
                            .font(.system(size: 15, weight: .semibold))
                            .foregroundStyle(.white)
                            .frame(width: 36, height: 36)
                            .background(Circle().fill(Color.white.opacity(0.18)))
                    }
                    .buttonStyle(.plain)
                    .padding(16)
                }
            }
        }
    }

    private func load() async {
        guard let ref = blobRef,
              let url = model.blobURL(projectId: projectId, key: ref) else {
            failed = true
            return
        }
        var req = URLRequest(url: url)
        for (k, v) in model.authHeaders { req.setValue(v, forHTTPHeaderField: k) }
        do {
            let (data, response) = try await URLSession.shared.data(for: req)
            guard let http = response as? HTTPURLResponse, (200..<400).contains(http.statusCode),
                  let img = UIImage(data: data) else {
                failed = true
                return
            }
            image = img
            failed = false
        } catch {
            failed = true
        }
    }
}

/// Document card (Grok: icon, title, domain) for a non-audio attachment.
struct FileCard: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    let name: String
    let subtitle: String
    let projectId: Id
    let blobRef: String?
    let fromUser: Bool

    var body: some View {
        Button {
            if let ref = blobRef, let url = model.blobURL(projectId: projectId, key: ref) { UIApplication.shared.open(url) }
        } label: {
            HStack(spacing: 10) {
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(Color(hex: "#FFB000")!)
                    .frame(width: 22, height: 26)
                    .overlay(Image(systemName: "doc.text").font(.system(size: 12, weight: .bold)).foregroundStyle(.black))
                VStack(alignment: .leading, spacing: 2) {
                    Text(name).font(.system(size: Theme.body)).foregroundStyle(fromUser ? theme.userText : theme.text).lineLimit(1)
                    Text(subtitle).font(.system(size: Theme.small)).foregroundStyle(fromUser ? theme.userText.opacity(0.7) : theme.muted).lineLimit(1)
                }
            }
            .padding(.horizontal, Theme.bubbleHPad)
            .padding(.vertical, Theme.bubbleVPad)
            .background(RoundedRectangle(cornerRadius: Theme.bubbleRadius, style: .continuous).fill(fromUser ? theme.bubbleUser : theme.bubbleAssistant))
        }
        .buttonStyle(.plain)
    }
}

/// Voice memo bubble: play/pause, a static waveform, duration.
/// The whole chip is the control (waveform and timer included) — same idea as
/// `FileCard`. Audio is fetched with `URLSession` then played via
/// `AVAudioPlayer` so auth and the post-record `AVAudioSession` work;
/// streaming the blob through `AVPlayer` left the chip stuck on pause at `0:00`.
struct VoiceMemoBubble: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    let name: String
    let projectId: Id
    let blobRef: String?
    let fromUser: Bool
    @StateObject private var player = MemoPlayer()

    var body: some View {
        Button {
            guard let ref = blobRef, let url = model.blobURL(projectId: projectId, key: ref) else { return }
            player.toggle(url: url, headers: model.authHeaders)
        } label: {
            HStack(spacing: 10) {
                Circle().fill(theme.primary).frame(width: 32, height: 32)
                    .overlay(Image(systemName: player.playing ? "pause.fill" : "play.fill").font(.system(size: 13, weight: .bold)).foregroundStyle(theme.onPrimary).offset(x: player.playing ? 0 : 1))
                HStack(spacing: 2) {
                    ForEach(0..<22, id: \.self) { i in
                        let played: Bool = Double(i) / 22.0 < player.progress
                        let barHeight: CGFloat = 6 + CGFloat((i * 7) % 13)
                        Capsule()
                            .fill((fromUser ? theme.userText : theme.text).opacity(played ? 1 : 0.35))
                            .frame(width: 3, height: barHeight)
                    }
                }
                Text(player.durationLabel)
                    .font(.system(size: Theme.tiny, design: .monospaced))
                    .foregroundStyle(fromUser ? theme.userText.opacity(0.8) : theme.muted)
            }
            .padding(.horizontal, Theme.bubbleHPad)
            .padding(.vertical, Theme.bubbleVPad)
            .background(RoundedRectangle(cornerRadius: Theme.bubbleRadius, style: .continuous).fill(fromUser ? theme.bubbleUser : theme.bubbleAssistant))
        }
        .buttonStyle(.plain)
        .disabled(blobRef == nil)
        .onAppear {
            if let ref = blobRef, let url = model.blobURL(projectId: projectId, key: ref) {
                player.loadDuration(url: url, headers: model.authHeaders)
            }
        }
    }
}

@MainActor
final class MemoPlayer: ObservableObject {
    @Published var playing = false
    @Published var progress: Double = 0
    @Published var durationLabel = "0:00"
    private var audio: AVAudioPlayer?
    private var tick: Timer?
    private var totalSeconds: Double = 0
    private var localData: Data?
    private var fetchTask: Task<Void, Never>?

    /// Resolve the memo length as soon as the bubble appears (not only on play).
    /// Remote `AVURLAsset` often returns no duration for m4a until the whole file is
    /// buffered (moov at the end), so download then probe with `AVAudioPlayer`.
    func loadDuration(url: URL, headers: [String: String]) {
        guard totalSeconds <= 0, localData == nil else { return }
        fetchTask?.cancel()
        fetchTask = Task { _ = await ensureLocal(url: url, headers: headers) }
    }

    func toggle(url: URL, headers: [String: String]) {
        if let audio {
            if audio.isPlaying {
                audio.pause()
                playing = false
                stopTick()
                if totalSeconds > 0 { durationLabel = Self.format(totalSeconds) }
            } else {
                Self.activatePlayback()
                audio.play()
                playing = true
                startTick()
            }
            return
        }
        playing = true
        fetchTask?.cancel()
        fetchTask = Task {
            guard await ensureLocal(url: url, headers: headers) else {
                playing = false
                return
            }
            guard !Task.isCancelled else {
                playing = false
                return
            }
            startPlaying()
        }
    }

    /// Download the blob (Bearer / `?token=`) so duration and playback do not
    /// depend on `AVPlayer` streaming authenticated URLs.
    private func ensureLocal(url: URL, headers: [String: String]) async -> Bool {
        if localData != nil, totalSeconds > 0 { return true }
        do {
            var req = URLRequest(url: url)
            for (k, v) in headers { req.setValue(v, forHTTPHeaderField: k) }
            let (data, response) = try await URLSession.shared.data(for: req)
            guard !Task.isCancelled else { return false }
            guard let http = response as? HTTPURLResponse, (200..<400).contains(http.statusCode), !data.isEmpty else {
                return false
            }
            let probe = try AVAudioPlayer(data: data)
            let d = probe.duration
            guard d.isFinite, d > 0 else { return false }
            guard !Task.isCancelled else { return false }
            localData = data
            totalSeconds = d
            if !playing { durationLabel = Self.format(d) }
            return true
        } catch {
            return false
        }
    }

    private func startPlaying() {
        guard let localData else { playing = false; return }
        Self.activatePlayback()
        do {
            let p = try AVAudioPlayer(data: localData)
            p.prepareToPlay()
            audio = p
            totalSeconds = p.duration
            guard p.play() else { playing = false; return }
            playing = true
            startTick()
        } catch {
            playing = false
        }
    }

    private func startTick() {
        stopTick()
        tick = Timer.scheduledTimer(withTimeInterval: 0.25, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.updateProgress() }
        }
    }

    private func stopTick() {
        tick?.invalidate()
        tick = nil
    }

    private func updateProgress() {
        guard let audio else { return }
        let d = max(audio.duration, totalSeconds)
        guard d > 0 else { return }
        totalSeconds = d
        progress = min(1, audio.currentTime / d)
        if audio.isPlaying {
            let remaining = Int(max(0, d - audio.currentTime))
            durationLabel = String(format: "%d:%02d", remaining / 60, remaining % 60)
            return
        }
        // Natural end (user pause sets `playing = false` before stopping the tick).
        if playing {
            playing = false
            audio.currentTime = 0
            progress = 0
            durationLabel = Self.format(d)
            stopTick()
        }
    }

    /// Switch off the recorder's play-and-record session so the memo is audible.
    private static func activatePlayback() {
        let session = AVAudioSession.sharedInstance()
        try? session.setCategory(.playback, mode: .spokenAudio, options: [.duckOthers])
        try? session.setActive(true)
    }

    private static func format(_ seconds: Double) -> String {
        let s = Int(seconds.rounded())
        return String(format: "%d:%02d", s / 60, s % 60)
    }
}

/// Streaming fade (the "veil"): each appended chunk fades in by alpha only.
struct StreamingText: View {
    let text: String
    let font: Font
    let color: Color

    @State private var chunks: [(start: Int, end: Int, at: Date)] = []
    @State private var seen = 0
    @State private var lastArrival: Date?
    @State private var emaGapMs: Double = 120

    var body: some View {
        TimelineView(.animation(minimumInterval: 1.0 / 30.0, paused: chunks.isEmpty)) { context in
            render(now: context.date)
        }
        .onAppear { observe(text) }
        .onChange(of: text) { _, new in observe(new) }
    }

    private var fadeSeconds: Double { min(max(emaGapMs * 3, 120), 400) / 1000 }

    private func observe(_ new: String) {
        let len = new.utf8.count
        let now = Date()
        if len <= seen {
            seen = len
            chunks.removeAll { $0.start >= len }
            return
        }
        if let last = lastArrival {
            let gap = now.timeIntervalSince(last) * 1000
            emaGapMs = emaGapMs * 0.7 + gap * 0.3
        }
        lastArrival = now
        chunks.append((start: seen, end: len, at: now))
        seen = len
        let fade = fadeSeconds
        chunks.removeAll { now.timeIntervalSince($0.at) >= fade }
    }

    private func render(now: Date) -> Text {
        let fade = fadeSeconds
        let bytes = Array(text.utf8)
        var pieces = Text("")
        var cursor = 0
        let live = chunks.filter { now.timeIntervalSince($0.at) < fade && $0.start < bytes.count }
        for chunk in live.sorted(by: { $0.start < $1.start }) {
            let start = max(chunk.start, cursor)
            let end = min(chunk.end, bytes.count)
            if start > cursor {
                pieces = pieces + Text(string(bytes, cursor, start)).foregroundColor(color)
            }
            if end > start {
                let p = now.timeIntervalSince(chunk.at) / fade
                let alpha = 1 - pow(1 - p, 1.6)
                pieces = pieces + Text(string(bytes, start, end)).foregroundColor(color.opacity(alpha))
            }
            cursor = max(cursor, end)
        }
        if cursor < bytes.count {
            pieces = pieces + Text(string(bytes, cursor, bytes.count)).foregroundColor(color)
        }
        return pieces.font(font)
    }

    private func string(_ bytes: [UInt8], _ from: Int, _ to: Int) -> String {
        String(decoding: bytes[from..<to], as: UTF8.self)
    }
}
