// Chat doc on Loro, shaped as crdt-sync §6.2 (same schema `arbos-core::doc`
// writes on the box side):
//
//   meta:      Map { chatId, projectId, schemaVersion, epoch }
//   messages:  List<Map { id, role, ..., parts: List<Map { kind, text: Text, ... }> }>
//   commands:  List<Map { id, kind, payload, issuedBy, issuedAt, status }>
//
// Reading goes through `getDeepValue` → JSON → `Message`, so the schema stays
// additive. The same wrapper holds the project doc (`settings`, `status`,
// `threads`, `memory`).

import Foundation
import Loro

final class ChatDoc {
    let key: Id
    let doc: LoroDoc
    private var subscriptions: [Subscription] = []

    init(key: Id) {
        self.key = key
        self.doc = LoroDoc()
    }

    var messages: LoroList { doc.getList(id: "messages") }
    var commands: LoroList { doc.getList(id: "commands") }

    var count: Int { Int(messages.len()) }

    func commit() { doc.commit() }

    func importUpdate(_ bytes: Data) throws {
        _ = try doc.import(bytes: bytes)
    }

    /// Does the replica already contain this checkpoint frontier?
    func contains(frontier: Data) -> Bool {
        guard !frontier.isEmpty, let vv = try? VersionVector.decode(bytes: frontier) else { return false }
        return doc.oplogVv().includesVv(other: vv)
    }

    /// Bytes for the on-device store. A replica loaded from a shallow relay
    /// checkpoint stays shallow; otherwise this is the full snapshot.
    func exportSnapshot() throws -> Data {
        try doc.export(mode: .snapshot)
    }

    /// Nothing imported or written yet (a fresh replica is not worth saving).
    var isEmpty: Bool { doc.oplogFrontiers().isEmpty() }

    // MARK: Reading

    func readMessage(_ ix: Int) -> Message? {
        guard ix >= 0, ix < count, let map = messages.get(index: UInt32(ix))?.asLoroMap() else { return nil }
        return DocJSON.message(LoroJSON.any(map.getDeepValue()), chatId: key)
    }

    func readAll() -> [Message] {
        (0..<count).compactMap(readMessage)
    }

    func mapValue(_ name: String) -> [String: Any] {
        LoroJSON.any(doc.getMap(id: name).getDeepValue()) as? [String: Any] ?? [:]
    }

    // MARK: Writing (client side only: user rows + commands, §4.3)

    /// Append a user message as nested containers. Does not commit.
    struct Author {
        var sender: String
        var handle: String?
        var name: String?
        var avatar: String?
    }

    func insertUserMessage(id: Id, text: String, deviceId: String, attachments: [Attachment] = [],
                           author: Author? = nil, mentions: [String] = [], replyTo: String? = nil) throws {
        let list = messages
        let map = try list.insertContainer(pos: list.len(), child: LoroMap())
        try map.insert(key: "id", v: id)
        try map.insert(key: "chatId", v: key)
        try map.insert(key: "role", v: "user")
        try map.insert(key: "sender", v: author?.sender ?? "user")
        if let author {
            if let h = author.handle { try map.insert(key: "senderHandle", v: h) }
            if let n = author.name { try map.insert(key: "senderName", v: n) }
            if let a = author.avatar { try map.insert(key: "senderAvatar", v: a) }
        }
        if !mentions.isEmpty {
            let ml = try map.insertContainer(key: "mentions", child: LoroList())
            for m in mentions { try ml.push(v: m) }
        }
        if let replyTo { try map.insert(key: "replyTo", v: replyTo) }
        try map.insert(key: "status", v: "done")
        try map.insert(key: "deviceId", v: deviceId)
        try map.insert(key: "createdAt", v: nowMs())
        try map.insert(key: "waitingUser", v: false)
        try map.insert(key: "pending", v: false)
        for k in ["turn", "decision", "subagentId", "subagentName", "inReplyTo", "usage", "returnContract", "model", "effort", "completedAt"] {
            try map.insert(key: k, v: LoroValue.null)
        }
        let parts = try map.insertContainer(key: "parts", child: LoroList())
        for a in attachments {
            let part = try parts.insertContainer(pos: parts.len(), child: LoroMap())
            try part.insert(key: "id", v: newId())
            try part.insert(key: "kind", v: "attachment")
            let att = try part.insertContainer(key: "attachment", child: LoroMap())
            try att.insert(key: "name", v: a.name)
            try att.insert(key: "mime", v: a.mime)
            if let kind = a.kind { try att.insert(key: "kind", v: kind) }
            if let ref = a.blobRef { try att.insert(key: "ref", v: ref) } else { try att.insert(key: "ref", v: LoroValue.null) }
        }
        let part = try parts.insertContainer(pos: parts.len(), child: LoroMap())
        try part.insert(key: "id", v: newId())
        try part.insert(key: "kind", v: "text")
        let textC = try part.insertContainer(key: "text", child: LoroText())
        try textC.pushStr(s: text)
        _ = try map.insertContainer(key: "hiddenParts", child: LoroList())
    }

    /// A system line ("<name> started a video call", "Call ended · 12 min"):
    /// role `system`, one text part, `callId` so clients can fold it. Does not commit.
    func insertSystemLine(id: Id, text: String, deviceId: String, callId: String? = nil) throws {
        let list = messages
        let map = try list.insertContainer(pos: list.len(), child: LoroMap())
        try map.insert(key: "id", v: id)
        try map.insert(key: "chatId", v: key)
        try map.insert(key: "role", v: "system")
        try map.insert(key: "sender", v: "system")
        try map.insert(key: "status", v: "done")
        try map.insert(key: "deviceId", v: deviceId)
        try map.insert(key: "createdAt", v: nowMs())
        try map.insert(key: "completedAt", v: nowMs())
        try map.insert(key: "waitingUser", v: false)
        try map.insert(key: "pending", v: false)
        if let callId {
            try map.insert(key: "callId", v: callId)
            try map.insert(key: "call_id", v: callId)
        }
        for k in ["turn", "decision", "subagentId", "subagentName", "inReplyTo", "usage", "returnContract", "model", "effort"] {
            try map.insert(key: k, v: LoroValue.null)
        }
        let parts = try map.insertContainer(key: "parts", child: LoroList())
        let part = try parts.insertContainer(pos: parts.len(), child: LoroMap())
        try part.insert(key: "id", v: newId())
        try part.insert(key: "kind", v: "text")
        let textC = try part.insertContainer(key: "text", child: LoroText())
        try textC.pushStr(s: text)
        _ = try map.insertContainer(key: "hiddenParts", child: LoroList())
    }

    /// D82: the agent's first line in a brand-new project's chat, written by the device
    /// that created the project. Role `assistant`, sender `agent:<project>`, and
    /// `greeting: true` so memory and the transcript never read it as the user's words.
    /// Does not commit.
    func insertAssistantGreeting(id: Id, text: String, deviceId: String, projectId: Id, agentName: String) throws {
        let list = messages
        let map = try list.insertContainer(pos: list.len(), child: LoroMap())
        try map.insert(key: "id", v: id)
        try map.insert(key: "chatId", v: key)
        try map.insert(key: "role", v: "assistant")
        try map.insert(key: "sender", v: "agent:\(projectId)")
        try map.insert(key: "senderName", v: agentName)
        try map.insert(key: "greeting", v: true)
        try map.insert(key: "status", v: "done")
        try map.insert(key: "deviceId", v: deviceId)
        try map.insert(key: "createdAt", v: nowMs())
        try map.insert(key: "completedAt", v: nowMs())
        try map.insert(key: "waitingUser", v: false)
        try map.insert(key: "pending", v: false)
        for k in ["turn", "decision", "subagentId", "subagentName", "inReplyTo", "usage", "returnContract", "model", "effort"] {
            try map.insert(key: k, v: LoroValue.null)
        }
        let parts = try map.insertContainer(key: "parts", child: LoroList())
        let part = try parts.insertContainer(pos: parts.len(), child: LoroMap())
        try part.insert(key: "id", v: newId())
        try part.insert(key: "kind", v: "text")
        let textC = try part.insertContainer(key: "text", child: LoroText())
        try textC.pushStr(s: text)
        _ = try map.insertContainer(key: "hiddenParts", child: LoroList())
    }

    /// Find a message map by id (client writes reactions onto existing rows).
    func findMessage(_ id: Id) -> LoroMap? {
        let list = messages
        let n = Int(list.len())
        guard n > 0 else { return nil }
        for i in 0..<n {
            guard let map = list.get(index: UInt32(i))?.asLoroMap() else { continue }
            if case .string(let s) = map.get(key: "id")?.asValue(), s == id {
                return map
            }
        }
        return nil
    }

    /// Toggle one emoji reaction for `sender` on a message. Several emojis may
    /// sit on the same message. CRDT shape: flat `"emoji|sender" → true` under
    /// `reactions` (matches arbos-core; survives concurrent same-emoji first-react).
    /// Un-react also clears a legacy nested emoji-bucket entry when present.
    /// Turning a reaction off is a no-op when the `reactions` map is missing
    /// (does not create containers). Does not commit.
    func toggleReaction(messageId: Id, emoji: String, sender: String) throws {
        guard !emoji.isEmpty, !sender.isEmpty, let map = findMessage(messageId) else { return }
        let key = "\(emoji)|\(sender)"
        let existing = map.get(key: "reactions")?.asLoroMap()
        var isOn = false
        if let reactions = existing {
            if case .bool(true) = reactions.get(key: key)?.asValue() {
                isOn = true
            } else if let bucket = reactions.get(key: emoji)?.asLoroMap(),
                      case .bool(true) = bucket.get(key: sender)?.asValue() {
                isOn = true
            }
        }
        if isOn {
            // Match core `set_reaction(..., on=false)`: no-op without a map.
            guard let reactions = existing else { return }
            try reactions.delete(key: key)
            if let bucket = reactions.get(key: emoji)?.asLoroMap() {
                try bucket.delete(key: sender)
            }
        } else {
            let reactions = try map.getOrCreateContainer(key: "reactions", child: LoroMap())
            try reactions.insert(key: key, v: true)
        }
    }

    /// Append a `commands` row (`send`, `stop`, `answer`, `retry`, `retitle`). Does not commit.
    func pushCommand(_ kind: String, payload: [String: Any], device: String) throws {
        let list = commands
        let map = try list.insertContainer(pos: list.len(), child: LoroMap())
        try map.insert(key: "id", v: newId())
        try map.insert(key: "kind", v: kind)
        let p = try map.insertContainer(key: "payload", child: LoroMap())
        for (k, v) in payload {
            try LoroJSON.insert(into: p, key: k, value: v)
        }
        try map.insert(key: "issuedBy", v: device)
        try map.insert(key: "issuedAt", v: nowMs())
        try map.insert(key: "status", v: "pending")
    }

    /// Mirror of the model picker into the project doc `settings` (crdt-sync §6.3).
    func setModelSettings(model: String, effort: Effort) throws {
        let settings = doc.getMap(id: "settings")
        try settings.insert(key: "agentModel", v: model)
        try settings.insert(key: "agentEffort", v: effort.rawValue)
        let models: [String: LoroValue] = [
            "talking_model": .map(value: ["id": .string(value: model), "effort": .string(value: effort.rawValue)]),
        ]
        try settings.insert(key: "models", v: LoroValue.map(value: models))
    }

    // MARK: Subscriptions

    func onChange(_ handler: @escaping (Touched) -> Void) {
        let sub = doc.subscribeRoot { event in
            handler(ChatDoc.touched(event))
        }
        subscriptions.append(sub)
    }

    func onLocalUpdate(_ handler: @escaping (Data) -> Void) {
        let sub = doc.subscribeLocalUpdate(callback: handler)
        subscriptions.append(sub)
    }

    /// Map a root diff to message indexes. Paths look like
    /// `[(root, key "messages"), (list, seq i), (map, key "parts"), ...]`.
    static func touched(_ event: DiffEvent) -> Touched {
        var out = Touched()
        for diff in event.events {
            var sawMessages = false
            var found: Int? = nil
            var broke = false
            for item in diff.path {
                switch item.index {
                case .key(let k):
                    if !sawMessages {
                        if k == "messages" {
                            sawMessages = true
                        } else {
                            broke = true
                        }
                    }
                case .seq(let i):
                    if sawMessages {
                        found = Int(i)
                    }
                case .node:
                    break
                }
                if broke || found != nil { break }
            }
            if !sawMessages {
                // A diff whose target is the root `messages` list itself has an empty path
                // only when it is the root container: check the target id.
                if case .root(let name, _) = diff.target, name == "messages" {
                    out.listChanged = true
                } else {
                    out.other = true
                }
                continue
            }
            if let i = found {
                out.messages.insert(i)
            } else {
                out.listChanged = true
            }
        }
        return out
    }
}

/// LoroValue ↔ Foundation JSON.
enum LoroJSON {
    static func any(_ v: LoroValue) -> Any {
        switch v {
        case .null: return NSNull()
        case .bool(let b): return b
        case .double(let d): return d
        case .i64(let i): return NSNumber(value: i)
        case .binary(let d): return d.base64EncodedString()
        case .string(let s): return s
        case .list(let items): return items.map(any)
        case .map(let m): return m.mapValues(any)
        case .container: return NSNull()
        }
    }

    /// Write a JSON-ish value under `key`. Objects become `LoroMap`, arrays `LoroList`.
    static func insert(into map: LoroMap, key: String, value: Any) throws {
        switch value {
        case is NSNull:
            try map.insert(key: key, v: LoroValue.null)
        case let b as Bool:
            try map.insert(key: key, v: b)
        case let n as NSNumber:
            if CFNumberIsFloatType(n) {
                try map.insert(key: key, v: n.doubleValue)
            } else {
                try map.insert(key: key, v: n.int64Value)
            }
        case let s as String:
            try map.insert(key: key, v: s)
        case let arr as [Any]:
            let list = try map.insertContainer(key: key, child: LoroList())
            for (i, item) in arr.enumerated() {
                try insert(into: list, pos: UInt32(i), value: item)
            }
        case let obj as [String: Any]:
            let child = try map.insertContainer(key: key, child: LoroMap())
            for (k, v) in obj {
                try insert(into: child, key: k, value: v)
            }
        default:
            try map.insert(key: key, v: String(describing: value))
        }
    }

    static func insert(into list: LoroList, pos: UInt32, value: Any) throws {
        switch value {
        case is NSNull:
            try list.insert(pos: pos, v: LoroValue.null)
        case let b as Bool:
            try list.insert(pos: pos, v: b)
        case let n as NSNumber:
            if CFNumberIsFloatType(n) {
                try list.insert(pos: pos, v: n.doubleValue)
            } else {
                try list.insert(pos: pos, v: n.int64Value)
            }
        case let s as String:
            try list.insert(pos: pos, v: s)
        case let arr as [Any]:
            let child = try list.insertContainer(pos: pos, child: LoroList())
            for (i, item) in arr.enumerated() {
                try insert(into: child, pos: UInt32(i), value: item)
            }
        case let obj as [String: Any]:
            let child = try list.insertContainer(pos: pos, child: LoroMap())
            for (k, v) in obj {
                try insert(into: child, key: k, value: v)
            }
        default:
            try list.insert(pos: pos, v: String(describing: value))
        }
    }
}
