// Secrets state (D83): the Settings overview, per-agent pages and the actions behind every
// switch. Errors land in `notice` with the relay's message; the Settings sheet shows them above
// itself. Nothing here ever holds a value after the call that sent it returns.

import Foundation

extension String {
    /// `"approval:abc".stripping(prefix: "approval:")` → `"abc"`; nil when the prefix is absent.
    func stripping(prefix: String) -> String? {
        hasPrefix(prefix) ? String(dropFirst(prefix.count)) : nil
    }
}

@MainActor
extension AppModel {
    var secretsRowValue: String? {
        guard credentials != nil else { return nil }
        return secrets?.rowValue ?? "No secrets yet"
    }

    /// The relay can connect a 1Password vault (Phase 2 deployed).
    var vaultProvidersAvailable: Bool { !(secrets?.providers.isEmpty ?? true) }

    func refreshSecrets() async {
        guard credentials != nil, !isFixture else { return }
        do {
            secrets = try await relay.secrets()
        } catch let e as RelayError where e.status == 404 {
            // Older relays have no secrets route.
            secrets = SecretsOverview(paused: false, pausedAt: nil, vaults: [], secrets: [], requestsPending: 0, activity: [])
        } catch {
            // Leave the previous state; opening Settings must not alert on a blip.
        }
    }

    func refreshProjectSecrets(_ projectId: Id) async {
        guard credentials != nil, !isFixture else { return }
        if let v = try? await relay.projectSecrets(projectId) {
            projectSecrets[projectId] = v
        }
    }

    /// Run one secrets call; refresh the overview after; surface the relay's message on failure.
    /// Returns the error text (nil on success) so sheets can show it inline too.
    @discardableResult
    func secretsAction(_ what: String, refreshProject: Id? = nil, _ body: @escaping () async throws -> Void) async -> String? {
        guard !secretsBusy else { return "Busy" }
        secretsBusy = true
        defer { secretsBusy = false }
        do {
            try await body()
            await refreshSecrets()
            if let p = refreshProject { await refreshProjectSecrets(p) }
            return nil
        } catch {
            let text = (error as? RelayError).map { $0.message.isEmpty ? "HTTP \($0.status)" : $0.message } ?? error.localizedDescription
            notice = Notice(text: "\(what): \(text)")
            return text
        }
    }

    func setSecretsPaused(_ paused: Bool) {
        // Optimistic: the switch flips now, the relay confirms (or the refresh puts it back).
        secrets?.paused = paused
        Task { await secretsAction(paused ? "Pause all secrets" : "Secrets") { try await self.relay.setSecretsPaused(paused) } }
    }

    func setSecretAccess(secretId: String, projectId: String, enabled: Bool, note: String? = nil, hosts: [String]? = nil, scope: [String: Any]? = nil) {
        if let i = secrets?.secrets.firstIndex(where: { $0.id == secretId }),
           let j = secrets?.secrets[i].agents.firstIndex(where: { $0.projectId == projectId }) {
            secrets?.secrets[i].agents[j].enabled = enabled
        }
        if let i = projectSecrets[projectId]?.items.firstIndex(where: { $0.secret?.id == secretId }) {
            projectSecrets[projectId]?.items[i].enabled = enabled
        }
        Task {
            await secretsAction(enabled ? "Turn on" : "Turn off", refreshProject: projectId) {
                _ = try await self.relay.setSecretAccess(secretId, projectId: projectId, enabled: enabled, note: note, hosts: hosts, scope: scope)
            }
        }
    }

    func removeSecretAccess(secretId: String, projectId: String) {
        Task { await secretsAction("Turn off", refreshProject: projectId) { try await self.relay.deleteSecretAccess(secretId, projectId: projectId) } }
    }

    func revokeSecretEverywhere(_ secretId: String) {
        Task { await secretsAction("Remove from all agents") { _ = try await self.relay.revokeSecretEverywhere(secretId) } }
    }

    func scrubSecret(_ secretId: String) {
        Task { await secretsAction("Scrub from context") { _ = try await self.relay.scrubSecret(secretId) } }
    }

    func deleteSecret(_ secretId: String) {
        secrets?.secrets.removeAll { $0.id == secretId }
        Task { await secretsAction("Delete secret") { try await self.relay.deleteSecret(secretId) } }
    }

    func patchSecret(_ secretId: String, _ body: [String: Any]) {
        // The two switches are exclusive on the relay; show that at once.
        if let i = secrets?.secrets.firstIndex(where: { $0.id == secretId }) {
            if let on = body["inject_env"] as? Bool {
                secrets?.secrets[i].injectEnv = on
                if on { secrets?.secrets[i].askEveryTime = false }
            }
            if let ask = body["ask_every_time"] as? Bool {
                secrets?.secrets[i].askEveryTime = ask
                if ask { secrets?.secrets[i].injectEnv = false }
            }
        }
        Task { await secretsAction("Save") { _ = try await self.relay.patchSecret(secretId, body) } }
    }

    func decideSecretRequest(_ id: String, decision: String, projectId: String? = nil) {
        Task {
            await secretsAction("Secrets", refreshProject: projectId) { try await self.relay.decideSecretRequest(id, decision: decision) }
        }
    }

    func setVaultAccess(vaultId: String, projectId: String, enabled: Bool, note: String? = nil) {
        if let i = projectSecrets[projectId]?.items.firstIndex(where: { $0.vault?.id == vaultId }) {
            projectSecrets[projectId]?.items[i].enabled = enabled
        }
        Task { await secretsAction(enabled ? "Turn on" : "Turn off", refreshProject: projectId) { try await self.relay.setVaultAccess(vaultId, projectId: projectId, enabled: enabled, note: note) } }
    }

    func deleteVault(_ vaultId: String) {
        secrets?.vaults.removeAll { $0.id == vaultId }
        Task { await secretsAction("Disconnect") { try await self.relay.deleteVault(vaultId) } }
    }
}
