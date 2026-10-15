// HTTP side of `arbos-relay` (crdt-sync §9): pairing, registry, chats,
// models, wake. One bearer token per call, read from the credentials at
// call time so a re-pair takes effect at once.

import Foundation

struct RelayError: Error, LocalizedError {
    var status: Int
    var message: String
    /// `error.type` from the self-serve relay (token_expired, credits_exhausted, …).
    var type: String? = nil

    var errorDescription: String? {
        message.isEmpty ? "HTTP \(status)" : "HTTP \(status): \(message)"
    }

    var isUnauthorized: Bool { status == 401 }
}

/// Accepts `http://`, `https://`, `ws://` or `wss://`; derives the other.
struct RelayEndpoint: Equatable {
    var httpUrl: String
    var wsUrl: String

    init(_ raw: String) {
        var url = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        while url.hasSuffix("/") { url.removeLast() }
        if url.isEmpty {
            httpUrl = ""
            wsUrl = ""
        } else if url.hasPrefix("ws://") {
            httpUrl = "http://" + url.dropFirst(5)
            wsUrl = url
        } else if url.hasPrefix("wss://") {
            httpUrl = "https://" + url.dropFirst(6)
            wsUrl = url
        } else if url.hasPrefix("https://") {
            httpUrl = url
            wsUrl = "wss://" + url.dropFirst(8)
        } else if url.hasPrefix("http://") {
            httpUrl = url
            wsUrl = "ws://" + url.dropFirst(7)
        } else {
            httpUrl = "https://" + url
            wsUrl = "wss://" + url
        }
    }
}

struct RegistryProject {
    var project: Project
    var chats: [Chat]
}

final class RelayAPI {
    let endpoint: RelayEndpoint
    var token: () -> String
    /// Called once on a 401 for an authenticated call; return true after a
    /// successful token refresh and the call is retried with the new token.
    var refresher: (() async -> Bool)?

    private let session: URLSession

    init(endpoint: RelayEndpoint, token: @escaping () -> String) {
        self.endpoint = endpoint
        self.token = token
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = 30
        config.waitsForConnectivity = false
        session = URLSession(configuration: config)
    }

    // MARK: Raw calls

    func request(_ method: String, _ path: String, body: Any? = nil, auth: Bool = true) async throws -> (Data, HTTPURLResponse) {
        do {
            return try await requestOnce(method, path, body: body, auth: auth)
        } catch let error as RelayError where error.isUnauthorized && auth {
            if let refresher, await refresher() {
                return try await requestOnce(method, path, body: body, auth: auth)
            }
            throw error
        }
    }

    private func requestOnce(_ method: String, _ path: String, body: Any?, auth: Bool) async throws -> (Data, HTTPURLResponse) {
        guard let url = URL(string: endpoint.httpUrl + path) else {
            throw RelayError(status: 0, message: "Bad relay URL")
        }
        var req = URLRequest(url: url)
        req.httpMethod = method
        if auth {
            req.setValue("Bearer \(token())", forHTTPHeaderField: "Authorization")
        }
        if let body {
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
            req.httpBody = try JSONSerialization.data(withJSONObject: body)
        }
        let (data, response) = try await session.data(for: req)
        guard let http = response as? HTTPURLResponse else {
            throw RelayError(status: 0, message: "No HTTP response")
        }
        if http.statusCode >= 400 {
            let text = String(data: data, encoding: .utf8) ?? ""
            // Self-serve relay errors: {"error":{"type","message"}}.
            if let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
               let err = obj["error"] as? [String: Any] {
                throw RelayError(status: http.statusCode, message: err["message"] as? String ?? text, type: err["type"] as? String)
            }
            throw RelayError(status: http.statusCode, message: text.trimmingCharacters(in: .whitespacesAndNewlines))
        }
        return (data, http)
    }

    func json(_ method: String, _ path: String, body: Any? = nil, auth: Bool = true) async throws -> Any {
        let (data, _) = try await request(method, path, body: body, auth: auth)
        if data.isEmpty { return NSNull() }
        return (try? JSONSerialization.jsonObject(with: data)) ?? NSNull()
    }

    // MARK: Pairing (D35)

    func pair(code: String, name: String) async throws -> DeviceCredentials {
        let v = try await json("POST", "/pair", body: ["code": code.trimmingCharacters(in: .whitespaces), "name": name], auth: false)
        guard let m = v as? [String: Any], let tok = m["device_token"] as? String, !tok.isEmpty else {
            throw RelayError(status: 0, message: "The relay returned no token")
        }
        return DeviceCredentials(
            relayUrl: endpoint.httpUrl,
            deviceId: m["device_id"] as? String ?? "device",
            deviceToken: tok
        )
    }

    // MARK: Registry

    /// `GET /projects` then `GET /projects/{id}` for chats, models and spend.
    func registry() async throws -> [RegistryProject] {
        guard let list = try await json("GET", "/projects") as? [Any] else { return [] }
        var out: [RegistryProject] = []
        for item in list {
            guard let item = item as? [String: Any], let id = item["id"] as? String else { continue }
            // A project the list shows but this user may not open (403) is skipped, not fatal.
            let detail: [String: Any]
            do {
                detail = (try await json("GET", "/projects/\(id)")) as? [String: Any] ?? [:]
            } catch let e as RelayError where e.status == 403 || e.status == 404 {
                continue
            }
            let p = detail["project"] as? [String: Any] ?? item
            let models = p["models"] as? [String: Any] ?? [:]
            let talking = models["talking_model"] as? [String: Any] ?? [:]
            let boxField = (detail["box"] as? String) ?? (item["box"] as? String) ?? ""
            let project = Project(
                id: id,
                name: p["name"] as? String ?? "Project",
                icon: p["icon"] as? String ?? "Ω",
                color: p["color"] as? String ?? "#8b7cf6",
                state: BoxState.from(state: p["state"] as? String ?? "", box: boxField),
                stateSince: (detail["box_since"] as? NSNumber)?.int64Value ?? (p["box_since"] as? NSNumber)?.int64Value,
                stateDetail: nil,
                model: talking["id"] as? String ?? "anthropic/claude-haiku-5.5",
                effort: Effort.from(talking["effort"] as? String),
                spendTodayUsd: (detail["spend_today"] as? NSNumber)?.doubleValue
                    ?? (p["spend_today_usd"] as? NSNumber)?.doubleValue ?? 0,
                spendCapUsd: (p["spend_cap_usd"] as? NSNumber)?.doubleValue
                    ?? (p["spend_cap_daily"] as? NSNumber)?.doubleValue ?? 25,
                shared: p["shared"] as? Bool ?? false,
                ownerHandle: p["owner_handle"] as? String
            )
            let chats = ((detail["chats"] as? [Any]) ?? []).compactMap(RelayAPI.parseChat)
            out.append(RegistryProject(project: project, chats: chats))
        }
        return out
    }

    static func parseChat(_ raw: Any) -> Chat? {
        guard let v = raw as? [String: Any], let id = v["id"] as? String, let pid = v["project_id"] as? String else {
            return nil
        }
        return Chat(
            id: id,
            projectId: pid,
            title: v["title"] as? String ?? "New chat",
            isPinned: v["is_pinned"] as? Bool ?? false,
            unread: false,
            lastMessageAt: (v["created_at"] as? NSNumber)?.int64Value ?? 0
        )
    }

    func createChat(projectId: Id) async throws -> Chat {
        let v = try await json("POST", "/projects/\(projectId)/chats", body: ["title": "New chat"])
        guard let m = v as? [String: Any], let chat = RelayAPI.parseChat(m["chat"] ?? NSNull()) else {
            throw RelayError(status: 0, message: "The relay returned no chat")
        }
        return chat
    }

    func closeChat(projectId: Id, chatId: Id) async throws {
        _ = try await request("DELETE", "/projects/\(projectId)/chats/\(chatId)")
    }

    func createProject(name: String) async throws {
        _ = try await json("POST", "/projects", body: ["name": name])
    }

    func setModel(projectId: Id, model: String, effort: Effort) async throws {
        let body: [String: Any] = ["models": ["talking_model": ["id": model, "effort": effort.rawValue]]]
        _ = try await request("PATCH", "/projects/\(projectId)", body: body)
    }

    func wake(projectId: Id) async throws {
        _ = try await json("POST", "/projects/\(projectId)/wake", body: [String: Any]())
    }

    // MARK: Machine (D70: one box per user, projects are workspaces on it)

    /// `GET /machine` → the caller's one box and its workspaces.
    func machine() async throws -> Machine {
        guard let m = try await json("GET", "/machine") as? [String: Any] else {
            throw RelayError(status: 0, message: "Bad /machine response")
        }
        let spec = m["spec"] as? [String: Any] ?? [:]
        var workspaces: [Id: WorkspaceState] = [:]
        for w in (m["workspaces"] as? [Any]) ?? [] {
            if let w = w as? [String: Any], let pid = w["project_id"] as? String {
                workspaces[pid] = WorkspaceState.from(w["state"] as? String)
            }
        }
        return Machine(
            state: BoxState.fromWire(m["state"] as? String ?? "") ?? .hibernatedLight,
            since: (m["since"] as? NSNumber)?.int64Value,
            spec: Machine.specLine(
                cpu: (spec["cpu"] as? NSNumber)?.intValue,
                memGib: (spec["mem_gib"] as? NSNumber)?.intValue,
                diskGib: (spec["disk_gib"] as? NSNumber)?.intValue,
                klass: spec["class"] as? String,
                provider: m["provider"] as? String
            ),
            provider: m["provider"] as? String ?? "",
            sandboxId: m["sandbox_id"] as? String,
            spendTodayUsd: (m["spend_today_usd"] as? NSNumber)?.doubleValue ?? 0,
            spendCapUsd: (m["spend_cap_usd"] as? NSNumber)?.doubleValue ?? 0,
            workspaces: workspaces,
            reported: true
        )
    }

    func startMachine() async throws {
        _ = try await json("POST", "/machine/start", body: [String: Any]())
    }

    func stopMachine() async throws {
        _ = try await json("POST", "/machine/stop", body: [String: Any]())
    }

    /// Model ids the relay proxy can serve, filtered like the Mac client.
    func models() async throws -> [String] {
        let v = try await json("GET", "/proxy/v1/models")
        let data = (v as? [String: Any])?["data"] as? [Any] ?? []
        let allowed = ["anthropic/", "openai/", "google/", "x-ai/"]
        var ids = data.compactMap { ($0 as? [String: Any])?["id"] as? String }
            .filter { id in !id.contains(":") && allowed.contains { id.hasPrefix($0) } }
        ids.sort()
        var seen = Set<String>()
        ids = ids.filter { seen.insert($0).inserted }
        return ids
    }

    /// Upload an attachment to the relay blob mirror; returns the key.
    func putBlob(projectId: Id, key: String, mime: String, data: Data) async throws -> String {
        guard let url = URL(string: "\(endpoint.httpUrl)/blob/\(projectId)/\(key)") else {
            throw RelayError(status: 0, message: "Bad relay URL")
        }
        var req = URLRequest(url: url)
        req.httpMethod = "PUT"
        req.setValue("Bearer \(token())", forHTTPHeaderField: "Authorization")
        req.setValue(mime, forHTTPHeaderField: "Content-Type")
        req.httpBody = data
        let (body, response) = try await session.data(for: req)
        guard let http = response as? HTTPURLResponse, http.statusCode < 400 else {
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            throw RelayError(status: status, message: String(data: body, encoding: .utf8) ?? "")
        }
        return key
    }

    // MARK: People chats (D72)

    func searchUsers(_ q: String) async throws -> [Person] {
        let query = q.trimmingCharacters(in: .whitespaces).trimmingCharacters(in: CharacterSet(charactersIn: "@"))
        guard !query.isEmpty, let enc = query.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) else { return [] }
        return ((try await json("GET", "/users/search?q=\(enc)")) as? [Any] ?? []).compactMap(Person.parse)
    }

    func openDM(handle: String) async throws -> PeopleChat {
        let v = try await json("POST", "/chats/dm", body: ["handle": handle.trimmingCharacters(in: CharacterSet(charactersIn: "@ "))])
        guard let chat = PeopleChat.parse(v) else { throw RelayError(status: 0, message: "The relay returned no chat") }
        return chat
    }

    func peopleChats() async throws -> [PeopleChat] {
        ((try await json("GET", "/chats")) as? [Any] ?? []).compactMap(PeopleChat.parse)
    }

    func peopleChat(_ id: String) async throws -> PeopleChat {
        guard let chat = PeopleChat.parse(try await json("GET", "/chats/\(id)")) else { throw RelayError(status: 0, message: "Bad chat") }
        return chat
    }

    func markRead(chatId: String, seq: Int64) async throws {
        _ = try await request("PUT", "/chats/\(chatId)/read", body: ["seq": seq])
    }

    func leaveChat(_ id: String) async throws { _ = try await request("POST", "/chats/\(id)/leave", body: [String: Any]()) }
    func hideChat(_ id: String) async throws { _ = try await request("POST", "/chats/\(id)/hide", body: [String: Any]()) }
    func deleteChat(_ id: String) async throws { _ = try await request("DELETE", "/chats/\(id)") }

    func addAgent(chatId: String, projectId: Id) async throws -> PeopleChat {
        let v = try await json("POST", "/chats/\(chatId)/agents", body: ["project_id": projectId])
        if let chat = PeopleChat.parse(v) { return chat }
        return try await peopleChat(chatId)
    }

    func removeAgent(chatId: String, projectId: Id) async throws {
        _ = try await request("DELETE", "/chats/\(chatId)/agents/\(projectId)")
    }

    func setReplyPolicy(chatId: String, projectId: Id, policy: String) async throws {
        _ = try await request("PUT", "/chats/\(chatId)/agents/\(projectId)/settings", body: ["reply_policy": policy])
    }

    func removeMember(chatId: String, userId: String) async throws {
        _ = try await request("DELETE", "/chats/\(chatId)/members/\(userId)")
    }

    func block(handle: String) async throws { _ = try await request("POST", "/blocks", body: ["handle": handle]) }
    func unblock(userId: String) async throws { _ = try await request("DELETE", "/blocks/\(userId)") }
    func blocks() async throws -> [Person] {
        ((try await json("GET", "/blocks")) as? [Any] ?? []).compactMap { raw -> Person? in
            guard let d = raw as? [String: Any], let id = d["id"] as? String else { return nil }
            return Person(id: id, handle: d["handle"] as? String ?? "", avatarUrl: d["avatar_url"] as? String, name: nil, status: "blocked")
        }
    }

    func pushes() async throws -> [PushItem] {
        ((try await json("GET", "/me/pushes")) as? [Any] ?? []).compactMap(PushItem.parse)
    }

    // Projects together (step 3)
    func projectMembers(_ projectId: Id) async throws -> [ProjectMember] {
        let v = try await json("GET", "/projects/\(projectId)/members")
        let list = (v as? [Any]) ?? ((v as? [String: Any])?["members"] as? [Any]) ?? []
        return list.compactMap(ProjectMember.parse)
    }

    func addProjectMember(_ projectId: Id, handle: String) async throws -> [ProjectMember] {
        let v = try await json("POST", "/projects/\(projectId)/members", body: ["handle": handle.trimmingCharacters(in: CharacterSet(charactersIn: "@ "))])
        return (((v as? [String: Any])?["members"] as? [Any]) ?? []).compactMap(ProjectMember.parse)
    }

    func removeProjectMember(_ projectId: Id, userId: String) async throws {
        _ = try await request("DELETE", "/projects/\(projectId)/members/\(userId)")
    }

    /// Room checkpoint (crdt-sync §7.3). Returns `(seq, bytes)`.
    func checkpoint(room: String) async throws -> (Int64, Data) {
        let (data, http) = try await request("GET", "/room/\(room)/checkpoint")
        let seq = Int64(http.value(forHTTPHeaderField: "x-arbos-checkpoint-seq") ?? "") ?? 0
        return (seq, data)
    }

    /// WebSocket URL for a room; the token is re-read before every dial (§10).
    func roomURL(room: String) -> URL? {
        URL(string: "\(endpoint.wsUrl)/room/\(room)?token=\(token())")
    }
}
