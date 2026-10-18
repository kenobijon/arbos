// Secrets (D83, docs/spec-secrets.md): the relay's `/secrets` shapes as the app sees them.
// A Secret is a named value an agent may use but never read; an Access is one switch
// (secret × agent) with a note; Pause all is the kill switch. No value ever reaches the
// phone: the relay never returns one, and these types have no field for it.

import Foundation

/// A person the owner authorized to direct agents to use this secret (`secret.directors[]`).
struct SecretDirector: Identifiable, Equatable {
    var id: String { userId }
    var userId: String
    var handle: String
    var name: String?
    var avatarUrl: String?
    /// Constraints the agent must honour when this person directs use
    /// (e.g. "only 1k usd of credit usage per day").
    var note: String
    var createdAt: Int64

    var displayName: String {
        if let name, !name.isEmpty { return name }
        return handle.isEmpty ? userId : "@\(handle)"
    }

    static func parse(_ v: Any?) -> SecretDirector? {
        guard let m = v as? [String: Any], let userId = DocJSON.str(m["user_id"]) else { return nil }
        return SecretDirector(
            userId: userId,
            handle: (DocJSON.str(m["handle"]) ?? "").trimmingCharacters(in: CharacterSet(charactersIn: "@")),
            name: DocJSON.str(m["name"]),
            avatarUrl: DocJSON.str(m["avatar_url"]),
            note: DocJSON.str(m["note"]) ?? "",
            createdAt: DocJSON.int(m["created_at"]) ?? 0
        )
    }
}

/// One agent's switch on a secret (`secret.agents[]`, `GET /projects/{id}/secrets items[]`).
struct SecretAgentAccess: Identifiable, Equatable {
    var id: String
    var projectId: String
    var name: String
    var enabled: Bool
    var note: String
    var hosts: [String]
    /// `all` or `chat`.
    var scopeKind: String
    var scopeChatId: String?
    var lastUsedAt: Int64?
    var disabledAt: Int64?

    static func parse(_ v: Any?) -> SecretAgentAccess? {
        guard let m = v as? [String: Any], let pid = DocJSON.str(m["project_id"]) else { return nil }
        let scope = m["scope"] as? [String: Any]
        return SecretAgentAccess(
            id: DocJSON.str(m["access_id"]) ?? pid,
            projectId: pid,
            name: DocJSON.str(m["name"]) ?? pid,
            enabled: DocJSON.bool(m["enabled"]),
            note: DocJSON.str(m["note"]) ?? "",
            hosts: (m["hosts"] as? [Any])?.compactMap { $0 as? String } ?? [],
            scopeKind: DocJSON.str(scope?["kind"]) ?? "all",
            scopeChatId: DocJSON.str(scope?["chat_id"]),
            lastUsedAt: DocJSON.int(m["last_used_at"]),
            disabledAt: DocJSON.int(m["disabled_at"])
        )
    }
}

struct SecretItem: Identifiable, Equatable {
    var id: String
    var name: String
    /// `$STRIPE_KEY` in the box; derived from the name.
    var env: String
    /// `key | login | text`.
    var kind: String
    var note: String
    var hosts: [String]
    var askEveryTime: Bool
    /// "Real value in the agent's environment": agents with access get the value itself as
    /// `$STRIPE_KEY`, no card per use (D83 decision 2 as decided).
    var injectEnv: Bool
    var expiresAt: Int64?
    var version: Int
    var createdAt: Int64
    var lastUsedAt: Int64?
    /// Logins only: the username is metadata, the password is the secret.
    var username: String?
    var vaultName: String
    var agents: [SecretAgentAccess]
    /// People (besides the owner) who may direct an agent to use this secret.
    var directors: [SecretDirector]

    var kindLabel: String {
        switch kind {
        case "login": return "Login"
        case "text": return "Text"
        default: return "Key"
        }
    }

    var icon: String { kind == "login" ? "person.fill" : "key.fill" }

    static func parse(_ v: Any?) -> SecretItem? {
        guard let m = v as? [String: Any], let id = DocJSON.str(m["id"]) else { return nil }
        return SecretItem(
            id: id,
            name: DocJSON.str(m["name"]) ?? id,
            env: DocJSON.str(m["env"]) ?? DocJSON.str(m["name"]) ?? id,
            kind: DocJSON.str(m["kind"]) ?? "key",
            note: DocJSON.str(m["note"]) ?? "",
            hosts: (m["hosts"] as? [Any])?.compactMap { $0 as? String } ?? [],
            askEveryTime: DocJSON.bool(m["ask_every_time"]),
            injectEnv: DocJSON.bool(m["inject_env"]),
            expiresAt: DocJSON.int(m["expires_at"]),
            version: Int(DocJSON.int(m["version"]) ?? 1),
            createdAt: DocJSON.int(m["created_at"]) ?? 0,
            lastUsedAt: DocJSON.int(m["last_used_at"]),
            username: DocJSON.str(m["username"]),
            vaultName: DocJSON.str(m["vault_name"]) ?? "Arbos",
            agents: (m["agents"] as? [Any])?.compactMap(SecretAgentAccess.parse) ?? [],
            directors: (m["directors"] as? [Any])?.compactMap(SecretDirector.parse) ?? []
        )
    }
}

/// One audit row (`activity[]`): names and hosts only, never a value.
struct SecretActivity: Identifiable, Equatable {
    var id: Int64
    var at: Int64
    /// `created | value_replaced | access_on | access_off | requested | decided | used | refused | paused | unpaused | expired | …`
    var action: String
    var name: String
    var targetKind: String
    var projectId: String?
    var projectName: String?
    var host: String?
    var by: String
    var reason: String?
    var decision: String?

    static func parse(_ v: Any?) -> SecretActivity? {
        guard let m = v as? [String: Any], let id = DocJSON.int(m["id"]) else { return nil }
        let detail = m["detail"] as? [String: Any]
        return SecretActivity(
            id: id,
            at: DocJSON.int(m["at"]) ?? 0,
            action: DocJSON.str(m["action"]) ?? "",
            name: DocJSON.str(m["name"]) ?? DocJSON.str(m["target_name"]) ?? "",
            targetKind: DocJSON.str(m["target_kind"]) ?? "secret",
            projectId: DocJSON.str(m["project_id"]),
            projectName: DocJSON.str(m["project_name"]),
            host: DocJSON.str(m["host"]),
            by: DocJSON.str(m["by"]) ?? "",
            reason: DocJSON.str(detail?["reason"]),
            decision: DocJSON.str(detail?["decision"])
        )
    }

    /// One line for the activity card: "Research used STRIPE_KEY · api.stripe.com".
    var line: String {
        let agent = projectName ?? "An agent"
        let at = host.map { " · \($0)" } ?? ""
        switch action {
        case "used": return "\(agent) used \(name)\(at)"
        case "refused": return "Refused: \(agent) · \(name)\(at)"
        case "requested": return "\(agent) asked for \(name)\(at)"
        case "decided":
            switch decision ?? "" {
            case "denied": return "\(agent) asked for \(name). Denied."
            case "allowed_once": return "You let \(agent) use \(name) once"
            case "allowed_chat": return "You let \(agent) use \(name) in one chat"
            default: return "You let \(agent) use \(name)"
            }
        case "access_on": return by.hasPrefix("agent:") ? "\(agent) is using \(name)\(at)" : "You gave \(agent) access to \(name)"
        case "access_off": return "You turned \(name) off for \(agent)"
        case "created": return "You added \(name)"
        case "updated": return "You edited \(name)"
        case "value_replaced": return "You replaced the value of \(name)"
        case "scrubbed": return "You scrubbed \(name) from context"
        case "deleted": return "You deleted \(name)"
        case "paused": return by == "stop_machine" ? "Secrets paused with the machine" : "You paused all secrets"
        case "unpaused": return "You turned secrets back on"
        case "expired": return "\(name) expired"
        case "expiring": return "\(name) expires soon"
        case "vault_connected": return "You connected \(name)"
        case "vault_disconnected": return "You disconnected \(name)"
        case "vault_refreshed": return "\(name) refreshed"
        case "env_on": return "You put the real value of \(name) in the environment"
        case "env_off": return "You took the real value of \(name) out of the environment"
        case "injected": return "\(name) went into \(agent)'s environment"
        case "director_on": return "You let someone direct agents to use \(name)"
        case "director_off": return "You removed a director from \(name)"
        default: return "\(action.replacingOccurrences(of: "_", with: " ")) · \(name)"
        }
    }
}

/// A card the owner has not answered yet, as Settings lists it.
struct SecretRequestItem: Identifiable, Equatable {
    var id: String
    var projectId: String
    var projectName: String?
    var name: String
    var purpose: String
    var use: String
    var host: String?
    var status: String
    var createdAt: Int64

    var isPending: Bool { status == "pending" }

    static func parse(_ v: Any?) -> SecretRequestItem? {
        guard let m = v as? [String: Any], let id = DocJSON.str(m["id"]) else { return nil }
        return SecretRequestItem(
            id: id,
            projectId: DocJSON.str(m["project_id"]) ?? "",
            projectName: DocJSON.str(m["project_name"]),
            name: DocJSON.str(m["name"]) ?? "",
            purpose: DocJSON.str(m["purpose"]) ?? "",
            use: DocJSON.str(m["use"]) ?? "header",
            host: DocJSON.str(m["host"]),
            status: DocJSON.str(m["status"]) ?? "pending",
            createdAt: DocJSON.int(m["created_at"]) ?? 0
        )
    }
}

/// A connected vault (Phase 2: 1Password). Listed by `GET /secrets` as `vaults[]`.
struct ConnectedVault: Identifiable, Equatable {
    var id: String
    var provider: String
    var name: String
    var itemsCount: Int
    var tokenExpiresAt: Int64?
    /// `ok | token_invalid | rate_limited | expired`.
    var status: String
    var readsUsedHour: Int?
    var readsPerHour: Int?
    var agents: [SecretAgentAccess]
    /// Items the owner put into the agents' environment as real values.
    var envItems: Int = 0

    var providerLabel: String { provider == "onepassword" ? "1Password" : provider.capitalized }

    static func parse(_ v: Any?) -> ConnectedVault? {
        guard let m = v as? [String: Any], let id = DocJSON.str(m["id"]) else { return nil }
        let limits = m["limits"] as? [String: Any]
        return ConnectedVault(
            id: id,
            provider: DocJSON.str(m["provider"]) ?? "onepassword",
            name: DocJSON.str(m["name"]) ?? "Vault",
            itemsCount: Int(DocJSON.int(m["items_count"]) ?? 0),
            tokenExpiresAt: DocJSON.int(m["token_expires_at"]),
            status: DocJSON.str(m["status"]) ?? "ok",
            readsUsedHour: DocJSON.int(limits?["reads_used_hour"]).map(Int.init),
            readsPerHour: DocJSON.int(limits?["reads_per_hour"]).map(Int.init),
            agents: (m["agents"] as? [Any])?.compactMap(SecretAgentAccess.parse) ?? [],
            envItems: Int(DocJSON.int(m["env_items"]) ?? 0)
        )
    }
}

/// `GET /secrets`.
struct SecretsOverview: Equatable {
    var paused: Bool
    var pausedAt: Int64?
    var vaults: [ConnectedVault]
    var secrets: [SecretItem]
    var requestsPending: Int
    var activity: [SecretActivity]
    /// Vault providers this relay can connect (`["onepassword"]` once Phase 2 is deployed).
    var providers: [String] = []
    /// Cards the owner has not answered yet, for the Secrets root ("Waiting for you").
    var requests: [SecretRequestItem] = []
    var loaded = true

    static func parse(_ v: Any?) -> SecretsOverview? {
        guard let m = v as? [String: Any] else { return nil }
        return SecretsOverview(
            paused: DocJSON.bool(m["paused"]),
            pausedAt: DocJSON.int(m["paused_at"]),
            vaults: (m["vaults"] as? [Any])?.compactMap(ConnectedVault.parse) ?? [],
            secrets: (m["secrets"] as? [Any])?.compactMap(SecretItem.parse) ?? [],
            requestsPending: Int(DocJSON.int(m["requests_pending"]) ?? 0),
            activity: (m["activity"] as? [Any])?.compactMap(SecretActivity.parse) ?? [],
            providers: (m["providers"] as? [Any])?.compactMap { $0 as? String } ?? [],
            requests: (m["requests"] as? [Any])?.compactMap(SecretRequestItem.parse) ?? []
        )
    }

    /// Settings row value: `4 secrets · 1Password`, `No secrets yet`, `Paused`.
    var rowValue: String {
        if paused { return "Paused" }
        if secrets.isEmpty && vaults.isEmpty { return "No secrets yet" }
        var s = "\(secrets.count) secret\(secrets.count == 1 ? "" : "s")"
        if let v = vaults.first { s += " · \(v.providerLabel)" }
        return s
    }
}

/// One row of the agent's Secrets page (`GET /projects/{id}/secrets items[]`): an Access on a
/// secret or on a whole vault.
struct ProjectSecretAccess: Identifiable, Equatable {
    var id: String
    var enabled: Bool
    var note: String
    var hosts: [String]
    var lastUsedAt: Int64?
    var disabledAt: Int64?
    var targetKind: String
    var secret: SecretItem?
    var vault: ConnectedVault?

    /// The vault row shows the vault name alone: its lock tile and "everything in this vault" say the rest.
    var title: String { secret?.env ?? vault?.name ?? "Secret" }
    var icon: String { vault != nil ? "lock.rectangle.stack.fill" : (secret?.icon ?? "key.fill") }

    static func parse(_ v: Any?) -> ProjectSecretAccess? {
        guard let m = v as? [String: Any], let access = SecretAgentAccess.parse(m) else { return nil }
        var secret: SecretItem? = nil
        if let s = m["secret"] as? [String: Any] {
            var sm = s
            sm["agents"] = []
            secret = SecretItem.parse(sm)
        }
        return ProjectSecretAccess(
            id: access.id,
            enabled: access.enabled,
            note: access.note,
            hosts: access.hosts,
            lastUsedAt: access.lastUsedAt,
            disabledAt: access.disabledAt,
            targetKind: DocJSON.str(m["target_kind"]) ?? (m["vault"] != nil ? "vault" : "secret"),
            secret: secret,
            vault: ConnectedVault.parse(m["vault"])
        )
    }
}

struct ProjectSecrets: Equatable {
    var paused: Bool
    var owner: Bool
    var items: [ProjectSecretAccess]
    var requests: [SecretRequestItem]
    var activity: [SecretActivity]

    static func parse(_ v: Any?) -> ProjectSecrets? {
        guard let m = v as? [String: Any] else { return nil }
        return ProjectSecrets(
            paused: DocJSON.bool(m["paused"]),
            owner: DocJSON.bool(m["owner"]),
            items: (m["items"] as? [Any])?.compactMap(ProjectSecretAccess.parse) ?? [],
            requests: (m["requests"] as? [Any])?.compactMap(SecretRequestItem.parse) ?? [],
            activity: (m["activity"] as? [Any])?.compactMap(SecretActivity.parse) ?? []
        )
    }
}

enum SecretsText {
    /// The note field: what agents may do with the key. Same words on both platforms.
    static let notePlaceholder = "What may agents do with it? e.g. read-only · confirm before charging · may spend up to $20 without asking"
    static let noteHint = "Agents treat this as the rule for the key — for egress and for programs that use the real value. Say what it is for, what is allowed, and any spend limit. No spending guidance means they show the option and price and confirm before anything that charges."
    static let accessNoteHint = "Shown whenever the agent sees this secret, on top of the secret's own note. Allowed actions and spend limits for this agent, e.g. reports only · may spend up to $5 a day."
    static let vaultNoteHint = "The rule for every item in this vault, for the agents you pick: what it is for, what is allowed, any spend limit. No spending guidance means they confirm before anything that charges."

    /// The variables a secret's real value takes: "$STRIPE_KEY", or
    /// "$GITHUB_LOGIN_USERNAME and $GITHUB_LOGIN_PASSWORD" for a login.
    static func realVarList(_ s: SecretItem) -> String {
        s.kind == "login" ? "$\(s.env)_USERNAME and $\(s.env)_PASSWORD" : "$\(s.env)"
    }

    /// "used 2 min ago" · "never used" · "turned off Oct 12".
    static func usage(lastUsedAt: Int64?, disabledAt: Int64? = nil, enabled: Bool = true) -> String {
        if !enabled, let d = disabledAt { return "turned off \(shortDate(d))" }
        guard let t = lastUsedAt else { return "never used" }
        return "used \(ago(t))"
    }

    static func ago(_ ms: Int64) -> String {
        let date = Date(timeIntervalSince1970: Double(ms) / 1000)
        if Date().timeIntervalSince(date) < 60 { return "just now" }
        let f = RelativeDateTimeFormatter()
        f.unitsStyle = .short
        return f.localizedString(for: date, relativeTo: Date())
    }

    static func shortDate(_ ms: Int64) -> String {
        let date = Date(timeIntervalSince1970: Double(ms) / 1000)
        let f = DateFormatter()
        f.setLocalizedDateFormatFromTemplate(Calendar.current.isDate(date, equalTo: Date(), toGranularity: .year) ? "MMM d" : "MMM d yyyy")
        return f.string(from: date)
    }

    /// Name rules from the relay (§4): letters, digits, space, `_`, `-`, `.`; ≤ 64.
    static func envName(_ name: String) -> String {
        var s = ""
        for ch in name.trimmingCharacters(in: .whitespaces) {
            if ch.isASCII && (ch.isLetter || ch.isNumber) { s.append(ch.uppercased()) } else { s.append("_") }
        }
        while s.contains("__") { s = s.replacingOccurrences(of: "__", with: "_") }
        s = s.trimmingCharacters(in: CharacterSet(charactersIn: "_"))
        if let f = s.first, f.isNumber { s = "_" + s }
        return s
    }

    static func validName(_ name: String) -> Bool {
        let t = name.trimmingCharacters(in: .whitespaces)
        guard !t.isEmpty, t.count <= 64 else { return false }
        guard t.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "_" || $0 == "-" || $0 == "." || $0 == " ") }) else { return false }
        return !envName(t).isEmpty
    }
}
