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

    var displayName: String { name?.isEmpty == false ? name! : handle }

    static func parse(_ raw: Any) -> Person? {
        guard let d = raw as? [String: Any], let id = d["id"] as? String, let handle = d["handle"] as? String else { return nil }
        return Person(id: id, handle: handle, avatarUrl: d["avatar_url"] as? String, name: d["name"] as? String,
                      status: d["status"] as? String ?? "member")
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
    /// Agents only.
    var project: Project?
    var ownerId: String?
    var ownerHandle: String?
    var replyPolicy: String?

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
        }
        let settings = d["settings"] as? [String: Any]
        return ChatMember(id: id, kind: isAgent ? .agent : .user, member: member, role: d["role"] as? String ?? "member",
                          handle: d["handle"] as? String ?? (isAgent ? "agent" : "someone"), avatarUrl: d["avatar_url"] as? String,
                          name: d["name"] as? String, status: d["status"] as? String ?? "member", project: project,
                          ownerId: d["owner"] as? String, ownerHandle: d["owner_handle"] as? String,
                          replyPolicy: settings?["reply_policy"] as? String)
    }
}

struct PeopleChat: Identifiable, Equatable {
    var id: String
    var kind: String
    var createdBy: String
    var createdAt: Int64
    var title: String
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

    /// The other person of a DM.
    func counterpart(me: String?) -> ChatMember? {
        humans.first { $0.id != me } ?? humans.first
    }

    var lockLabel: String {
        lockState == "people_only" ? "People only" : "Visible to \(agents.map { "@\($0.ownerHandle ?? "owner")" }.joined(separator: ", "))'s agent"
    }

    static func parse(_ raw: Any) -> PeopleChat? {
        guard let d = raw as? [String: Any], let id = d["id"] as? String else { return nil }
        let members = ((d["members"] as? [Any]) ?? []).compactMap(ChatMember.parse)
        let me = d["me"] as? [String: Any]
        let n = { (k: String) -> Int64 in (d[k] as? NSNumber)?.int64Value ?? 0 }
        return PeopleChat(id: id, kind: d["kind"] as? String ?? "dm", createdBy: d["created_by"] as? String ?? "", createdAt: n("created_at"),
                          title: d["title"] as? String ?? "Chat", pending: d["pending"] as? Bool ?? false, members: members,
                          headSeq: n("head_seq"), readSeq: n("read_seq"), unread: d["unread"] as? Bool ?? false,
                          lockState: d["lock_state"] as? String ?? "people_only", lastActivityAt: n("last_activity_at"),
                          myRole: me?["role"] as? String ?? "member", muted: me?["muted"] as? Bool ?? false)
    }
}

struct ProjectMember: Identifiable, Equatable {
    var id: String
    var handle: String
    var avatarUrl: String?
    var name: String?
    var status: String
    var role: String

    static func parse(_ raw: Any) -> ProjectMember? {
        guard let d = raw as? [String: Any], let id = d["user_id"] as? String else { return nil }
        return ProjectMember(id: id, handle: d["handle"] as? String ?? "", avatarUrl: d["avatar_url"] as? String, name: d["name"] as? String,
                             status: d["status"] as? String ?? "member", role: d["role"] as? String ?? "member")
    }
}

/// `GET /me/pushes` item.
struct PushItem: Identifiable, Equatable {
    var id: String
    var kind: String
    var payload: [String: String]
    /// `people_message` carries `sender {kind, id, name, handle, avatar, avatar_color, avatar_url}` (§7c).
    var sender: [String: String] = [:]
    var createdAt: Int64

    static func parse(_ raw: Any) -> PushItem? {
        guard let d = raw as? [String: Any], let id = d["id"] as? String, let kind = d["kind"] as? String else { return nil }
        var payload: [String: String] = [:]
        var sender: [String: String] = [:]
        for (k, v) in (d["payload"] as? [String: Any]) ?? [:] {
            if k == "sender", let s = v as? [String: Any] {
                for (sk, sv) in s { if let str = sv as? String { sender[sk] = str } }
            } else {
                payload[k] = "\(v)"
            }
        }
        return PushItem(id: id, kind: kind, payload: payload, sender: sender, createdAt: (d["created_at"] as? NSNumber)?.int64Value ?? 0)
    }
}

/// People-chat room slot used in room names, blob paths and doc meta.
let peopleSlot = "people"

/// Mention slug for an agent member: project name lower-cased, spaces → "-".
func mentionSlug(_ name: String) -> String {
    name.lowercased().replacingOccurrences(of: " ", with: "-")
}
