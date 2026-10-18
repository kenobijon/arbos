// Relay `/secrets` routes (D83 §10.1). Values travel one way: in a create or replace body,
// never back. Errors are the relay's `{"error":{"type","message"}}` through `RelayError`.

import Foundation

extension RelayAPI {
    func secrets() async throws -> SecretsOverview {
        guard let v = SecretsOverview.parse(try await json("GET", "/secrets")) else {
            throw RelayError(status: 0, message: "Bad secrets reply")
        }
        return v
    }

    /// `POST /secrets` → the new secret. `value` (key/text) or `fields` (login) is sent once.
    func createSecret(_ body: [String: Any]) async throws -> SecretItem {
        let v = try await json("POST", "/secrets", body: body) as? [String: Any]
        guard let s = SecretItem.parse(v?["secret"]) else { throw RelayError(status: 0, message: "Bad secret reply") }
        return s
    }

    func secret(_ id: String) async throws -> (SecretItem, [SecretActivity]) {
        let v = try await json("GET", "/secrets/\(id)") as? [String: Any]
        guard let s = SecretItem.parse(v?["secret"]) else { throw RelayError(status: 404, message: "No such secret", type: "not_found") }
        return (s, (v?["activity"] as? [Any])?.compactMap(SecretActivity.parse) ?? [])
    }

    func patchSecret(_ id: String, _ body: [String: Any]) async throws -> SecretItem {
        let v = try await json("PATCH", "/secrets/\(id)", body: body) as? [String: Any]
        guard let s = SecretItem.parse(v?["secret"]) else { throw RelayError(status: 0, message: "Bad secret reply") }
        return s
    }

    func replaceSecretValue(_ id: String, _ body: [String: Any]) async throws -> SecretItem {
        let v = try await json("PUT", "/secrets/\(id)/value", body: body) as? [String: Any]
        guard let s = SecretItem.parse(v?["secret"]) else { throw RelayError(status: 0, message: "Bad secret reply") }
        return s
    }

    func deleteSecret(_ id: String) async throws {
        _ = try await request("DELETE", "/secrets/\(id)")
    }

    /// `PUT /secrets/{id}/access/{project_id}` — create or update one switch.
    func setSecretAccess(_ id: String, projectId: String, enabled: Bool, note: String? = nil, hosts: [String]? = nil, scope: [String: Any]? = nil) async throws -> SecretAgentAccess? {
        var body: [String: Any] = ["enabled": enabled]
        if let note { body["note"] = note }
        if let hosts { body["hosts"] = hosts }
        if let scope { body["scope"] = scope }
        let v = try await json("PUT", "/secrets/\(id)/access/\(projectId)", body: body) as? [String: Any]
        return SecretAgentAccess.parse(v?["access"])
    }

    func deleteSecretAccess(_ id: String, projectId: String) async throws {
        _ = try await request("DELETE", "/secrets/\(id)/access/\(projectId)")
    }

    func revokeSecretEverywhere(_ id: String) async throws -> SecretItem? {
        let v = try await json("POST", "/secrets/\(id)/revoke-all") as? [String: Any]
        return SecretItem.parse(v?["secret"])
    }

    func projectSecrets(_ projectId: String) async throws -> ProjectSecrets {
        guard let v = ProjectSecrets.parse(try await json("GET", "/projects/\(projectId)/secrets")) else {
            throw RelayError(status: 0, message: "Bad reply")
        }
        return v
    }

    /// `PUT /me/secrets/settings {paused}` — the kill switch.
    func setSecretsPaused(_ paused: Bool) async throws -> Bool {
        let v = try await json("PUT", "/me/secrets/settings", body: ["paused": paused]) as? [String: Any]
        return DocJSON.bool(v?["paused"])
    }

    func secretsActivity(secretId: String? = nil, projectId: String? = nil, vaultId: String? = nil, before: Int64? = nil, limit: Int = 100) async throws -> (items: [SecretActivity], nextBefore: Int64?) {
        var q: [String] = ["limit=\(limit)"]
        if let secretId { q.append("secret_id=\(secretId)") }
        if let projectId { q.append("project_id=\(projectId)") }
        if let vaultId { q.append("vault_id=\(vaultId)") }
        if let before { q.append("before=\(before)") }
        let v = try await json("GET", "/secrets/activity?" + q.joined(separator: "&")) as? [String: Any]
        return ((v?["items"] as? [Any])?.compactMap(SecretActivity.parse) ?? [], DocJSON.int(v?["next_before"]))
    }

    func secretRequests() async throws -> [SecretRequestItem] {
        let v = try await json("GET", "/secrets/requests") as? [String: Any]
        return (v?["items"] as? [Any])?.compactMap(SecretRequestItem.parse) ?? []
    }

    /// `POST /secrets/requests/{id}/decide {decision}` — `once | chat | always | deny`.
    func decideSecretRequest(_ id: String, decision: String) async throws {
        _ = try await request("POST", "/secrets/requests/\(id)/decide", body: ["decision": decision])
    }

    // MARK: Connected vaults (Phase 2)

    func connectVault(provider: String, token: String, name: String?, agents: [[String: Any]]) async throws -> ConnectedVault {
        var body: [String: Any] = ["provider": provider, "token": token, "agents": agents]
        if let name, !name.isEmpty { body["name"] = name }
        let v = try await json("POST", "/vaults", body: body) as? [String: Any]
        guard let vault = ConnectedVault.parse(v?["vault"]) else { throw RelayError(status: 0, message: "Bad vault reply") }
        return vault
    }

    func vault(_ id: String) async throws -> (ConnectedVault, [[String: Any]], [SecretActivity]) {
        let v = try await json("GET", "/vaults/\(id)") as? [String: Any]
        guard let vault = ConnectedVault.parse(v?["vault"]) else { throw RelayError(status: 404, message: "No such vault", type: "not_found") }
        return (vault, (v?["items"] as? [[String: Any]]) ?? [], (v?["activity"] as? [Any])?.compactMap(SecretActivity.parse) ?? [])
    }

    func replaceVaultToken(_ id: String, token: String) async throws {
        _ = try await request("PUT", "/vaults/\(id)/token", body: ["token": token])
    }

    func refreshVault(_ id: String) async throws {
        _ = try await request("POST", "/vaults/\(id)/refresh")
    }

    func setVaultAccess(_ id: String, projectId: String, enabled: Bool, note: String? = nil) async throws {
        var body: [String: Any] = ["enabled": enabled]
        if let note { body["note"] = note }
        _ = try await request("PUT", "/vaults/\(id)/access/\(projectId)", body: body)
    }

    func deleteVault(_ id: String) async throws {
        _ = try await request("DELETE", "/vaults/\(id)")
    }
}
