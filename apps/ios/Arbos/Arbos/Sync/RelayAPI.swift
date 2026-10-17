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

    /// `ios/1.0.0 (202610161326)` — who placed a call, for the relay's call record.
    static var clientHeader: String {
        let v = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "?"
        let b = Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "?"
        return "ios/\(v) (\(b))"
    }

    func request(_ method: String, _ path: String, body: Any? = nil, auth: Bool = true, headers: [String: String] = [:]) async throws -> (Data, HTTPURLResponse) {
        do {
            return try await requestOnce(method, path, body: body, auth: auth, headers: headers)
        } catch let error as RelayError where error.isUnauthorized && auth {
            if let refresher, await refresher() {
                return try await requestOnce(method, path, body: body, auth: auth, headers: headers)
            }
            throw error
        }
    }

    private func requestOnce(_ method: String, _ path: String, body: Any?, auth: Bool, headers: [String: String] = [:]) async throws -> (Data, HTTPURLResponse) {
        guard let url = URL(string: endpoint.httpUrl + path) else {
            throw RelayError(status: 0, message: "Bad relay URL")
        }
        var req = URLRequest(url: url)
        req.httpMethod = method
        if auth {
            req.setValue("Bearer \(token())", forHTTPHeaderField: "Authorization")
        }
        for (k, v) in headers { req.setValue(v, forHTTPHeaderField: k) }
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
            let owner = p["owner"] as? [String: Any] ?? [:]
            let machine = p["machine"] as? [String: Any] ?? [:]
            let primaryChat: String? = ((p["primary_chat"] as? String) ?? (p["primary_chat_id"] as? String) ?? (detail["primary_chat"] as? String)).flatMap { $0.isEmpty ? nil : $0 }
            let spendToday: Double = (detail["spend_today"] as? NSNumber)?.doubleValue ?? (p["spend_today_usd"] as? NSNumber)?.doubleValue ?? 0
            let spendCap: Double = (p["spend_cap_usd"] as? NSNumber)?.doubleValue ?? (p["spend_cap_daily"] as? NSNumber)?.doubleValue ?? 25
            let since: Int64? = (detail["box_since"] as? NSNumber)?.int64Value ?? (p["box_since"] as? NSNumber)?.int64Value
            var project = Project(
                id: id,
                name: p["name"] as? String ?? "Project",
                icon: p["icon"] as? String ?? "Ω",
                color: p["color"] as? String ?? "#8b7cf6",
                state: BoxState.from(state: p["state"] as? String ?? "", box: boxField),
                stateSince: since,
                stateDetail: nil,
                model: talking["id"] as? String ?? "anthropic/claude-haiku-5.5",
                effort: Effort.from(talking["effort"] as? String),
                spendTodayUsd: spendToday,
                spendCapUsd: spendCap
            )
            project.shared = p["shared"] as? Bool ?? false
            project.ownerHandle = p["owner_handle"] as? String
            project.avatar = (p["avatar"] as? String).flatMap { $0.isEmpty ? nil : $0 }
            project.avatarColor = (p["avatar_color"] as? String).flatMap { $0.isEmpty ? nil : $0 }
            project.instructions = (p["instructions"] as? String) ?? ""
            project.description = (p["description"] as? String) ?? ""
            project.ownerName = owner["name"] as? String
            project.ownerAvatarUrl = owner["avatar_url"] as? String
            project.ownerLastSeenAt = (owner["last_seen_at"] as? NSNumber)?.int64Value
            project.ownerOnline = (owner["online"] as? NSNumber)?.boolValue ?? false
            project.machineState = machine["state"] as? String
            project.machineClass = machine["class"] as? String
            project.machineSnapshot = machine["snapshot"] as? String
            project.machineAlwaysOn = (machine["always_on"] as? NSNumber)?.boolValue ?? false
            project.primaryChatId = primaryChat
            project.voice = ((p["voice"] as? String) ?? (detail["voice"] as? String)).flatMap { $0.isEmpty ? nil : $0 }
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
            lastMessageAt: (v["created_at"] as? NSNumber)?.int64Value ?? 0,
            kind: v["kind"] as? String ?? "chat",
            participants: ((v["participants"] as? [Any]) ?? []).compactMap { ($0 as? String) ?? ($0 as? [String: Any])?["project_id"] as? String ?? ($0 as? [String: Any])?["id"] as? String }
        )
    }

    func createChat(projectId: Id) async throws -> Chat {
        let v = try await json("POST", "/projects/\(projectId)/chats", body: ["title": "New chat"])
        guard let m = v as? [String: Any], let chat = RelayAPI.parseChat(m["chat"] ?? NSNull()) else {
            throw RelayError(status: 0, message: "The relay returned no chat")
        }
        return chat
    }


    func createProject(
        name: String,
        avatar: String? = nil,
        avatarColor: String? = nil,
        color: String? = nil,
        description: String? = nil,
        instructions: String? = nil
    ) async throws {
        var body: [String: Any] = ["name": name]
        if let avatar { body["avatar"] = avatar }
        if let avatarColor { body["avatar_color"] = avatarColor }
        if let color { body["color"] = color }
        if let description, !description.isEmpty { body["description"] = String(description.prefix(500)) }
        if let instructions, !instructions.isEmpty { body["instructions"] = instructions }
        _ = try await json("POST", "/projects", body: body)
    }

    /// Soft-delete an owned agent (`DELETE /projects/{id}`).
    func deleteProject(_ projectId: Id) async throws {
        _ = try await request("DELETE", "/projects/\(projectId)")
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
        let alwaysOn = (m["always_on"] as? NSNumber)?.boolValue ?? false
        let held = (m["held"] as? NSNumber)?.boolValue ?? false
        let wire = BoxState.fromWire(m["state"] as? String ?? "") ?? .hibernatedLight
        return Machine(
            state: alwaysOn && !held && wire != .error ? .awake : wire,
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
            alwaysOn: alwaysOn,
            held: held,
            machineTodayUsd: (m["machine_today_usd"] as? NSNumber)?.doubleValue ?? 0,
            reported: true
        )
    }

    func startMachine() async throws {
        _ = try await json("POST", "/machine/start", body: [String: Any]())
    }

    func stopMachine() async throws {
        _ = try await json("POST", "/machine/stop", body: [String: Any]())
    }

    /// Always-on boxes (D73) are restarted, never stopped. Uses
    /// `POST /machine/restart` when the relay has it and falls back to
    /// stop → start on relays that predate the route.
    func restartMachine() async throws {
        do {
            _ = try await json("POST", "/machine/restart", body: [String: Any]())
        } catch let error as RelayError where error.status == 404 || error.status == 405 {
            try await stopMachine()
            try await startMachine()
        }
    }

    /// Model ids the relay proxy can serve, filtered like the Mac client,
    /// then trimmed to the newest major of each family and sorted newest-first.
    func models() async throws -> [String] {
        let v = try await json("GET", "/proxy/v1/models")
        let data = (v as? [String: Any])?["data"] as? [Any] ?? []
        let allowed = ["anthropic/", "openai/", "google/", "x-ai/"]
        var ids = data.compactMap { ($0 as? [String: Any])?["id"] as? String }
            .filter { id in !id.contains(":") && allowed.contains { id.hasPrefix($0) } }
        var seen = Set<String>()
        ids = ids.filter { seen.insert($0).inserted }
        ids = Self.currentModels(ids)
        ids.sort(by: Self.newerModel)
        return ids
    }

    /// Drop older majors in the same provider/stem family
    /// (`google/gemini-2.5-*` goes away when any `google/gemini-3*` is listed).
    private static func currentModels(_ ids: [String]) -> [String] {
        var maxMajor: [String: Int] = [:]
        for id in ids {
            let key = modelFamily(id)
            if let major = modelVersion(id).first {
                maxMajor[key] = max(maxMajor[key] ?? major, major)
            }
        }
        return ids.filter { id in
            guard let major = modelVersion(id).first, let keep = maxMajor[modelFamily(id)] else {
                return true
            }
            return major == keep
        }
    }

    /// `google/gemini-3.1-flash` → `google/gemini`.
    private static func modelFamily(_ id: String) -> String {
        let parts = id.split(separator: "/", maxSplits: 1).map(String.init)
        let provider = parts.first ?? ""
        let leaf = parts.count > 1 ? parts[1] : id
        let stem = leaf.prefix(while: \.isLetter)
        return "\(provider)/\(stem)"
    }

    /// Number groups in the leaf id (`gemini-3.1-flash` → `[3, 1]`). Trailing
    /// date stamps (`…-20241022`) are dropped so they do not look like versions.
    private static func modelVersion(_ id: String) -> [Int] {
        let leaf = id.split(separator: "/").last.map(String.init) ?? id
        var nums: [Int] = []
        var current = ""
        for ch in leaf {
            if ch.isNumber {
                current.append(ch)
            } else if !current.isEmpty {
                if let n = Int(current) { nums.append(n) }
                current = ""
            }
        }
        if !current.isEmpty, let n = Int(current) { nums.append(n) }
        if let last = nums.last, last >= 10_000_000 { nums.removeLast() }
        return nums
    }

    /// Newer first: higher version within a family, else reverse id order.
    private static func newerModel(_ a: String, _ b: String) -> Bool {
        let fa = modelFamily(a), fb = modelFamily(b)
        if fa == fb {
            let va = modelVersion(a), vb = modelVersion(b)
            let n = min(va.count, vb.count)
            for i in 0..<n where va[i] != vb[i] { return va[i] > vb[i] }
            if va.count != vb.count { return va.count > vb.count }
        }
        return a > b
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

    /// `POST /feedback` (JSON, device token): `{note ≤4000, image: base64 PNG/JPEG ≤15 MB,
    /// mime, app_version, platform: "ios", os, project_id?, width, height}` → 201 `{id, created_at}`.
    /// Feeds the same automatic fix loop as the Mac sheet.
    func sendFeedback(note: String, image: Data?, mime: String?, width: Int, height: Int,
                      appVersion: String, os: String, projectId: Id?) async throws -> String? {
        var body: [String: Any] = [
            "note": String(note.prefix(4000)), "app_version": appVersion, "platform": "ios", "os": os,
            "width": width, "height": height,
        ]
        if let image, let mime {
            body["image"] = image.base64EncodedString()
            body["mime"] = mime
        }
        if let projectId, !projectId.isEmpty { body["project_id"] = projectId }
        let v = try await json("POST", "/feedback", body: body)
        return (v as? [String: Any])?["id"] as? String
    }

    /// `POST /proxy/v1/audio/transcriptions` (multipart `file` + `model`) → the memo's text.
    /// The relay meters it on `projectId` (`X-Arbos-Project`).
    func transcribe(projectId: Id?, data: Data, mime: String) async throws -> String {
        guard let url = URL(string: "\(endpoint.httpUrl)/proxy/v1/audio/transcriptions") else {
            throw RelayError(status: 0, message: "Bad relay URL")
        }
        let boundary = "arbos-\(UUID().uuidString)"
        var body = Data()
        // Match Mac: mini-transcribe is much faster than whisper-1 for short voice memos.
        body.append(Data("--\(boundary)\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ngpt-4o-mini-transcribe\r\n".utf8))
        body.append(Data("--\(boundary)\r\nContent-Disposition: form-data; name=\"file\"; filename=\"memo.m4a\"\r\nContent-Type: \(mime)\r\n\r\n".utf8))
        body.append(data)
        body.append(Data("\r\n--\(boundary)--\r\n".utf8))
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.timeoutInterval = 60
        req.setValue("Bearer \(token())", forHTTPHeaderField: "Authorization")
        req.setValue("multipart/form-data; boundary=\(boundary)", forHTTPHeaderField: "Content-Type")
        if let projectId { req.setValue(projectId, forHTTPHeaderField: "X-Arbos-Project") }
        req.httpBody = body
        let (reply, response) = try await session.data(for: req)
        guard let http = response as? HTTPURLResponse, http.statusCode < 400 else {
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            throw RelayError(status: status, message: String(data: reply, encoding: .utf8) ?? "")
        }
        guard let obj = try? JSONSerialization.jsonObject(with: reply) as? [String: Any], let text = obj["text"] as? String else {
            throw RelayError(status: http.statusCode, message: "No transcript in the reply")
        }
        return text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    // MARK: Calls v1

    /// `POST /chats/{id}/calls` (or the projects form) `{kind}` → 201 new call / 200 join;
    /// both carry a fresh LiveKit token. Returns the call and whether we started it.
    func startCall(chatId: String, projectId: String?, kind: CallKind) async throws -> (Call, started: Bool) {
        let path = (projectId == nil || projectId == peopleSlot) ? "/chats/\(chatId)/calls" : "/projects/\(projectId!)/chats/\(chatId)/calls"
        let (data, http) = try await request("POST", path, body: ["kind": kind.rawValue], headers: ["X-Arbos-Client": RelayAPI.clientHeader])
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any], let call = Call.parse(obj) else {
            throw RelayError(status: http.statusCode, message: "Bad call response")
        }
        // 201 {call, joined: false} = we started it; 200 {call, joined: true} = we were added.
        let joined = (obj["joined"] as? NSNumber)?.boolValue ?? (http.statusCode != 201)
        return (call, !joined)
    }

    func joinCall(_ callId: String) async throws -> Call {
        guard let call = Call.parse(try await json("POST", "/calls/\(callId)/join", body: [String: Any]())) else {
            throw RelayError(status: 0, message: "Bad call response")
        }
        return call
    }

    /// `POST /calls/{id}/leave` → `{ended, duration_s}`; the client told `ended: true` writes the end line.
    func leaveCall(_ callId: String) async throws -> (ended: Bool, durationS: Int) {
        let v = try await json("POST", "/calls/\(callId)/leave", body: [String: Any]()) as? [String: Any]
        return (((v?["ended"] as? NSNumber)?.boolValue) ?? false, ((v?["duration_s"] as? NSNumber)?.intValue) ?? 0)
    }

    /// `GET /calls/{id}` → the call, also after it ended (no token then).
    func call(_ callId: String) async throws -> Call {
        guard let call = Call.parse(try await json("GET", "/calls/\(callId)")) else { throw RelayError(status: 0, message: "Bad call response") }
        return call
    }

    /// `GET /chats/{id}/call` (or the projects form) → the active call, nil on 404.
    func activeCall(chatId: String, projectId: String?) async throws -> Call? {
        let path = (projectId == nil || projectId == peopleSlot) ? "/chats/\(chatId)/call" : "/projects/\(projectId!)/chats/\(chatId)/call"
        do {
            return Call.parse(try await json("GET", path))
        } catch let e as RelayError where e.status == 404 {
            return nil
        }
    }

    /// `POST /devices/{id}/voip {token, environment, bundle_id}` → `{push}` (false until the APNs key exists).
    func registerVoip(deviceId: Id, token: String, environment: String, bundleId: String) async throws -> Bool {
        let v = try await json("POST", "/devices/\(deviceId)/voip", body: ["token": token, "environment": environment, "bundle_id": bundleId])
        return ((v as? [String: Any])?["push"] as? NSNumber)?.boolValue ?? true
    }

    func unregisterVoip(deviceId: Id) async throws {
        _ = try await request("DELETE", "/devices/\(deviceId)/voip")
    }

    // MARK: Push notifications

    /// `POST /devices/{id}/push {platform, token, environment, bundle_id, app_version}`
    /// registers this device's APNs token (idempotent; re-sent on every launch).
    /// Returns the relay's `push` flag: false until it has an APNs key.
    func registerPush(deviceId: Id, token: String, environment: String, bundleId: String, appVersion: String) async throws -> Bool {
        let v = try await json("POST", "/devices/\(deviceId)/push", body: [
            "platform": "ios", "token": token, "environment": environment, "bundle_id": bundleId, "app_version": appVersion,
        ])
        return ((v as? [String: Any])?["push"] as? NSNumber)?.boolValue ?? true
    }

    /// `DELETE /devices/{id}/push` on sign-out so a stale token is not pushed to.
    func unregisterPush(deviceId: Id) async throws {
        _ = try await request("DELETE", "/devices/\(deviceId)/push")
    }

    /// `GET /me/unread` → total + per-chat counts. nil when the relay has no route yet.
    func unread() async throws -> (total: Int, chats: [String: Int])? {
        do {
            guard let v = try await json("GET", "/me/unread") as? [String: Any] else { return nil }
            var chats: [String: Int] = [:]
            if let list = v["chats"] as? [[String: Any]] {
                for c in list { if let id = c["chat_id"] as? String { chats[id] = (c["unread"] as? NSNumber)?.intValue ?? 0 } }
            } else if let dict = v["chats"] as? [String: Any] {
                for (k, n) in dict { chats[k] = (n as? NSNumber)?.intValue ?? 0 }
            }
            let total = (v["total"] as? NSNumber)?.intValue ?? chats.values.reduce(0, +)
            return (total, chats)
        } catch let e as RelayError where e.status == 404 || e.status == 405 {
            return nil
        }
    }

    // MARK: People chats (D72)

    /// `GET /users/{id}` → one person with presence (§9); nil on 404.
    func user(_ id: String) async throws -> Person? {
        do { return Person.parse(try await json("GET", "/users/\(id)")) }
        catch let e as RelayError where e.status == 404 { return nil }
    }

    /// `GET /users?offset&limit` → the directory (members and invited people
    /// except me), alphabetical, paged by `next_offset`.
    func users() async throws -> [Person] {
        var out: [Person] = []
        var offset = 0
        while true {
            guard let page = try await json("GET", "/users?offset=\(offset)&limit=500") as? [String: Any] else { break }
            out += ((page["users"] as? [Any]) ?? []).compactMap(Person.parse)
            guard let next = (page["next_offset"] as? NSNumber)?.intValue, next > offset else { break }
            offset = next
        }
        return out
    }

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

    func createGroup(handles: [String]) async throws -> PeopleChat {
        let v = try await json("POST", "/chats", body: ["kind": "group", "members": handles])
        guard let chat = PeopleChat.parse(v) else { throw RelayError(status: 0, message: "The relay returned no chat") }
        return chat
    }

    func patchProject(_ projectId: Id, name: String?, color: String?, icon: String?, avatar: String? = nil, avatarColor: String? = nil, description: String? = nil) async throws {
        var body: [String: Any] = [:]
        if let description { body["description"] = String(description.prefix(500)) }
        if let name { body["name"] = name }
        if let color { body["color"] = color }
        if let icon { body["icon"] = icon }
        if let avatar { body["avatar"] = avatar }
        // "mono" travels as a slug: a null would make the relay re-pick its coloured default (D74).
        if let avatarColor { body["avatar_color"] = avatarColor }
        guard !body.isEmpty else { return }
        _ = try await request("PATCH", "/projects/\(projectId)", body: body)
    }

    /// §10.3 `PATCH /projects/{id} {"voice": id|null}` (null = the default voice). Returns the
    /// project's `voice` as the relay now has it.
    func setProjectVoice(_ projectId: Id, voice: String?) async throws -> String? {
        let value: Any = voice.map { $0 as Any } ?? NSNull()
        let body: [String: Any] = ["voice": value]
        let v = try await json("PATCH", "/projects/\(projectId)", body: body)
        let m = v as? [String: Any]
        let p: [String: Any]? = (m?["project"] as? [String: Any]) ?? m
        let kept: String? = p?["voice"] as? String
        return kept.flatMap { $0.isEmpty ? nil : $0 }
    }

    /// §10.3 `GET /voices` (no auth) → the default id and the catalogue. `sample_url` may be
    /// relative; it is resolved against the relay.
    func voices() async throws -> VoiceCatalog {
        let v = try await json("GET", "/voices", auth: false)
        guard let m = v as? [String: Any] else { throw RelayError(status: 0, message: "The relay returned no voices") }
        let base = endpoint.httpUrl
        let list: [Voice] = ((m["voices"] as? [[String: Any]]) ?? []).compactMap { d in
            guard let id = d["id"] as? String, !id.isEmpty else { return nil }
            let rawSample = (d["sample_url"] as? String) ?? "/voices/\(id)/sample.mp3"
            let sample = rawSample.hasPrefix("http") ? rawSample : base + (rawSample.hasPrefix("/") ? "" : "/") + rawSample
            return Voice(id: id, name: (d["name"] as? String) ?? id.capitalized, description: d["description"] as? String,
                         seconds: (d["seconds"] as? NSNumber)?.doubleValue, sampleURL: URL(string: sample))
        }
        return VoiceCatalog(defaultId: (m["default"] as? String) ?? list.first?.id ?? "", voices: list)
    }

    func peopleChats() async throws -> [PeopleChat] {
        try await peopleChatsJSON().compactMap(PeopleChat.parse)
    }

    /// The relay's `GET /chats` array as received, so the app can keep it on device.
    func peopleChatsJSON() async throws -> [Any] {
        (try await json("GET", "/chats")) as? [Any] ?? []
    }

    func peopleChat(_ id: String) async throws -> PeopleChat {
        guard let chat = PeopleChat.parse(try await json("GET", "/chats/\(id)")) else { throw RelayError(status: 0, message: "Bad chat") }
        return chat
    }

    func markRead(chatId: String, seq: Int64) async throws {
        _ = try await request("PUT", "/chats/\(chatId)/read", body: ["seq": seq])
    }

    /// Announce a message we just wrote so recipients get a real preview
    /// (§7c): people chats `POST /chats/{id}/notify`, project chats
    /// `POST /projects/{pid}/chats/{cid}/notify`. Deduped by the relay.
    func notifyMessage(chatId: String, projectId: Id?, messageId: String, text: String) async throws {
        let body: [String: Any] = ["message_id": messageId, "text": String(text.prefix(140))]
        if let projectId, projectId != peopleSlot {
            _ = try await request("POST", "/projects/\(projectId)/chats/\(chatId)/notify", body: body)
        } else {
            _ = try await request("POST", "/chats/\(chatId)/notify", body: body)
        }
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

    func addMember(chatId: String, handle: String) async throws -> PeopleChat {
        let v = try await json("POST", "/chats/\(chatId)/members", body: ["handle": handle.trimmingCharacters(in: CharacterSet(charactersIn: "@ "))])
        if let chat = PeopleChat.parse(v) { return chat }
        return try await peopleChat(chatId)
    }

    func removeMember(chatId: String, userId: String) async throws {
        _ = try await request("DELETE", "/chats/\(chatId)/members/\(userId)")
    }

    /// `PUT /chats/{id}/settings {read_receipts?, muted?}` per member.
    func chatSettings(chatId: String, muted: Bool? = nil, readReceipts: Bool? = nil) async throws {
        var body: [String: Any] = [:]
        if let muted { body["muted"] = muted }
        if let readReceipts { body["read_receipts"] = readReceipts }
        _ = try await request("PUT", "/chats/\(chatId)/settings", body: body)
    }

    /// `PATCH /chats/{id} {title?, image_key?}` — custom group name and optional picture key.
    func patchPeopleChat(_ chatId: String, title: String? = nil, imageKey: String? = nil) async throws -> PeopleChat {
        var body: [String: Any] = [:]
        if let title { body["title"] = title }
        if let imageKey { body["image_key"] = imageKey }
        let v = try await json("PATCH", "/chats/\(chatId)", body: body)
        guard let chat = PeopleChat.parse(v) else { throw RelayError(status: 0, message: "The relay returned no chat") }
        return chat
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

    // MARK: Connectors (F1)

    /// `GET /connectors/catalog` + `GET /connectors` → one view for Settings.
    func connectors() async throws -> ConnectorsView {
        let catalog = (try await json("GET", "/connectors/catalog")) as? [String: Any] ?? [:]
        let list = (try await json("GET", "/connectors")) as? [String: Any] ?? [:]
        let providers: [ConnectorProvider] = ((catalog["providers"] as? [Any]) ?? []).compactMap { raw in
            guard let m = raw as? [String: Any], let provider = m["provider"] as? String else { return nil }
            return ConnectorProvider(
                provider: provider,
                label: m["label"] as? String ?? provider,
                connect: m["connect"] as? String ?? "oauth",
                // Bool and NSNumber both show up from JSONSerialization.
                oauthConfigured: (m["oauth_configured"] as? Bool)
                    ?? (m["oauth_configured"] as? NSNumber)?.boolValue
                    ?? false
            )
        }
        let items: [Connector] = ((list["items"] as? [Any]) ?? []).compactMap { raw in
            guard let m = raw as? [String: Any], let id = m["id"] as? String, let provider = m["provider"] as? String else { return nil }
            return Connector(
                id: id,
                provider: provider,
                label: m["label"] as? String ?? provider,
                status: ConnectorStatus(wire: m["status"] as? String ?? ""),
                lastError: m["last_error"] as? String,
                mock: (m["mock"] as? NSNumber)?.boolValue ?? false
            )
        }
        return ConnectorsView(
            mock: (list["mock"] as? NSNumber)?.boolValue ?? (catalog["mock"] as? NSNumber)?.boolValue ?? false,
            providers: providers,
            items: items,
            loaded: true
        )
    }

    /// Start OAuth (`authUrl`) or store a pasted token. Google uses OAuth only.
    func connectConnector(provider: String, token: String? = nil) async throws -> String? {
        var body: [String: Any] = [:]
        if let token, !token.isEmpty { body["token"] = token }
        let v = try await json("POST", "/connectors/\(provider)/connect", body: body)
        let m = v as? [String: Any]
        return m?["authUrl"] as? String
    }

    func disconnectConnector(id: String) async throws {
        _ = try await request("DELETE", "/connectors/\(id)")
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
