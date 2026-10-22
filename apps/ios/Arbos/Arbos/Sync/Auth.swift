// Sign in with GitHub against the relay (self-serve). The relay runs the OAuth
// dance; the app opens `/auth/github/start?client=ios` in an
// ASWebAuthenticationSession, receives `arbos://auth/callback?code=…`, and
// exchanges the code for an access + refresh token pair kept in the Keychain.
// Endpoint paths and JSON keys live in `AuthEndpoints` so the backend's final
// shapes are a one-place change.

import AuthenticationServices
import Foundation
import UIKit

enum AuthMode: String {
    case github
    case paircode
}

struct AuthTokens: Codable, Equatable {
    var accessToken: String
    var refreshToken: String
    /// Unix ms; 0 when the relay did not say.
    var expiresAt: Int64
    var relayUrl: String
    var deviceId: String

    /// Less than five minutes left (the relay's refresh rule).
    var isExpiringSoon: Bool {
        expiresAt > 0 && expiresAt - nowMs() < AuthEndpoints.refreshAheadMs
    }
}

struct AuthUser: Codable, Equatable {
    var id: String
    var login: String
    var name: String?
    var avatarUrl: String?
    /// D79 `avatar_color_index_effective` (nil → derived from `id`).
    var avatarColorIndexEffective: Int? = nil
}

/// `GET /me` credits block.
struct Credits: Codable, Equatable {
    var balanceUsd: Double
    var grantedUsd: Double
    var spentUsd: Double
    var spentTodayUsd: Double
    var dailyCapUsd: Double?
}

/// A device waiting for this signed-in user's approval (`GET /auth/approvals`).
struct PendingDevice: Identifiable, Equatable {
    var id: String { pendingId }
    var pendingId: String
    var userCode: String
    var name: String
    var createdAt: Int64
    var expiresAt: Int64
}

/// Relay auth routes (internal/selfserve-auth-api.md, 2026-10-15).
enum AuthEndpoints {
    static let callbackScheme = "arbos"
    static let callback = "arbos://auth/callback"
    /// Relay connector OAuth success/error page redirects here so the auth sheet closes.
    static let connectorCallbackHost = "connectors"
    static func start(deviceName: String) -> String {
        let name = deviceName.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? "iPhone"
        let redirect = callback.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? callback
        return "/auth/github/start?redirect=\(redirect)&device_name=\(name)"
    }
    static let token = "/auth/token"
    static let refresh = "/auth/refresh"
    static let logout = "/auth/logout"
    static let me = "/me"
    static let approvals = "/auth/approvals"
    static let approve = "/auth/approvals/approve"
    static let deny = "/auth/approvals/deny"
    /// Refresh when less than this is left on the access token.
    static let refreshAheadMs: Int64 = 5 * 60 * 1000

    static func isConnectorCallback(_ url: URL) -> Bool {
        url.scheme == callbackScheme && url.host == connectorCallbackHost
    }
}

struct AuthError: Error, LocalizedError {
    var message: String
    var errorDescription: String? { message }

    static func humanize(_ type: String, _ message: String) -> String {
        switch type {
        case "invite_required": return "This relay is invite-only. Ask for an invite code."
        case "oauth_denied": return "GitHub sign-in was declined."
        case "oauth_failed": return "GitHub sign-in failed. Try again."
        case "handle_taken": return "That handle is already taken."
        case "bad_handle": return "Handle must be 1–39 letters, numbers, or hyphens."
        case "": return message
        default: return message.isEmpty ? type : message
        }
    }
}

/// Drives the system web sign-in sheet. Ephemeral so no consent alert is shown
/// and no cookies leak between accounts; the relay's tokens are what persist.
@MainActor
final class WebAuth: NSObject, ASWebAuthenticationPresentationContextProviding {
    private var session: ASWebAuthenticationSession?

    func signIn(startURL: URL) async throws -> String {
        let url = try await open(startURL)
        let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
        if let err = items.first(where: { $0.name == "error" })?.value {
            let message = items.first(where: { $0.name == "message" })?.value ?? err
            throw AuthError(message: AuthError.humanize(err, message))
        }
        guard let code = items.first(where: { $0.name == "code" })?.value, !code.isEmpty else {
            throw AuthError(message: "The relay did not return a sign-in code")
        }
        return code
    }

    /// Open `startURL` in the system auth sheet and wait for any `arbos://` callback
    /// (GitHub sign-in or connector OAuth return).
    func open(_ startURL: URL) async throws -> URL {
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<URL, Error>) in
            let session = ASWebAuthenticationSession(url: startURL, callbackURLScheme: AuthEndpoints.callbackScheme) { url, error in
                if let error {
                    if (error as? ASWebAuthenticationSessionError)?.code == .canceledLogin {
                        cont.resume(throwing: AuthError(message: "Sign-in cancelled"))
                    } else {
                        cont.resume(throwing: error)
                    }
                    return
                }
                guard let url else {
                    cont.resume(throwing: AuthError(message: "Sign-in produced no callback"))
                    return
                }
                cont.resume(returning: url)
            }
            session.presentationContextProvider = self
            session.prefersEphemeralWebBrowserSession = true
            self.session = session
            if !session.start() {
                cont.resume(throwing: AuthError(message: "Could not open the sign-in sheet"))
            }
        }
    }

    nonisolated func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        MainActor.assumeIsolated {
            let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            return scenes.flatMap(\.windows).first { $0.isKeyWindow } ?? scenes.first?.windows.first ?? ASPresentationAnchor()
        }
    }
}

// MARK: - Relay calls

extension RelayAPI {
    /// github | paircode. `/healthz` carries `auth_mode` on new relays; older
    /// relays (no field) are paircode.
    func authMode() async -> AuthMode {
        guard let v = try? await json("GET", "/healthz", auth: false) as? [String: Any] else { return .paircode }
        if let raw = v["auth_mode"] as? String, let mode = AuthMode(rawValue: raw) { return mode }
        return v["auth"] as? String == "github" ? .github : .paircode
    }

    func authStartURL(deviceName: String) -> URL? {
        URL(string: endpoint.httpUrl + AuthEndpoints.start(deviceName: deviceName))
    }

    /// `POST /auth/token {grant_code}` → tokens + user.
    func exchange(grantCode: String) async throws -> (AuthTokens, AuthUser?) {
        let v = try await json("POST", AuthEndpoints.token, body: ["grant_code": grantCode], auth: false)
        return try RelayAPI.parseTokens(v, relayUrl: endpoint.httpUrl)
    }

    /// `POST /auth/refresh {refresh_token}` → rotated pair (the old refresh token dies).
    func refresh(_ tokens: AuthTokens) async throws -> (AuthTokens, AuthUser?) {
        let v = try await json("POST", AuthEndpoints.refresh, body: ["refresh_token": tokens.refreshToken], auth: false)
        return try RelayAPI.parseTokens(v, relayUrl: endpoint.httpUrl)
    }

    /// `GET /me` → user, credits, machine summary.
    func me() async throws -> (AuthUser, Credits?, [String: Any]) {
        guard let m = try await json("GET", AuthEndpoints.me) as? [String: Any] else { throw AuthError(message: "Bad /me response") }
        let user = RelayAPI.parseUser(m["user"] as? [String: Any]) ?? AuthUser(id: "", login: "github")
        var credits: Credits? = nil
        if let c = m["credits"] as? [String: Any] {
            let n = { (k: String) -> Double in (c[k] as? NSNumber)?.doubleValue ?? 0 }
            credits = Credits(balanceUsd: n("balance_usd"), grantedUsd: n("granted_usd"), spentUsd: n("spent_usd"),
                              spentTodayUsd: n("spent_today_usd"), dailyCapUsd: (c["daily_cap_usd"] as? NSNumber)?.doubleValue)
        }
        var summary = m["machine"] as? [String: Any] ?? [:]
        // Remote feature flags ride along with the machine summary (one tuple, no new callers).
        if let features = m["features"] as? [String: Any] { summary["features"] = features }
        return (user, credits, summary)
    }

    /// `PATCH /me {name?, login?}` → updated user (display name and/or handle).
    func patchMe(name: String?, login: String?) async throws -> AuthUser {
        var body: [String: Any] = [:]
        if let name { body["name"] = name }
        if let login { body["login"] = login }
        guard let m = try await json("PATCH", AuthEndpoints.me, body: body) as? [String: Any],
              let user = RelayAPI.parseUser(m["user"] as? [String: Any]) else {
            throw AuthError(message: "Could not update profile")
        }
        return user
    }

    /// `DELETE /me/avatar`: back to the GitHub picture; then `GET /me` for the fresh user.
    func deleteMeAvatar() async throws -> AuthUser {
        _ = try await request("DELETE", "/me/avatar")
        return try await me().0
    }

    func approvals() async throws -> [PendingDevice] {
        let v = try await json("GET", AuthEndpoints.approvals)
        let list = (v as? [Any]) ?? ((v as? [String: Any])?["approvals"] as? [Any]) ?? []
        return list.compactMap { raw -> PendingDevice? in
            guard let d = raw as? [String: Any], let pid = d["pending_id"] as? String else { return nil }
            return PendingDevice(pendingId: pid, userCode: d["user_code"] as? String ?? "", name: d["name"] as? String ?? "New device",
                                 createdAt: (d["created_at"] as? NSNumber)?.int64Value ?? 0,
                                 expiresAt: (d["expires_at"] as? NSNumber)?.int64Value ?? 0)
        }
    }

    func approve(userCode: String) async throws {
        _ = try await json("POST", AuthEndpoints.approve, body: ["user_code": userCode])
    }

    func deny(userCode: String) async throws {
        _ = try await json("POST", AuthEndpoints.deny, body: ["user_code": userCode])
    }

    func logout() async {
        _ = try? await json("POST", AuthEndpoints.logout, body: [String: Any]())
    }

    static func parseTokens(_ v: Any, relayUrl: String) throws -> (AuthTokens, AuthUser?) {
        guard let m = v as? [String: Any], let access = m["access_token"] as? String, !access.isEmpty else {
            throw AuthError(message: "The relay returned no access token")
        }
        let refresh = m["refresh_token"] as? String ?? ""
        var expiresAt: Int64 = 0
        if let s = (m["expires_in"] as? NSNumber)?.int64Value { expiresAt = nowMs() + s * 1000 }
        return (AuthTokens(accessToken: access, refreshToken: refresh, expiresAt: expiresAt, relayUrl: relayUrl,
                           deviceId: m["device_id"] as? String ?? ""), parseUser(m["user"] as? [String: Any]))
    }

    static func parseUser(_ u: [String: Any]?) -> AuthUser? {
        guard let u, let login = u["login"] as? String else { return nil }
        return AuthUser(id: u["id"] as? String ?? "", login: login, name: u["name"] as? String, avatarUrl: u["avatar_url"] as? String)
    }
}

// MARK: - Keychain

extension Keychain {
    private static func authAccount(_ relayUrl: String) -> String { "auth:" + relayUrl }

    static func loadAuth(relayUrl: String) -> AuthTokens? {
        loadData(account: authAccount(relayUrl)).flatMap { try? JSONDecoder().decode(AuthTokens.self, from: $0) }
    }

    static func storeAuth(_ tokens: AuthTokens) throws {
        try storeData(try JSONEncoder().encode(tokens), account: authAccount(tokens.relayUrl))
    }

    static func deleteAuth(relayUrl: String) {
        deleteData(account: authAccount(relayUrl))
    }
}
