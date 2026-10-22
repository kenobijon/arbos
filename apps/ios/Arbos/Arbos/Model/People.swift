// People chats (D72 v1): DMs by GitHub handle, shared agents, project
// members. Shapes from internal/people-chats-api.md.

import Foundation

struct Person: Identifiable, Equatable, Hashable {
    var id: String
    var handle: String
    var avatarUrl: String?
    var name: String?
    /// member (signed in) | invited (DM opened, never signed in) | github (not on Arbos yet)
    var status: String
    /// §9 presence: last seen (ms, minute-floored; nil = never) and online (< 90 s).
    var lastSeenAt: Int64? = nil
    var online: Bool = false
    /// D79 `avatar_color_index_effective` from the relay (nil → derived from `id`).
    var avatarColorIndex: Int? = nil

    var displayName: String { name?.isEmpty == false ? name! : handle }

    static func parse(_ raw: Any) -> Person? {
        guard let d = raw as? [String: Any], let id = d["id"] as? String, let handle = d["handle"] as? String else { return nil }
        var person = Person(id: id, handle: handle, avatarUrl: d["avatar_url"] as? String, name: d["name"] as? String,
                            status: d["status"] as? String ?? "member",
                            lastSeenAt: (d["last_seen_at"] as? NSNumber)?.int64Value, online: (d["online"] as? NSNumber)?.boolValue ?? false)
        person.avatarColorIndex = (d["avatar_color_index_effective"] as? NSNumber)?.intValue
        return person
    }
}

struct ChatMember: Identifiable, Equatable {
    enum Kind: Equatable { case user, agent }
    var id: String
    var kind: Kind
    /// `user:<id>` or `agent:<project_id>` as the relay writes it.
    var member: String
    var role: String
    var handle: String
    var avatarUrl: String?
    var name: String?
    var status: String
    /// When this member was added to the chat (relay `joined_at`, unix ms).
    var joinedAt: Int64
    /// Agents only.
    var project: Project?
    var ownerId: String?
    var ownerHandle: String?
    var replyPolicy: String?
    /// §9 presence for people; agents carry `description` instead.
    var lastSeenAt: Int64? = nil
    var online: Bool = false
    var description: String? = nil
    /// D79 `avatar_color_index_effective` from the relay (nil → derived from `id`).
    var avatarColorIndex: Int? = nil

    var isAgent: Bool { kind == .agent }
    var displayName: String { kind == .agent ? (project?.name ?? handle) : (name?.isEmpty == false ? name! : handle) }

    static func parse(_ raw: Any) -> ChatMember? {
        guard let d = raw as? [String: Any], let member = d["member"] as? String else { return nil }
        let isAgent = member.hasPrefix("agent:")
        let id = String(member.split(separator: ":", maxSplits: 1).last ?? Substring(member))
        var project: Project? = nil
        if isAgent, let p = d["project"] as? [String: Any], let pid = p["id"] as? String {
            project = Project(id: pid, name: p["name"] as? String ?? "Agent", icon: p["icon"] as? String ?? "A",
                              color: p["color"] as? String ?? "#8b7cf6", state: .awake, stateSince: nil, stateDetail: nil,
                              model: "", effort: .low, spendTodayUsd: 0, spendCapUsd: 0,
                              avatar: (p["avatar"] as? String).flatMap { $0.isEmpty ? nil : $0 },
                              avatarColor: ((p["avatar_color"] as? String) ?? (d["avatar_color"] as? String)).flatMap { $0.isEmpty ? nil : $0 })
            project?.avatarColorIndex = (p["avatar_color_index"] as? NSNumber)?.intValue
            project?.avatarColorIndexEffective = (p["avatar_color_index_effective"] as? NSNumber)?.intValue
        }
        let settings = d["settings"] as? [String: Any]
        let joinedAt = (d["joined_at"] as? NSNumber)?.int64Value ?? (d["joined_at"] as? Int64) ?? 0
        var m = ChatMember(id: id, kind: isAgent ? .agent : .user, member: member, role: d["role"] as? String ?? "member",
                          handle: d["handle"] as? String ?? (isAgent ? "agent" : "someone"), avatarUrl: d["avatar_url"] as? String,
                          name: d["name"] as? String, status: d["status"] as? String ?? "member", joinedAt: joinedAt,
                          project: project, ownerId: d["owner"] as? String, ownerHandle: d["owner_handle"] as? String,
                          replyPolicy: settings?["reply_policy"] as? String)
        m.lastSeenAt = (d["last_seen_at"] as? NSNumber)?.int64Value
        m.online = (d["online"] as? NSNumber)?.boolValue ?? false
        m.avatarColorIndex = (d["avatar_color_index_effective"] as? NSNumber)?.intValue
        m.description = (d["description"] as? String) ?? ((d["project"] as? [String: Any])?["description"] as? String)
        return m
    }
}

struct PeopleChat: Identifiable, Equatable {
    var id: String
    var kind: String
    var createdBy: String
    var createdAt: Int64
    var title: String
    /// Blob key under `people:{id}` for an optional group picture.
    var imageKey: String? = nil
    var pending: Bool
    var members: [ChatMember]
    var headSeq: Int64
    var readSeq: Int64
    var unread: Bool
    var lockState: String
    var lastActivityAt: Int64
    var myRole: String
    var muted: Bool

    var humans: [ChatMember] { members.filter { !$0.isAgent } }
    var agents: [ChatMember] { members.filter { $0.isAgent } }

    /// Several humans, any agent, or `kind == group`: show a cluster, not a single person.
    /// Adding an agent to a DM turns the chat into a group for the header and profile.
    var isGroup: Bool { kind == "group" || humans.count > 2 || !agents.isEmpty }

    /// The other person of a DM.
    func counterpart(me: String?) -> ChatMember? {
        humans.first { $0.id != me } ?? humans.first
    }

    var lockLabel: String {
        lockState == "people_only" ? "People only" : "Visible to \(agents.map { "@\($0.ownerHandle ?? "owner")" }.joined(separator: ", "))'s agent"
    }

    /// Is this person already in the chat? Me (`meId`) counts as in. A handle
    /// match is enough: search results and roster rows need not share an id.
    func hasPerson(_ p: Person, meId: String?) -> Bool {
        if let meId, p.id == meId { return true }
        return humans.contains { $0.id == p.id || $0.handle.lowercased() == p.handle.lowercased() }
    }

    /// Is this agent (by project id) already in the chat?
    func hasAgent(_ projectId: String) -> Bool {
        agents.contains { $0.id == projectId || $0.project?.id == projectId }
    }

    static func parse(_ raw: Any) -> PeopleChat? {
        guard let d = raw as? [String: Any], let id = d["id"] as? String else { return nil }
        let members = ((d["members"] as? [Any]) ?? []).compactMap(ChatMember.parse)
        let me = d["me"] as? [String: Any]
        let n = { (k: String) -> Int64 in (d[k] as? NSNumber)?.int64Value ?? 0 }
        let imageKey = (d["image_key"] as? String).flatMap { $0.isEmpty ? nil : $0 }
        return PeopleChat(id: id, kind: d["kind"] as? String ?? "dm", createdBy: d["created_by"] as? String ?? "", createdAt: n("created_at"),
                          title: d["title"] as? String ?? "Chat", imageKey: imageKey, pending: d["pending"] as? Bool ?? false, members: members,
                          headSeq: n("head_seq"), readSeq: n("read_seq"), unread: d["unread"] as? Bool ?? false,
                          lockState: d["lock_state"] as? String ?? "people_only", lastActivityAt: n("last_activity_at"),
                          myRole: me?["role"] as? String ?? "member", muted: me?["muted"] as? Bool ?? false)
    }
}

/// The relay calls "Add" makes for one group: new people by handle, new agents
/// by project id, both on `chatId` — never a new group. Whoever is already in
/// (people, me, agents) is dropped so a stale pick never hits `already_member`.
struct AddMembersPlan: Equatable {
    var chatId: String
    var handles: [String]
    var agentIds: [String]

    static func make(chat: PeopleChat, meId: String?, picked: [Person], pickedAgents: [Project]) -> AddMembersPlan {
        AddMembersPlan(
            chatId: chat.id,
            handles: picked.filter { !chat.hasPerson($0, meId: meId) }.map(\.handle),
            agentIds: pickedAgents.map(\.id).filter { !chat.hasAgent($0) }
        )
    }

    var isEmpty: Bool { handles.isEmpty && agentIds.isEmpty }
}

struct ProjectMember: Identifiable, Equatable {
    var id: String
    var handle: String
    var avatarUrl: String?
    var name: String?
    var status: String
    var role: String
    var lastSeenAt: Int64? = nil
    var online: Bool = false

    var avatarColorIndex: Int? = nil

    static func parse(_ raw: Any) -> ProjectMember? {
        guard let d = raw as? [String: Any], let id = (d["user_id"] as? String) ?? (d["id"] as? String) else { return nil }
        var pm = ProjectMember(id: id, handle: d["handle"] as? String ?? "", avatarUrl: d["avatar_url"] as? String, name: d["name"] as? String,
                             status: d["status"] as? String ?? "member", role: d["role"] as? String ?? "member",
                             lastSeenAt: (d["last_seen_at"] as? NSNumber)?.int64Value, online: (d["online"] as? NSNumber)?.boolValue ?? false)
        pm.avatarColorIndex = (d["avatar_color_index_effective"] as? NSNumber)?.intValue
        return pm
    }
}

/// `GET /me/pushes` item.
struct PushItem: Identifiable, Equatable {
    var id: String
    var kind: String
    var payload: [String: String]
    /// `people_message` carries `sender {kind, id, name, handle, avatar, avatar_color, avatar_url}` (§7c).
    var sender: [String: String] = [:]
    /// The payload as received, for nested shapes (`call_started.call`).
    var raw: [String: Any] = [:]
    var createdAt: Int64

    static func == (a: PushItem, b: PushItem) -> Bool { a.id == b.id && a.kind == b.kind }

    static func parse(_ raw: Any) -> PushItem? {
        guard let d = raw as? [String: Any], let kind = d["kind"] as? String,
              let id = (d["id"] as? String) ?? (d["id"] as? NSNumber).map({ $0.stringValue }) else { return nil }
        var payload: [String: String] = [:]
        var sender: [String: String] = [:]
        let rawPayload = (d["payload"] as? [String: Any]) ?? [:]
        for (k, v) in rawPayload {
            if k == "sender", let s = v as? [String: Any] {
                for (sk, sv) in s { if let str = sv as? String { sender[sk] = str } }
            } else {
                payload[k] = "\(v)"
            }
        }
        return PushItem(id: id, kind: kind, payload: payload, sender: sender, raw: rawPayload, createdAt: (d["created_at"] as? NSNumber)?.int64Value ?? 0)
    }
}

/// People-chat room slot used in room names, blob paths and doc meta.
let peopleSlot = "people"

/// Mention slug for an agent member: project name lower-cased, spaces → "-".
func mentionSlug(_ name: String) -> String {
    name.lowercased().replacingOccurrences(of: " ", with: "-")
}
