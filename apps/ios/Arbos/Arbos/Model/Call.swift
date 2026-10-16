// Calls v1 (internal/calls-v1-contract.md): a LiveKit room per call, one
// active call per chat. `sender{}` (the same shape as everywhere) names
// who started it and who is in it; LiveKit identity is `u_<user_id>` and
// the participant metadata is the sender JSON.

import Foundation

enum CallKind: String, Codable {
    case voice, video
}

/// `sender{}`: `{kind, id, name, handle, avatar, avatar_color, avatar_url}`.
struct CallPerson: Identifiable, Equatable, Hashable {
    var id: String
    var kind: String
    var name: String?
    var handle: String
    var avatar: String?
    var avatarColor: String?
    var avatarUrl: String?

    var displayName: String { name?.isEmpty == false ? name! : "@\(handle)" }
    var isAgent: Bool { kind == "agent" }

    static func parse(_ raw: Any?) -> CallPerson? {
        guard let d = raw as? [String: Any], let id = d["id"] as? String else { return nil }
        func s(_ k: String) -> String? { (d[k] as? String).flatMap { $0.isEmpty ? nil : $0 } }
        return CallPerson(id: id, kind: s("kind") ?? "user", name: s("name"), handle: s("handle") ?? id,
                          avatar: s("avatar"), avatarColor: s("avatar_color"), avatarUrl: s("avatar_url"))
    }

    static func parse(metadata: String?) -> CallPerson? {
        guard let metadata, let data = metadata.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) else { return nil }
        return parse(obj)
    }

    /// From a LiveKit identity `u_<id>` when there is no metadata yet.
    static func placeholder(identity: String, name: String?) -> CallPerson {
        let id = identity.hasPrefix("u_") ? String(identity.dropFirst(2)) : identity
        return CallPerson(id: id, kind: identity.hasPrefix("a_") ? "agent" : "user", name: name, handle: name ?? id,
                          avatar: nil, avatarColor: nil, avatarUrl: nil)
    }
}

/// §8 `reachable[]` on a 201 start: can this callee be rung right now?
struct CallReachability: Equatable {
    var userId: String
    var handle: String
    var name: String?
    var anyLive: Bool
    var anyVoip: Bool

    var displayName: String { name?.isEmpty == false ? name! : "@\(handle)" }

    static func parse(_ raw: Any?) -> CallReachability? {
        guard let d = raw as? [String: Any], let id = d["user_id"] as? String else { return nil }
        return CallReachability(userId: id, handle: d["handle"] as? String ?? id, name: d["name"] as? String,
                                anyLive: (d["any_live"] as? NSNumber)?.boolValue ?? false, anyVoip: (d["any_voip"] as? NSNumber)?.boolValue ?? false)
    }
}

struct Call: Identifiable, Equatable {
    var callId: String
    var chatId: String
    /// `people` for DMs, else the shared project's id.
    var projectId: String
    var kind: CallKind
    var room: String
    var livekitUrl: String
    var token: String?
    var tokenExpiresAt: Int64?
    var startedBy: CallPerson?
    var startedAt: Int64
    var endedAt: Int64?
    var reason: String?
    var participants: [CallPerson]
    /// Only on the 201 start response.
    var reachable: [CallReachability] = []

    var id: String { callId }
    var isPeople: Bool { projectId == peopleSlot || projectId.isEmpty }
    var ended: Bool { endedAt != nil }

    static func parse(_ raw: Any?) -> Call? {
        guard let outer = raw as? [String: Any] else { return nil }
        let d = (outer["call"] as? [String: Any]) ?? outer
        guard let callId = (d["call_id"] as? String) ?? (d["id"] as? String), let chatId = d["chat_id"] as? String else { return nil }
        let n = { (k: String) -> Int64? in (d[k] as? NSNumber)?.int64Value }
        return Call(callId: callId, chatId: chatId, projectId: (d["project_id"] as? String) ?? peopleSlot,
                    kind: CallKind(rawValue: d["kind"] as? String ?? "voice") ?? .voice,
                    room: d["room"] as? String ?? "", livekitUrl: d["livekit_url"] as? String ?? "",
                    token: (d["token"] as? String).flatMap { $0.isEmpty ? nil : $0 }, tokenExpiresAt: n("token_expires_at"),
                    startedBy: CallPerson.parse(d["started_by"]), startedAt: n("started_at") ?? nowMs(), endedAt: n("ended_at"),
                    reason: d["reason"] as? String,
                    participants: ((d["participants"] as? [Any]) ?? []).compactMap(CallPerson.parse),
                    reachable: ((outer["reachable"] as? [Any]) ?? (d["reachable"] as? [Any]) ?? []).compactMap(CallReachability.parse))
    }
}

/// "Call ended · 12 min" / "Call ended".
func callEndedLine(durationS: Int) -> String {
    durationS < 60 ? "Call ended" : "Call ended · \(durationS / 60) min"
}
