// App state: the relay registry, one Loro replica per open room, and the
// view of them the screens read. Everything runs on the main actor; Loro
// frames are ~130 bytes every 120 ms, so there is nothing to move off it.

import Foundation
import Combine
import SwiftUI
import UIKit
import os

enum Appearance: String, CaseIterable, Identifiable {
    case system, dark, light
    var id: String { rawValue }
    var label: String {
        switch self {
        case .system: return "System"
        case .dark: return "Dark"
        case .light: return "Light"
        }
    }
}

enum Surface: String, CaseIterable, Identifiable {
    case matte, glass
    var id: String { rawValue }
    var label: String {
        switch self {
        case .matte: return "Matte"
        case .glass: return "Glass"
        }
    }
}

struct Notice: Identifiable, Equatable {
    let id = UUID()
    var text: String
}

/// Last successful home registry, kept in the on-device store so reopening the
/// app paints the agent list before the relay round-trip finishes.
private struct RegistryCache: Codable {
    var projects: [Project]
    var chats: [Id: [Chat]]
}

/// Keys in the store's `kv` table.
private enum StoreKey {
    static let registry = "registry"
    static let people = "people"
    static let me = "me"
    static let unread = "unread"
    static let lastChat = "lastChat"
}

private let pushLog = Logger(subsystem: "com.unarbos.arbos.ios", category: "pushes")

@MainActor
final class AppModel: ObservableObject {
    /// The hosted relay; self-hosters change it under Advanced.
    static let defaultRelayUrl = "https://relay.arbos.life"

    @Published var appearanceSetting: String = UserDefaults.standard.string(forKey: "appearance") ?? Appearance.system.rawValue {
        didSet { UserDefaults.standard.set(appearanceSetting, forKey: "appearance") }
    }
    /// Glass is the iPhone default (D10 amendment, 2026-10-17: Grok's header
    /// blurs the transcript under it). The key is only written when the user
    /// picks a surface in Settings, so an absent key means "never chose" → Glass,
    /// and an explicit "matte" stays Matte.
    @Published var surfaceSetting: String = UserDefaults.standard.string(forKey: "surface") ?? Surface.glass.rawValue {
        didSet { UserDefaults.standard.set(surfaceSetting, forKey: "surface") }
    }

    @Published private(set) var credentials: DeviceCredentials?
    /// Self-serve sign-in (GitHub). `tokens` is the JWT pair; `credentials`
    /// mirrors its access token so every existing call path keeps working.
    @Published private(set) var tokens: AuthTokens?
    @Published private(set) var user: AuthUser?
    @Published private(set) var credits: Credits?
    @Published private(set) var approvals: [PendingDevice] = []
    /// People chats (D72): DMs I am a member of, newest activity first.
    @Published var peopleChats: [PeopleChat] = []
    /// Chat ids with unread rows (per device, from head_seq > read_seq and pushes).
    @Published var peopleUnread: Set<String> = []
    @Published private(set) var blocked: [Person] = []
    /// Every Arbos user the relay knows (members + invited), for the New Message sheet.
    @Published var directory: [Person] = []
    private var directoryLoadedAt: Int64 = 0
    /// The chat currently on screen; pushes for it never notify while the
    /// scene is active (Mac: `window_active`). Background / lock clears focus
    /// so a reply still pushes a banner.
    var visibleChatId: String? {
        didSet {
            guard visibleChatId != oldValue else { return }
            if let old = oldValue { rooms[old]?.focused = false }
            if let new = visibleChatId {
                rooms[new]?.focused = appActive
                PushManager.shared.clearDelivered(chatId: new)
            }
        }
    }
    /// Scene is `.active`. Focus and in-app banner suppression only apply then.
    private var appActive = true

    /// Called from the root scene: leaving `.active` drops `focused_chat` on
    /// the relay (and stops swallowing banners) so APNs can fire.
    func setAppActive(_ active: Bool) {
        guard appActive != active else { return }
        appActive = active
        if let id = visibleChatId { rooms[id]?.focused = active }
        // Leaving the foreground is the last safe moment to write before a kill.
        if !active { flushStore(all: true) }
    }

    /// Chat on screen *and* the app is in the foreground — the only case where
    /// a message notification is suppressed.
    private func isWatching(_ chatId: String) -> Bool {
        appActive && visibleChatId == chatId
    }
    /// Per-chat unread counts from `GET /me/unread` (empty on relays without it).
    @Published private(set) var unreadCounts: [String: Int] = [:]
    /// A chat a notification asked for before its list had loaded.
    private var pendingOpen: (chatId: String, projectId: String)?
    private var notifiedPushIds: Set<String> = []
    @Published private(set) var authMode: AuthMode = .github
    @Published private(set) var signingIn = false
    @Published private(set) var signInError: String?
    private let webAuth = WebAuth()
    private var refreshTask: Task<Bool, Never>?
    @Published var projects: [Project] = [] {
        didSet {
            if projects.map(\.id) != oldValue.map(\.id) || projects.map(\.avatar) != oldValue.map(\.avatar) { recomputeAvatars() }
        }
    }
    @Published var chats: [Id: [Chat]] = [:]
    @Published var messages: [Id: [Message]] = [:]
    @Published var caughtUp: Set<Id> = []
    @Published var link: Link = .connecting
    @Published private(set) var models: [String] = []
    @Published var machine: Machine = .unknown
    @Published private(set) var machineBusy = false
    /// `machine.always_on` as last reported by `GET /me` (nil = field absent).
    private var meAlwaysOn: Bool?
    /// Navigation: home → chat. Shared so menus anywhere can open a chat.
    @Published var path: [Route] = [] {
        didSet {
            // The open chat is loaded first next launch and evicted last.
            if case .chat(let chatId, _)? = path.last, let store {
                store.touchDoc(chatId)
                store.put(StoreKey.lastChat, Data(chatId.utf8))
            }
        }
    }
    /// True when the Debug fixture replaced the relay (screenshots).
    var isFixture = false
    /// Debug demo tours run once per process, whichever view instance starts them.
    var demoStarted = false

    enum Route: Hashable {
        case chat(chatId: Id, projectId: Id)
    }

    /// Who the call screen shows before anyone joins: the DM peer, or (for a
    /// people group) every other human plus the agents in that chat; for a
    /// project chat the project's agent (Calls Phase 2).
    func expectedCallPeople(_ c: Call) -> [CallPerson] {
        guard let chat = peopleChat(c.chatId) else {
            if !c.agents.isEmpty { return c.agents }
            guard let project = project(c.projectId) else { return [] }
            return [CallPerson(id: project.id, kind: "agent", name: project.name, handle: mentionSlug(project.name),
                               avatar: project.avatar, avatarColor: project.avatarColor, avatarUrl: nil)]
        }
        let me = user?.id
        if chat.isGroup {
            return chat.members.filter { $0.id != me }.map(CallPerson.from(member:))
        }
        guard let other = chat.counterpart(me: me) else { return [] }
        return [CallPerson.from(member: other)]
    }

    /// Is this chat the one on screen (top of the navigation path)?
    func isShowing(chatId: Id) -> Bool {
        if case .chat(let id, _)? = path.last { return id == chatId }
        return false
    }

    func open(chat: Chat) {
        unarchive(chat.projectId)
        rememberOpened(chat)
        path = [.chat(chatId: chat.id, projectId: chat.projectId)]
    }

    func open(project: Project) {
        unarchive(project.id)
        if let chat = landingChat(of: project.id) {
            open(chat: chat)
        } else {
            Task { if let c = await newChat(projectId: project.id) { open(chat: c) } }
        }
    }

    // MARK: Home archive (device-local)

    /// Agents hidden from the home list until found in search (or opened).
    @Published private(set) var archivedProjectIds: Set<Id> = {
        Set(UserDefaults.standard.stringArray(forKey: "archivedProjects") ?? [])
    }()

    func isArchived(_ projectId: Id) -> Bool { archivedProjectIds.contains(projectId) }

    func archive(_ projectId: Id) {
        guard !archivedProjectIds.contains(projectId) else { return }
        archivedProjectIds.insert(projectId)
        UserDefaults.standard.set(Array(archivedProjectIds), forKey: "archivedProjects")
        // Archive clears a home pin — the row is hidden until search/open.
        unpinHome(projectId: projectId)
    }

    func unarchive(_ projectId: Id) {
        guard archivedProjectIds.contains(projectId) else { return }
        archivedProjectIds.remove(projectId)
        UserDefaults.standard.set(Array(archivedProjectIds), forKey: "archivedProjects")
    }

    // MARK: Home pins (device-local)

    /// Home-list keys (`p:<projectId>` / `c:<peopleChatId>`) kept at the top.
    @Published private(set) var pinnedHomeIds: Set<String> = {
        Set(UserDefaults.standard.stringArray(forKey: "pinnedHome") ?? [])
    }()

    func isHomePinned(projectId: Id) -> Bool { pinnedHomeIds.contains("p:" + projectId) }
    func isHomePinned(peopleChatId: Id) -> Bool { pinnedHomeIds.contains("c:" + peopleChatId) }

    func pinHome(projectId: Id) {
        unarchive(projectId)
        setHomePinned("p:" + projectId, true)
    }

    func unpinHome(projectId: Id) { setHomePinned("p:" + projectId, false) }

    func pinHome(peopleChatId: Id) { setHomePinned("c:" + peopleChatId, true) }
    func unpinHome(peopleChatId: Id) { setHomePinned("c:" + peopleChatId, false) }

    private func setHomePinned(_ key: String, _ pinned: Bool) {
        if pinned {
            guard !pinnedHomeIds.contains(key) else { return }
            pinnedHomeIds.insert(key)
        } else {
            guard pinnedHomeIds.contains(key) else { return }
            pinnedHomeIds.remove(key)
        }
        UserDefaults.standard.set(Array(pinnedHomeIds), forKey: "pinnedHome")
    }

    @Published private(set) var machineError: String?
    @Published var notice: Notice?
    @Published private(set) var pairingBusy = false
    @Published private(set) var pairingError: String?
    @Published private(set) var registryError: String?
    /// Settings → Connectors (Google Gmail/Calendar, …). Empty until first refresh.
    @Published private(set) var connectors = ConnectorsView()
    @Published private(set) var connectorsBusy = false

    private var api: RelayAPI
    private var docs: [Id: ChatDoc] = [:]
    private var rooms: [Id: Room] = [:]
    /// Client-minted message ids whose push the relay has not `ACK`ed (S3),
    /// keyed by chat id so one room's ACK cannot leave another chat's bubble faint.
    private var unacked: [Id: Set<Id>] = [:]
    private var registryTask: Task<Void, Never>?

    // MARK: On-device store (#929)

    /// SQLite under Application Support, one file per relay + device; nil in
    /// the fixture, before sign-in, or with `ARBOS_NO_STORE=1` (launch timing A/B).
    private var store: DeviceStore?
    /// Rooms whose doc or cursor changed since the last save.
    private var dirtyDocs: Set<Id> = []
    private var flushTask: Task<Void, Never>?
    /// Where the first populated home list came from: "store" or "relay" (launch log).
    private(set) var contentOrigin = "relay"
    /// Keys the store loader has handed to `ensureRoom` (or decided to skip).
    private var storeDocsLoaded = false
    @Published private(set) var storeUsage = DeviceStore.Usage()
    /// Set when the first home list came from disk and the relay is not live yet.
    var showingSavedCopy: Bool { contentOrigin == "store" && link != .live }
    private static let flushDelayNs: UInt64 = 1_500_000_000

    var appearance: Appearance { Appearance(rawValue: appearanceSetting) ?? .system }
    var surface: Surface { Surface(rawValue: surfaceSetting) ?? .glass }
    var isPaired: Bool { credentials != nil }
    var relayUrl: String { api.endpoint.httpUrl }

    init() {
        LaunchClock.mark("app-init")
        var saved = UserDefaults.standard.string(forKey: "relayUrl") ?? AppModel.defaultRelayUrl
        var creds: DeviceCredentials?
        #if DEBUG
        // Simulator runs: `SIMCTL_CHILD_ARBOS_RELAY_URL` / `_TOKEN` skip the pairing screen.
        let env = ProcessInfo.processInfo.environment
        if let url = env["ARBOS_RELAY_URL"], !url.isEmpty { saved = url }
        if let tok = env["ARBOS_RELAY_TOKEN"], !tok.isEmpty {
            creds = DeviceCredentials(relayUrl: RelayEndpoint(saved).httpUrl, deviceId: env["ARBOS_DEVICE_ID"] ?? "ios:env", deviceToken: tok)
        }
        #endif
        #if DEBUG
        if env["ARBOS_FIXTURE"] == "1" {
            api = RelayAPI(endpoint: RelayEndpoint("http://fixture.invalid")) { "" }
            credentials = DeviceCredentials(relayUrl: "http://fixture.invalid", deviceId: "ios:fixture", deviceToken: "fixture")
            loadFixture()
            return
        }
        #endif
        let endpoint = RelayEndpoint(saved)
        if !endpoint.httpUrl.isEmpty {
            UserDefaults.standard.set(endpoint.httpUrl, forKey: "relayUrl")
        }
        api = RelayAPI(endpoint: endpoint) { "" }
        if creds == nil, let saved = Keychain.loadAuth(relayUrl: endpoint.httpUrl) {
            tokens = saved
            creds = DeviceCredentials(relayUrl: saved.relayUrl, deviceId: saved.deviceId, deviceToken: saved.accessToken)
        }
        if creds == nil { creds = Keychain.load(relayUrl: endpoint.httpUrl) }
        credentials = creds
        api.token = { [weak self] in self?.credentials?.deviceToken ?? "" }
        api.refresher = { [weak self] in await self?.refreshTokens() ?? false }
        if creds != nil {
            startAfterPairing()
        }
        Task { await probeAuthMode() }
        PushManager.shared.visibleChatId = { [weak self] in
            guard let self, self.appActive else { return nil }
            return self.visibleChatId
        }
        PushManager.shared.isChatArchived = { [weak self] id in self?.isChatArchived(id) ?? false }
        NotificationCenter.default.addObserver(forName: PushManager.openChat, object: nil, queue: .main) { [weak self] n in
            guard let self, let cid = n.userInfo?["chat_id"] as? String else { return }
            let pid = (n.userInfo?["project_id"] as? String) ?? peopleSlot
            Task { @MainActor in self.openFromNotification(chatId: cid, projectId: pid) }
        }
        NotificationCenter.default.addObserver(forName: PushManager.remoteReceived, object: nil, queue: .main) { [weak self] n in
            // content-available on ARBOS_CALL wakes a backgrounded phone; report
            // CallKit here so it rings without the user opening the app first.
            let info = n.userInfo ?? [:]
            Task { @MainActor in
                let aps = info["aps"] as? [String: Any]
                let category = (aps?["category"] as? String) ?? (info["category"] as? String)
                let event = info["event"] as? String
                if category == PushManager.callCategory || event == "started" || event == "ended" || event == "missed" {
                    CallManager.shared.handleCallAlert(info)
                }
                await self?.refreshPeople(); await self?.refreshUnread()
            }
        }
    }

    /// Deep link from a notification: the people chat or the project chat,
    /// or remembered until the lists arrive.
    func openFromNotification(chatId: String, projectId: String) {
        if peopleChat(chatId) != nil || projectId == peopleSlot && peopleChats.contains(where: { $0.id == chatId }) {
            path = [.chat(chatId: chatId, projectId: peopleSlot)]
            pendingOpen = nil
        } else if let c = chat(chatId) {
            path = [.chat(chatId: c.id, projectId: c.projectId)]
            pendingOpen = nil
        } else if projectId != peopleSlot, project(projectId) != nil {
            path = [.chat(chatId: chatId, projectId: projectId)]
            pendingOpen = nil
        } else {
            pendingOpen = (chatId, projectId)
        }
    }

    private func retryPendingOpen() {
        guard let p = pendingOpen else { return }
        openFromNotification(chatId: p.chatId, projectId: p.projectId)
    }

    // MARK: Feedback

    /// JPEG the screenshot (≤15 MB per the contract) and post it with the note.
    func sendFeedback(note: String, image: UIImage?, projectId: Id?) async throws -> String? {
        var data: Data? = nil
        var mime: String? = nil
        var w = 0, h = 0
        if let image {
            var quality: CGFloat = 0.9
            var jpeg = image.jpegData(compressionQuality: quality)
            while let d = jpeg, d.count > 14_000_000, quality > 0.3 {
                quality -= 0.2
                jpeg = image.jpegData(compressionQuality: quality)
            }
            data = jpeg
            mime = "image/jpeg"
            w = Int(image.size.width * image.scale)
            h = Int(image.size.height * image.scale)
        }
        if isFixture {
            try? await Task.sleep(nanoseconds: 900_000_000)
            return "01FIXTURE0000000000FEEDBK"
        }
        return try await api.sendFeedback(note: note, image: data, mime: mime, width: w, height: h,
                                          appVersion: FeedbackCenter.appVersion, os: "iOS \(UIDevice.current.systemVersion)", projectId: projectId)
    }

    // MARK: Calls v1

    private func hookCalls() {
        let calls = CallManager.shared
        calls.api = api
        calls.isFixture = isFixture
        calls.deviceId = { [weak self] in self?.deviceId ?? "" }
        calls.me = { [weak self] in
            guard let self, let u = self.user else { return nil }
            return CallPerson(id: u.id, kind: "user", name: u.name, handle: u.login, avatar: nil, avatarColor: nil, avatarUrl: u.avatarUrl)
        }
        calls.writeSystemLine = { [weak self] chatId, projectId, text, callId in
            self?.writeSystemLine(chatId: chatId, projectId: projectId, text: text, callId: callId)
        }
        calls.calleePresence = { [weak self] chatId in
            guard let self, let chat = self.peopleChat(chatId), chat.kind == "dm", let other = chat.counterpart(me: self.user?.id) else { return nil }
            let seen = max(other.lastSeenAt ?? 0, self.lastSeen[other.id] ?? 0)
            return (other.online || (nowMs() - seen) < 90_000, seen > 0 ? seen : nil)
        }
        calls.calleeName = { [weak self] chatId in
            guard let self, let chat = self.peopleChat(chatId) else { return nil }
            // Groups name the chat, not the first other human (that looked like a DM to Kenobi).
            if chat.isGroup { return chat.title.isEmpty ? "Group" : chat.title }
            return chat.counterpart(me: self.user?.id)?.displayName
        }
        // Presence carries `in_call` for the chat we are talking in.
        callSink = calls.$call.receive(on: DispatchQueue.main).sink { [weak self] c in
            guard let self else { return }
            for (key, room) in self.rooms { room.inCall = (c?.chatId == key) ? c?.callId : nil }
        }
        guard !isFixture else { return }
        calls.prepareVoip()
        Task { await calls.sendVoipTokenIfNeeded(force: true) }
    }
    private var callSink: AnyCancellable?

    /// A system row in a chat doc (call start/end lines). Fixture: local append.
    func writeSystemLine(chatId: Id, projectId: Id, text: String, callId: String) {
        #if DEBUG
        if isFixture {
            let m = Message(id: newId(), chatId: chatId, role: .system, sender: "system", status: .done, turn: nil, decision: nil,
                            subagentId: nil, subagentName: nil, inReplyTo: nil, usage: [:], model: nil, createdAt: nowMs(),
                            completedAt: nowMs(), pending: false, parts: [.text(id: newId(), text: text)], hiddenCount: 0)
            messages[chatId, default: []].append(m)
            return
        }
        #endif
        guard let doc = docs[chatId] else { return }
        do {
            try doc.insertSystemLine(id: newId(), text: text, deviceId: deviceId, callId: callId)
            doc.commit()
        } catch {
            notice = Notice(text: "Could not write the call line: \(error.localizedDescription)")
        }
    }

    /// Opening a chat: is a call on there? (GET /chats/{id}/call)
    func refreshActiveCall(chatId: Id, projectId: Id?) async {
        guard credentials != nil, !isFixture else { return }
        if let c = try? await api.activeCall(chatId: chatId, projectId: projectId) {
            CallManager.shared.noteActive(c, chatId: chatId)
        } else {
            CallManager.shared.noteActive(nil, chatId: chatId)
        }
    }

    /// A peer's presence says `in_call`: fetch it for the Join bar.
    private func peerInCall(_ callId: String, chatId: Id) {
        guard CallManager.shared.activeByChat[chatId]?.callId != callId else { return }
        Task {
            if let c = try? await api.call(callId) {
                CallManager.shared.noteActive(c, chatId: chatId)
                #if DEBUG
                if DebugHooks.autoJoinCall, !c.ended, !CallManager.shared.isActive, visibleChatId == chatId {
                    await CallManager.shared.join(c, title: peopleChat(chatId)?.title ?? "Call")
                }
                #endif
            }
        }
    }

    // MARK: Push registration + badge

    private func hookPush() {
        hookCalls()
        guard let creds = credentials, !isFixture else { return }
        let api = self.api
        PushManager.shared.register = { token, env in
            let version = (Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "") + " (" + (Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "") + ")"
            do {
                let enabled = try await api.registerPush(deviceId: creds.deviceId, token: token, environment: env,
                                                         bundleId: Bundle.main.bundleIdentifier ?? "com.unarbos.arbos.ios", appVersion: version)
                await MainActor.run { PushManager.shared.relayPushEnabled = enabled }
            } catch {
                #if DEBUG
                print("push: register failed: \(error.localizedDescription)")
                #endif
                await MainActor.run { PushManager.shared.forgetRegistration() }
            }
        }
        Task { await PushManager.shared.refreshStatus(); await PushManager.shared.sendTokenIfNeeded(force: true) }
    }

    /// `GET /me/unread` when the relay has it; otherwise the local counts.
    func refreshUnread() async {
        guard credentials != nil, !isFixture else { applyBadge(); return }
        if let r = (try? await api.unread()) ?? nil {
            unreadCounts = r.chats
            saveUnreadCache()
        }
        applyBadge()
    }

    /// Everything unread: people chats by sequence, project chats by flag.
    var unreadTotal: Int {
        // Archived agent chats never badge (§9c).
        if !unreadCounts.isEmpty {
            var total = unreadCounts.reduce(0) { $0 + (isChatArchived($1.key) ? 0 : $1.value) }
            // Live Loro rows can mark unread before /me/unread has the event.
            for id in peopleUnread where !isChatArchived(id) && (unreadCounts[id] ?? 0) == 0 {
                total += 1
            }
            for list in chats.values {
                for c in list where c.unread && !isChatArchived(c.id) && (unreadCounts[c.id] ?? 0) == 0 {
                    total += 1
                }
            }
            return total
        }
        let people = peopleChats.reduce(0) { $0 + unreadCount(for: $1) }
        let projects = chats.values.flatMap { $0 }.filter { $0.unread && !isChatArchived($0.id) }.count
        return people + projects
    }

    /// Unread rows of one agent chat for its dot: none while archived.
    func chatHasUnread(_ chat: Chat) -> Bool {
        if isChatArchived(chat.id) { return false }
        if chat.unread { return true }
        return (unreadCounts[chat.id] ?? 0) > 0 && chat.id != visibleChatId
    }

    func unreadCount(for chat: PeopleChat) -> Int {
        // On screen: never show a stale remote count (notify can lag behind Loro).
        if chat.id == visibleChatId { return 0 }
        let bySeq = Int(max(0, chat.headSeq - chat.readSeq))
        let local = bySeq > 0 ? bySeq : (peopleUnread.contains(chat.id) ? 1 : 0)
        // Loro can light peopleUnread before GET /me/unread catches up — never let a
        // stale remote 0 hide the blue circle / count.
        if let n = unreadCounts[chat.id] { return max(n, local) }
        return local
    }

    func applyBadge() {
        PushManager.shared.setBadge(unreadTotal)
    }

    // MARK: Sign in with GitHub (self-serve relay)

    func probeAuthMode() async {
        authMode = await api.authMode()
    }

    func signInWithGitHub() {
        guard !signingIn else { return }
        signingIn = true
        signInError = nil
        Task {
            defer { signingIn = false }
            do {
                guard let start = api.authStartURL(deviceName: UIDevice.current.name) else {
                    throw AuthError(message: "Bad relay address")
                }
                let code = try await webAuth.signIn(startURL: start)
                let (toks, who) = try await api.exchange(grantCode: code)
                adopt(tokens: toks, user: who)
            } catch {
                signInError = error.localizedDescription
            }
        }
    }

    private func adopt(tokens toks: AuthTokens, user who: AuthUser?) {
        tokens = toks
        if let who { user = who }
        do { try Keychain.storeAuth(toks) } catch {
            notice = Notice(text: "Session kept in memory only: \(error.localizedDescription)")
        }
        let wasPaired = credentials != nil
        credentials = DeviceCredentials(relayUrl: toks.relayUrl, deviceId: toks.deviceId, deviceToken: toks.accessToken)
        if !wasPaired { startAfterPairing() }
    }

    /// Single-flight refresh; false when the relay rejected the refresh token
    /// (the device is then signed out — a reused rotated token revokes it).
    func refreshTokens() async -> Bool {
        if let running = refreshTask { return await running.value }
        guard let current = tokens, !current.refreshToken.isEmpty else { return false }
        let task = Task<Bool, Never> { [weak self] in
            guard let self else { return false }
            do {
                let (next, who) = try await api.refresh(current)
                adopt(tokens: next, user: who)
                return true
            } catch {
                if let e = error as? RelayError, e.status == 401 || e.status == 403 {
                    signOut(remote: false)
                    signInError = "Your session ended. Sign in again."
                }
                return false
            }
        }
        refreshTask = task
        let ok = await task.value
        refreshTask = nil
        return ok
    }

    /// `/me` + pending approvals; runs with every registry poll.
    func refreshAccount() async {
        guard credentials != nil, !isFixture else { return }
        if tokens?.isExpiringSoon == true { _ = await refreshTokens() }
        if let (who, cr, summary) = try? await api.me() {
            user = who
            credits = cr
            saveAccountCache()
            if let on = (summary["always_on"] as? NSNumber)?.boolValue {
                meAlwaysOn = on
                if let held = (summary["held"] as? NSNumber)?.boolValue { machine.held = held }
                if on, !machine.alwaysOn {
                    machine.alwaysOn = true
                    machine.state = machine.coerced(machine.state)
                }
            }
        }
        if let list = try? await api.approvals() { approvals = list }
    }

    func approve(_ device: PendingDevice) {
        approvals.removeAll { $0.id == device.id }
        Task {
            do { try await api.approve(userCode: device.userCode) } catch {
                notice = Notice(text: "Approve failed: \(error.localizedDescription)")
            }
        }
    }

    func deny(_ device: PendingDevice) {
        approvals.removeAll { $0.id == device.id }
        Task { try? await api.deny(userCode: device.userCode) }
    }

    func signOut(remote: Bool = true) {
        let had = tokens
        if remote, let creds = credentials {
            let api = self.api
            Task { try? await api.unregisterPush(deviceId: creds.deviceId); try? await api.unregisterVoip(deviceId: creds.deviceId) }
        }
        CallManager.shared.forgetVoipRegistration()
        PushManager.shared.forgetRegistration()
        PushManager.shared.setBadge(0)
        if remote, had != nil { Task { await api.logout() } }
        if let had { Keychain.deleteAuth(relayUrl: had.relayUrl) }
        tokens = nil
        user = nil
        credits = nil
        approvals = []
        unpair(forget: true)
    }

    var deviceId: String { credentials?.deviceId ?? "ios:unpaired" }

    // MARK: Pairing

    func setRelayUrl(_ raw: String) {
        let endpoint = RelayEndpoint(raw)
        UserDefaults.standard.set(endpoint.httpUrl, forKey: "relayUrl")
        guard endpoint != api.endpoint else { return }
        let previous = credentials
        unpair(forget: false)
        api = RelayAPI(endpoint: endpoint) { [weak self] in self?.credentials?.deviceToken ?? "" }
        api.refresher = { [weak self] in await self?.refreshTokens() ?? false }
        tokens = nil
        user = nil
        credits = nil
        Task { await probeAuthMode() }
        if let saved = Keychain.loadAuth(relayUrl: endpoint.httpUrl) {
            tokens = saved
            credentials = DeviceCredentials(relayUrl: saved.relayUrl, deviceId: saved.deviceId, deviceToken: saved.accessToken)
        } else {
            credentials = Keychain.load(relayUrl: endpoint.httpUrl)
        }
        if credentials != nil {
            startAfterPairing()
        } else if let previous, tokens == nil, previous.deviceToken.hasPrefix("adv_") {
            // A quick-tunnel URL changes while the relay (and this device's
            // token) stays the same: try the old token on the new address
            // before asking for a pairing code.
            Task { await adopt(previous, at: endpoint) }
        }
    }

    private func adopt(_ old: DeviceCredentials, at endpoint: RelayEndpoint) async {
        let probe = RelayAPI(endpoint: endpoint) { old.deviceToken }
        guard (try? await probe.registry()) != nil else { return }
        let creds = DeviceCredentials(relayUrl: endpoint.httpUrl, deviceId: old.deviceId, deviceToken: old.deviceToken)
        try? Keychain.store(creds)
        credentials = creds
        startAfterPairing()
    }

    func pair(code: String) {
        guard !pairingBusy else { return }
        pairingBusy = true
        pairingError = nil
        Task {
            defer { pairingBusy = false }
            do {
                let creds = try await api.pair(code: code, name: UIDevice.current.name)
                do {
                    try Keychain.store(creds)
                } catch {
                    notice = Notice(text: "Token kept in memory only: \(error.localizedDescription)")
                }
                credentials = creds
                startAfterPairing()
            } catch {
                pairingError = (error as? RelayError)?.isUnauthorized == true
                    ? "That code was not accepted. Codes expire after 10 minutes."
                    : error.localizedDescription
            }
        }
    }

    func unpair(forget: Bool = true) {
        registryTask?.cancel()
        registryTask = nil
        for room in rooms.values { room.stop() }
        rooms.removeAll()
        docs.removeAll()
        projects = []
        chats = [:]
        messages = [:]
        caughtUp = []
        unacked = [:]
        peopleChats = []
        peopleUnread = []
        unreadCounts = [:]
        dirtyDocs = []
        flushTask?.cancel()
        flushTask = nil
        storeDocsLoaded = false
        // Sign-out wipes everything this account had on the device.
        store?.close(wipe: true)
        store = nil
        storeUsage = DeviceStore.Usage()
        if let creds = credentials {
            UserDefaults.standard.removeObject(forKey: "registryCache.\(creds.relayUrl).\(creds.deviceId)")
            if forget { Keychain.delete(relayUrl: creds.relayUrl) }
        }
        credentials = nil
        link = .connecting
        connectors = ConnectorsView()
        connectorsBusy = false
    }

    private func startAfterPairing() {
        link = .connecting
        openStore()
        // Paint the last home list and every saved transcript before any network.
        if let store, projects.isEmpty, peopleChats.isEmpty {
            loadStoreKV(store)
        }
        hookPush()
        Task {
            if let ids = try? await api.models(), !ids.isEmpty {
                models = ids
            }
        }
        registryTask?.cancel()
        registryTask = Task { [weak self] in
            // Saved docs first, so their rooms dial with a cursor instead of
            // the registry opening them empty (one checkpoint download each).
            if let self, let store = self.store { await self.loadStoreDocs(store) }
            self?.storeDocsLoaded = true
            while !Task.isCancelled {
                guard let self else { return }
                await self.refreshRegistry()
                await self.refreshAccount()
                await self.refreshPeople()
                await self.drainPushes()
                await self.refreshUnread()
                self.retryPendingOpen()
                try? await Task.sleep(nanoseconds: 15_000_000_000)
            }
        }
    }

    // MARK: Registry

    func refreshRegistry() async {
        do {
            let items = try await api.registry()
            registryError = nil
            var newProjects: [Project] = []
            var newChats: [Id: [Chat]] = [:]
            for item in items {
                var p = item.project
                if let prev = projects.first(where: { $0.id == p.id }) {
                    p.modelNote = prev.modelNote
                    if p.state == prev.state { p.stateDetail = prev.stateDetail }
                }
                newProjects.append(p)
                var mine = item.chats
                let prevChats = chats[p.id] ?? []
                for i in mine.indices {
                    if let prev = prevChats.first(where: { $0.id == mine[i].id }) {
                        mine[i].unread = prev.unread
                        mine[i].lastMessageAt = max(mine[i].lastMessageAt, prev.lastMessageAt)
                        if mine[i].title == "New chat" && prev.title != "New chat" {
                            mine[i].title = prev.title
                        }
                    }
                }
                newChats[p.id] = AppModel.sorted(mine)
            }
            for i in newProjects.indices {
                if let prev = projects.first(where: { $0.id == newProjects[i].id }) {
                    newProjects[i].workspace = prev.workspace
                }
            }
            projects = newProjects.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
            chats = newChats
            adoptRelayArchiveState(newChats)
            saveRegistryCache()
            await flushArchiveWrites()
            await refreshMachine()
            for p in newProjects {
                ensureRoom(projectId: p.id, key: projectDocKey(p.id), roomChat: projectDocChatId)
            }
            for list in newChats.values {
                for c in list {
                    ensureRoom(projectId: c.projectId, key: c.id, roomChat: c.id)
                }
            }
        } catch {
            if (error as? RelayError)?.isUnauthorized == true {
                // Token revoked or relay re-initialised: pair again.
                unpair(forget: true)
                pairingError = "This device was signed out by the relay. Pair again."
                return
            }
            registryError = error.localizedDescription
            link = .reconnecting
        }
    }

    // MARK: On-device store (#929): open, load, save, flush

    private func openStore() {
        guard store == nil, let creds = credentials, !isFixture else { return }
        if ProcessInfo.processInfo.environment["ARBOS_NO_STORE"] == "1" { return }
        store = DeviceStore.open(relayUrl: creds.relayUrl, account: creds.deviceId)
        storeUsage = store?.usage() ?? DeviceStore.Usage()
    }

    /// Registry, people chats, account and unread counts: small JSON, read
    /// synchronously so the first frame already has the list.
    private func loadStoreKV(_ store: DeviceStore) {
        if let data = store.get(StoreKey.registry),
           let cached = try? JSONDecoder().decode(RegistryCache.self, from: data), !cached.projects.isEmpty {
            projects = cached.projects
            chats = cached.chats
        }
        if let data = store.get(StoreKey.people),
           let raw = try? JSONSerialization.jsonObject(with: data) as? [Any] {
            let list = raw.compactMap(PeopleChat.parse)
            if !list.isEmpty {
                peopleChats = list
                for c in list where c.unread { peopleUnread.insert(c.id) }
            }
        }
        if user == nil, let data = store.get(StoreKey.me), let who = try? JSONDecoder().decode(AuthUser.self, from: data) {
            user = who
        }
        if let data = store.get(StoreKey.unread), let counts = try? JSONDecoder().decode([String: Int].self, from: data) {
            unreadCounts = counts
        }
        if !projects.isEmpty || !peopleChats.isEmpty { contentOrigin = "store" }
        LaunchClock.mark("kv-loaded", detail: "projects=\(projects.count) people=\(peopleChats.count)")
    }

    /// Import every saved room off the main thread (last opened first), then
    /// hand each to `ensureRoom` with its cursor so it paints and dials at once.
    private func loadStoreDocs(_ store: DeviceStore) async {
        let saved = store.loadDocs()
        guard !saved.isEmpty else { return }
        let pending = store.loadPending()
        let started = Date()
        // Import and the Loro → Message conversion run off the main thread; the
        // main actor only receives finished docs and message arrays.
        let docs: [(DeviceStore.SavedDoc, ChatDoc, [Message]?)] = await Task.detached(priority: .userInitiated) {
            saved.compactMap { s in
                let doc = ChatDoc(key: s.key)
                do {
                    try doc.importUpdate(s.snapshot)
                } catch {
                    NSLog("store: snapshot for \(s.key) unreadable: \(error.localizedDescription)")
                    return nil
                }
                let messages: [Message]? = s.roomChat == projectDocChatId ? nil : doc.readAll()
                return (s, doc, messages)
            }
        }.value
        for (s, doc, messages) in docs {
            ensureRoom(projectId: s.projectId, key: s.key, roomChat: s.roomChat,
                       restored: (doc, s.cursor, pending[s.key] ?? [], messages))
        }
        LaunchClock.mark("store-docs", detail: "\(docs.count) docs in \(Int(Date().timeIntervalSince(started) * 1000)) ms")
    }

    private func saveRegistryCache() {
        guard let store else { return }
        guard !projects.isEmpty else {
            store.remove(StoreKey.registry)
            return
        }
        if let data = try? JSONEncoder().encode(RegistryCache(projects: projects, chats: chats)) {
            store.put(StoreKey.registry, data)
        }
    }

    private func savePeopleCache(_ raw: [Any]) {
        guard let store, let data = try? JSONSerialization.data(withJSONObject: raw) else { return }
        store.put(StoreKey.people, data)
    }

    private func saveAccountCache() {
        guard let store, let who = user, let data = try? JSONEncoder().encode(who) else { return }
        store.put(StoreKey.me, data)
    }

    private func saveUnreadCache() {
        guard let store, let data = try? JSONEncoder().encode(unreadCounts) else { return }
        store.put(StoreKey.unread, data)
    }

    /// A room's doc or cursor moved: save soon (coalesced per 1.5 s).
    private func markDirty(_ key: Id) {
        guard store != nil else { return }
        dirtyDocs.insert(key)
        guard flushTask == nil else { return }
        flushTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: AppModel.flushDelayNs)
            guard let self, !Task.isCancelled else { return }
            self.flushTask = nil
            self.flushStore(all: false)
        }
    }

    /// Write dirty rooms (or every room) to the store. The cursor is read
    /// before the export, so a saved snapshot always covers its cursor.
    func flushStore(all: Bool) {
        guard let store else { return }
        flushTask?.cancel()
        flushTask = nil
        let keys = all ? Set(docs.keys) : dirtyDocs
        dirtyDocs = []
        for key in keys {
            guard let doc = docs[key], let room = rooms[key], !doc.isEmpty else { continue }
            let cursor = room.cursor
            guard let bytes = try? doc.exportSnapshot() else { continue }
            store.saveDoc(key: key, projectId: room.projectId, roomChat: room.roomChat, cursor: cursor, snapshot: bytes)
        }
        store.enforceCaps()
        if all { store.flush() }
        if !keys.isEmpty { storeUsage = store.usage() }
    }

    private func forgetDoc(_ key: Id) {
        dirtyDocs.remove(key)
        store?.deleteDoc(key)
    }

    static func sorted(_ chats: [Chat]) -> [Chat] {
        chats.sorted { a, b in
            if a.isPinned != b.isPinned { return a.isPinned }
            return a.lastMessageAt > b.lastMessageAt
        }
    }

    private func projectDocKey(_ projectId: Id) -> String { "proj:\(projectId)" }

    // MARK: Rooms

    /// `restored`: a doc imported from the on-device store with the relay
    /// cursor it was saved at and the pushes the relay never ACKed.
    private func ensureRoom(projectId: Id, key: Id, roomChat: String,
                            restored: (doc: ChatDoc, cursor: Int64, pending: [(String, Data)], messages: [Message]?)? = nil) {
        guard rooms[key] == nil else { return }
        let doc = restored?.doc ?? ChatDoc(key: key)
        let isProjectDoc = roomChat == projectDocChatId
        docs[key] = doc
        doc.onChange { [weak self] touched in
            Task { @MainActor in
                guard let self else { return }
                self.markDirty(key)
                if isProjectDoc {
                    self.applyProjectDoc(projectId)
                } else if touched.listChanged {
                    self.emitSnapshot(key)
                    self.markActivity(chatId: key)
                    // Loro is live; /me/pushes and /me/unread can lag — light the
                    // blue circle and drain the push queue as soon as a new row lands.
                    if self.caughtUp.contains(key) {
                        self.noteLiveUnread(chatId: key)
                    }
                } else if !touched.messages.isEmpty {
                    self.emitMessages(key, indexes: touched.messages)
                } else if self.messages[key] == nil, let doc = self.docs[key], doc.count > 0 {
                    // Checkpoint / bulk import sometimes reports no list path —
                    // still paint so home search can see the transcript.
                    self.emitSnapshot(key)
                }
            }
        }
        let room = Room(projectId: projectId, key: key, roomChat: roomChat, doc: doc, api: api,
                        cursor: restored?.cursor ?? 0, pending: restored?.pending ?? []) { [weak self] in
            self?.deviceId ?? "ios:unpaired"
        }
        room.onCursorMoved = { [weak self] in self?.markDirty(key) }
        room.onPendingChanged = { [weak self] batches in self?.store?.savePending(key: key, batches: batches) }
        room.onLink = { [weak self] l in
            guard let self else { return }
            self.link = l
            // Backfill rows after a reconnect must not light unread; only live
            // rows after the next catch-up do (Loro is ahead of /me/unread).
            if l != .live { self.caughtUp.remove(key) }
        }
        room.presenceUser = { [weak self] in self?.user.map { ($0.id, $0.login) } }
        room.onBoxState = { [weak self] state, detail in
            self?.setProjectState(projectId, state, detail: detail)
        }
        room.onDocPaint = { [weak self] in
            guard let self, !isProjectDoc else { return }
            self.emitSnapshot(key)
        }
        room.onCaughtUp = { [weak self] in
            guard let self else { return }
            if isProjectDoc {
                self.applyProjectDoc(projectId)
            } else {
                self.emitSnapshot(key)
                self.caughtUp.insert(key)
            }
        }
        room.onAcked = { [weak self] in
            // Project-doc writes are not user bubbles; clearing a global set here
            // used to leave people-chat messages stuck at pending opacity.
            guard let self, !isProjectDoc else { return }
            guard self.unacked[key] != nil else { return }
            self.unacked[key] = nil
            self.emitSnapshot(key)
        }
        if !isProjectDoc {
            room.onPresence = { [weak self] header in
                self?.notePresence(header)
                if let callId = header["in_call"] as? String { self?.peerInCall(callId, chatId: key) }
            }
        }
        rooms[key] = room
        if let restored {
            // Paint from disk now; the room's backfill only adds what is new.
            if isProjectDoc {
                applyProjectDoc(projectId)
            } else if let list = restored.messages {
                messages[key] = list
            } else {
                emitSnapshot(key)
            }
        }
        room.start()
    }

    private func emitSnapshot(_ chatId: Id) {
        guard let doc = docs[chatId] else { return }
        var list = doc.readAll()
        let pendingIds = unacked[chatId] ?? []
        for i in list.indices where list[i].role == .user {
            list[i].pending = pendingIds.contains(list[i].id)
        }
        messages[chatId] = list
    }

    private func emitMessages(_ chatId: Id, indexes: Set<Int>) {
        guard let doc = docs[chatId] else { return }
        var list = messages[chatId] ?? []
        var needSnapshot = false
        let pendingIds = unacked[chatId] ?? []
        for ix in indexes.sorted() {
            guard var m = doc.readMessage(ix) else { continue }
            if m.role == .user { m.pending = pendingIds.contains(m.id) }
            if ix < list.count, list[ix].id == m.id {
                list[ix] = m
            } else if ix == list.count {
                list.append(m)
            } else {
                needSnapshot = true
            }
        }
        if needSnapshot {
            emitSnapshot(chatId)
        } else {
            messages[chatId] = list
        }
    }

    private func markUnacked(chatId: Id, messageId: Id) {
        unacked[chatId, default: []].insert(messageId)
    }

    private func clearUnacked(chatId: Id, messageId: Id) {
        guard var set = unacked[chatId] else { return }
        set.remove(messageId)
        unacked[chatId] = set.isEmpty ? nil : set
    }

    private func markActivity(chatId: Id) {
        for (pid, list) in chats {
            if let i = list.firstIndex(where: { $0.id == chatId }) {
                var updated = list
                updated[i].lastMessageAt = nowMs()
                chats[pid] = AppModel.sorted(updated)
                return
            }
        }
    }

    /// A live Loro row landed while this chat is not on screen: show the blue
    /// circle immediately and drain `/me/pushes` so the banner does not wait
    /// for the 15 s registry poll (notify / APNs can lag the row).
    private func noteLiveUnread(chatId: Id) {
        guard !isWatching(chatId), !isChatArchived(chatId) else { return }
        if let last = messages[chatId]?.last {
            if last.pending { return }
            if last.role == .system { return }
            if let me = myUserId, last.sender == me { return }
            // Project-chat echoes from this device use sender "user".
            if peopleChat(chatId) == nil, last.role == .user, last.sender == "user" || last.sender.isEmpty {
                return
            }
        }
        var changed = false
        if peopleChat(chatId) != nil {
            if !peopleUnread.contains(chatId) {
                peopleUnread.insert(chatId)
                changed = true
            }
            if let i = peopleChats.firstIndex(where: { $0.id == chatId }) {
                peopleChats[i].lastActivityAt = nowMs()
                if peopleChats[i].headSeq <= peopleChats[i].readSeq {
                    peopleChats[i].headSeq = peopleChats[i].readSeq + 1
                    changed = true
                }
            }
        } else {
            for (pid, list) in chats {
                if let i = list.firstIndex(where: { $0.id == chatId }), !list[i].unread {
                    var updated = list
                    updated[i].unread = true
                    chats[pid] = updated
                    changed = true
                    break
                }
            }
        }
        if changed { applyBadge() }
        Task { await drainPushes() }
    }

    /// Project doc (crdt-sync §6.3): `settings.modelNote`, plus the D70
    /// mirrors `status.workspace {state, since}` and `status.box {state, since}`.
    /// The machine itself comes from `GET /machine`; the doc only refines the
    /// workspace dot between polls.
    private func applyProjectDoc(_ projectId: Id) {
        guard let doc = docs[projectDocKey(projectId)] else { return }
        let settings = doc.mapValue("settings")
        let note = settings["modelNote"] as? String ?? ""
        let status = doc.mapValue("status")
        let workspace = WorkspaceState.from((status["workspace"] as? [String: Any])?["state"] as? String)
        if let i = projects.firstIndex(where: { $0.id == projectId }) {
            if projects[i].modelNote != note { projects[i].modelNote = note }
            if workspace != .unknown, projects[i].workspace != workspace { projects[i].workspace = workspace }
        }
    }

    private func setProjectState(_ projectId: Id, _ state: BoxState, detail: String?) {
        guard let i = projects.firstIndex(where: { $0.id == projectId }) else { return }
        if projects[i].state == state && projects[i].stateDetail == detail { return }
        if projects[i].state != state { projects[i].stateSince = nowMs() }
        projects[i].state = state
        projects[i].stateDetail = detail
        // Room STATUS frames report the user's box (D70): the machine moved too.
        let shown = machine.coerced(state)
        if machine.reported, machine.state != shown, state != .error {
            machine.state = shown
            machine.since = nowMs()
        }
    }

    /// `GET /machine`: state, since, spec, spend and the workspace states.
    func refreshMachine() async {
        do {
            var m = try await api.machine()
            // `/me` may carry `always_on` on relays whose `/machine` does not.
            if meAlwaysOn == true, !m.alwaysOn {
                m.alwaysOn = true
                m.state = m.coerced(m.state)
            }
            machineError = nil
            if m != machine { machine = m }
            for i in projects.indices {
                if let w = m.workspaces[projects[i].id], projects[i].workspace != w { projects[i].workspace = w }
            }
        } catch {
            if (error as? RelayError)?.status == 404 {
                machineError = "This relay has no machine API yet"
            } else {
                machineError = error.localizedDescription
            }
        }
    }

    /// Machine-level error text (relay error or the machine's own state detail).
    var machineDetail: String? {
        if machine.state == .error {
            return projects.first { $0.state == .error }?.stateDetail ?? machineError
        }
        return nil
    }

    // MARK: Machine control (D70: one call for the whole box)

    func startMachine() {
        guard !machineBusy else { return }
        machineBusy = true
        machine.state = machine.coerced(.waking)
        machine.since = nowMs()
        Task {
            defer { machineBusy = false }
            do {
                try await api.startMachine()
            } catch {
                notice = Notice(text: "Start failed: \(error.localizedDescription)")
                machine.state = .error
                machineError = error.localizedDescription
            }
            await refreshMachine()
        }
    }

    func stopMachine() {
        guard !machineBusy else { return }
        machineBusy = true
        Task {
            defer { machineBusy = false }
            do {
                try await api.stopMachine()
                machine.state = .stoppedByUser
                machine.since = nowMs()
            } catch {
                notice = Notice(text: "Stop failed: \(error.localizedDescription)")
            }
            await refreshMachine()
        }
    }

    /// Always-on boxes (D73): the one control is a restart. The state stays
    /// awake throughout; `machineBusy` is the only progress signal.
    func restartMachine() {
        guard !machineBusy else { return }
        machineBusy = true
        Task {
            defer { machineBusy = false }
            do {
                try await api.restartMachine()
                machine.since = nowMs()
            } catch {
                notice = Notice(text: "Restart failed: \(error.localizedDescription)")
                machine.state = .error
                machineError = error.localizedDescription
            }
            await refreshMachine()
        }
    }

    // MARK: Connectors (F1)

    /// Load the relay's connector catalog + connected accounts for Settings.
    func refreshConnectors() {
        guard credentials != nil, !isFixture else { return }
        Task {
            do {
                connectors = try await api.connectors()
            } catch let e as RelayError where e.status == 404 {
                // Older relays have no connectors route.
                connectors = ConnectorsView(loaded: true)
            } catch {
                notice = Notice(text: "Connectors: \(error.localizedDescription)")
            }
        }
    }

    /// Start Google OAuth in Safari; the relay finishes the callback, then we poll.
    func connectConnector(provider: String) {
        guard !connectorsBusy, credentials != nil, !isFixture else { return }
        connectorsBusy = true
        Task {
            defer { connectorsBusy = false }
            do {
                if let urlStr = try await api.connectConnector(provider: provider),
                   let url = URL(string: urlStr) {
                    await UIApplication.shared.open(url)
                }
                for wait in [0, 4, 8, 15, 30] as [UInt64] {
                    if wait > 0 { try? await Task.sleep(nanoseconds: wait * 1_000_000_000) }
                    do {
                        connectors = try await api.connectors()
                    } catch { break }
                    if connectors.connector(for: provider)?.status == .connected { break }
                }
            } catch {
                notice = Notice(text: "Connect \(provider): \(error.localizedDescription)")
                await refreshConnectorsOnce()
            }
        }
    }

    func disconnectConnector(id: Id) {
        guard !connectorsBusy, credentials != nil, !isFixture else { return }
        connectorsBusy = true
        var next = connectors
        next.items.removeAll { $0.id == id }
        connectors = next
        Task {
            defer { connectorsBusy = false }
            do {
                try await api.disconnectConnector(id: id)
            } catch {
                notice = Notice(text: "Disconnect: \(error.localizedDescription)")
            }
            await refreshConnectorsOnce()
        }
    }

    private func refreshConnectorsOnce() async {
        guard credentials != nil, !isFixture else { return }
        do {
            connectors = try await api.connectors()
        } catch let e as RelayError where e.status == 404 {
            connectors = ConnectorsView(loaded: true)
        } catch {}
    }

    // MARK: People chats (D72)

    var myUserId: String? { user.map { "user:\($0.id)" } }

    func peopleChat(_ id: String) -> PeopleChat? { peopleChats.first { $0.id == id } }

    func isPeopleChat(_ id: String) -> Bool { peopleChat(id) != nil }

    func refreshPeople() async {
        guard credentials != nil, !isFixture else { return }
        do {
            let raw = try await api.peopleChatsJSON()
            let list = raw.compactMap(PeopleChat.parse)
            peopleChats = list
            for c in list where c.unread { peopleUnread.insert(c.id) }
            for c in list where !c.unread && visibleChatId != c.id {
                // Loro can mark unread before the people list's head_seq catches up;
                // keep the blue circle while the latest row is still incoming.
                if peopleUnread.contains(c.id), let last = messages[c.id]?.last,
                   !last.pending, last.role != .system,
                   myUserId == nil || last.sender != myUserId {
                    continue
                }
                peopleUnread.remove(c.id)
            }
            for c in list { ensureRoom(projectId: peopleSlot, key: c.id, roomChat: c.id) }
            savePeopleCache(raw)
        } catch {
            // Relays without people chats (older) answer 404; stay quiet.
        }
    }

    /// Search handles (prefix) for the new-chat sheet.
    func searchPeople(_ q: String) async -> [Person] {
        if isFixture {
            let needle = q.lowercased()
            return directory.filter { $0.handle.lowercased().contains(needle) || ($0.name ?? "").lowercased().contains(needle) }
        }
        return (try? await api.searchUsers(q)) ?? []
    }

    /// `GET /users` (members and invited people except me), cached 10 min.
    /// Older relays without the route: the union of one-letter searches.
    func loadDirectory(force: Bool = false) async {
        guard credentials != nil, !isFixture else { return }
        if !force, nowMs() - directoryLoadedAt < 600_000, !directory.isEmpty { return }
        directoryLoadedAt = nowMs()
        if let list = try? await api.users() {
            directory = list.filter { $0.id != user?.id }
            return
        }
        var seen: [String: Person] = [:]
        await withTaskGroup(of: [Person].self) { group in
            for ch in "abcdefghijklmnopqrstuvwxyz0123456789" {
                group.addTask { [api] in (try? await api.searchUsers(String(ch))) ?? [] }
            }
            for await list in group {
                for p in list where p.status != "github" && p.id != user?.id { seen[p.id] = p }
            }
        }
        directory = seen.values.sorted { $0.handle.lowercased() < $1.handle.lowercased() }
    }

    /// People group: `POST /chats {kind: group, members}`. Relays that still
    /// return 501 for that route: open a DM with the first person, then add the
    /// rest (`POST /chats/{id}/members` turns a DM into a group on the third person).
    /// Optional `agentIds` are added afterwards (`POST /chats/{id}/agents`) so the
    /// group can start with people and your agents in one step.
    func createGroup(handles: [String], agentIds: [Id] = []) async -> Bool {
        let cleaned = handles
            .map { $0.trimmingCharacters(in: CharacterSet(charactersIn: "@ ")) }
            .filter { !$0.isEmpty }
        guard !cleaned.isEmpty else {
            notice = Notice(text: "Add at least one other person")
            return false
        }
        do {
            var chat: PeopleChat
            do {
                chat = try await api.createGroup(handles: cleaned)
            } catch let e as RelayError where e.status == 404 || e.status == 405 || e.status == 501 {
                chat = try await createGroupViaDm(handles: cleaned)
            }
            for pid in agentIds {
                do {
                    chat = try await api.addAgent(chatId: chat.id, projectId: pid)
                } catch {
                    notice = Notice(text: "Group created, but adding an agent failed: \(error.localizedDescription)")
                }
            }
            if let i = peopleChats.firstIndex(where: { $0.id == chat.id }) {
                peopleChats[i] = chat
            } else {
                peopleChats.insert(chat, at: 0)
            }
            ensureRoom(projectId: peopleSlot, key: chat.id, roomChat: chat.id)
            path = [.chat(chatId: chat.id, projectId: peopleSlot)]
            return true
        } catch {
            notice = Notice(text: "Could not create the group: \(error.localizedDescription)")
            return false
        }
    }

    /// Fallback when `POST /chats {kind: group}` is not on the relay yet.
    private func createGroupViaDm(handles: [String]) async throws -> PeopleChat {
        var chat = try await api.openDM(handle: handles[0])
        for handle in handles.dropFirst() {
            chat = try await api.addMember(chatId: chat.id, handle: handle)
        }
        return chat
    }

    /// Agent profile: rename / recolour a project (`PATCH /projects/{id}`).
    // MARK: Call voice (§10.3)

    @Published private(set) var voiceCatalog: VoiceCatalog?

    /// `GET /voices` once per launch (the relay caches it for an hour anyway).
    func loadVoices() async {
        guard voiceCatalog == nil, !isFixture else { return }
        voiceCatalog = try? await api.voices()
    }

    /// Save on tap: show it at once, PATCH, then read back what the relay kept.
    func setVoice(_ projectId: Id, _ voice: String?) {
        if let i = projects.firstIndex(where: { $0.id == projectId }) { projects[i].voice = voice }
        guard !isFixture else { return }
        Task {
            do {
                let kept = try await api.setProjectVoice(projectId, voice: voice)
                if let i = projects.firstIndex(where: { $0.id == projectId }) { projects[i].voice = kept }
            } catch {
                notice = Notice(text: "Voice: \(error.localizedDescription)")
                await refreshRegistry()
            }
        }
    }

    func updateProject(_ projectId: Id, name: String? = nil, color: String? = nil, icon: String? = nil, avatar: String? = nil, avatarColor: String? = nil, description: String? = nil) {
        if let i = projects.firstIndex(where: { $0.id == projectId }) {
            if let description { projects[i].description = description }
            if let name { projects[i].name = name }
            if let color { projects[i].color = color }
            if let icon { projects[i].icon = icon }
            if let avatar { projects[i].avatar = avatar }
            // "mono" is kept as an explicit pick; nil would fall back to the coloured default (D74).
            if let avatarColor { projects[i].avatarColor = avatarColor }
        }
        guard !isFixture else { return }
        Task {
            do { try await api.patchProject(projectId, name: name, color: color, icon: icon, avatar: avatar, avatarColor: avatarColor, description: description) }
            catch { notice = Notice(text: "Could not save: \(error.localizedDescription)") }
        }
    }

    // MARK: Tree avatars

    /// Project id → tree slug (explicit `avatar`, else hashed + deduplicated).
    /// Recomputed whenever the project list changes.
    @Published private(set) var avatarMap: [Id: String] = [:]

    /// Picks made on this phone, kept until the relay stores `avatar` itself
    /// (PATCH /projects/{id} ignores unknown fields on relays without it).
    private var avatarOverrides: [Id: String] {
        get { (UserDefaults.standard.dictionary(forKey: "avatarOverrides") as? [String: String]) ?? [:] }
        set { UserDefaults.standard.set(newValue, forKey: "avatarOverrides") }
    }

    func avatarSlug(for project: Project) -> String? {
        if let slug = project.avatar, TreeAvatars.shared.has(slug) { return slug }
        if let slug = avatarMap[project.id] { return slug }
        // Agents from other people's lists (shared agents in a DM) are not in
        // `projects`; the relay's default for them, when the art is bundled.
        if let slug = TreeAvatars.shared.relayDefault(project.id), TreeAvatars.shared.has(slug) { return slug }
        return nil
    }

    func recomputeAvatars() {
        let overrides = avatarOverrides
        let withPicks = projects.map { p -> Project in
            var q = p
            if q.avatar == nil, let slug = overrides[p.id] { q.avatar = slug }
            return q
        }
        let map = TreeAvatars.shared.assign(withPicks)
        if map != avatarMap { avatarMap = map }
    }

    /// Colour picks made on this phone (same fallback as `avatarOverrides`).
    private var avatarColorOverrides: [Id: String] {
        get { (UserDefaults.standard.dictionary(forKey: "avatarColorOverrides") as? [String: String]) ?? [:] }
        set { UserDefaults.standard.set(newValue, forKey: "avatarColorOverrides") }
    }

    /// The colour set a project's tree is tinted with; nil = mono. A project
    /// without a chosen colour shows the relay's hash default (D74: coloured,
    /// never mono); only an explicit "mono" pick draws the white tree.
    func avatarColor(for project: Project) -> TreeColor? {
        let name = project.avatarColor ?? avatarColorOverrides[project.id]
        guard let name else { return TreeAvatars.shared.defaultColor(forProjectId: project.id) }
        guard name != "mono" else { return nil }
        return TreeAvatars.shared.color(named: name) ?? TreeAvatars.shared.defaultColor(forProjectId: project.id)
    }

    func pickAvatarColor(_ projectId: Id, name: String) {
        var o = avatarColorOverrides
        o[projectId] = name
        avatarColorOverrides = o
        updateProject(projectId, avatarColor: name)
        objectWillChange.send()
    }

    func pickAvatar(_ projectId: Id, slug: String) {
        var o = avatarOverrides
        o[projectId] = slug
        avatarOverrides = o
        updateProject(projectId, avatar: slug)
        recomputeAvatars()
    }

    /// Open (or create) the DM with a handle and navigate to it.
    func openDM(handle: String, navigate: Bool = true) async -> PeopleChat? {
        do {
            let chat = try await api.openDM(handle: handle)
            if !peopleChats.contains(where: { $0.id == chat.id }) { peopleChats.insert(chat, at: 0) }
            ensureRoom(projectId: peopleSlot, key: chat.id, roomChat: chat.id)
            if navigate { path = [.chat(chatId: chat.id, projectId: peopleSlot)] }
            return chat
        } catch {
            notice = Notice(text: (error as? RelayError)?.type == "not_available" ? "You can't message this person." : "Could not open the chat: \(error.localizedDescription)")
            return nil
        }
    }

    func open(peopleChat chat: PeopleChat) {
        path = [.chat(chatId: chat.id, projectId: peopleSlot)]
    }

    /// Send into a people chat: author fields + mentions resolved from `@slug`s.
    /// `address` is an optional chip target (`agent:…` / `user:…` / `"everyone"`) when the
    /// text itself has no `@` mention — so a follow-up can keep talking to one agent, or
    /// "hey everyone" can reach every agent, without typing `@`.
    func sendPeople(chatId: String, text: String, replyTo: String?, address: String? = nil, attachments: [PendingAttachment] = []) {
        guard let doc = docs[chatId], let chat = peopleChat(chatId), let me = user else { return }
        var mentions = AppModel.mentions(in: text, members: chat.members)
        // Leading bare person names ("Allan", "Allan?", "Yo Allan …") count as addressing
        // that person — so a sticky "Message @bob" chip does not wake the agent. A mid-line
        // @handle ("can you see @jo?") still keeps the chip target.
        let leadingPeople = AppModel.namedPeople(in: text, members: chat.members)
        for person in leadingPeople where !mentions.contains(person) {
            mentions.append(person)
        }
        // Only agent @mentions in the typed text count as an address. A leading person name
        // must not keep a sticky agent chip — otherwise "Message @bob" + "Allan?" wakes bob.
        // A person @handle mid-line must not block the chip (bob still answers about @jo).
        let addressedAgent = mentions.contains { $0.hasPrefix("agent:") }
        if !addressedAgent, let address {
            if address == "everyone" {
                mentions.append(contentsOf: chat.agents.map(\.member).filter { !mentions.contains($0) })
            } else if address.hasPrefix("agent:"), !leadingPeople.isEmpty {
                // Text opens by naming a person; leave the sticky agent out of mentions.
            } else if chat.members.contains(where: { $0.member == address }), !mentions.contains(address) {
                mentions.append(address)
            }
        }
        // In an agent-only DM, carry on with the agent who last spoke when nothing addresses
        // an agent and the message is not aimed at a person. A DM with a human peer (Allan +
        // bob) must not inherit that sticky target — otherwise "New test" / "Don't respond bob"
        // still wakes bob. A person chip / @handle / bare name must not also ping the last
        // agent — otherwise "Message Allan" still wakes bob. In a group, leave the message
        // unaddressed (unless a chip / @agent set one) so the box decision model can choose
        // which agent replies from chat context.
        var reply = replyTo
        let addressedPerson = mentions.contains { $0.hasPrefix("user:") }
        let hasHumanPeer = chat.humans.contains { $0.id != me.id }
        if chat.kind != "group", !hasHumanPeer, !mentions.contains(where: { $0.hasPrefix("agent:") }),
           !addressedPerson, reply == nil,
           let last = messages(of: chatId).last(where: { $0.role == .assistant }) {
            reply = last.id
            if let h = last.senderHandle?.lowercased(),
               let agent = chat.agents.first(where: {
                   mentionSlug($0.project?.name ?? "").lowercased() == h || $0.handle.lowercased() == h
               }) {
                mentions.append(agent.member)
            }
        }
        let author = ChatDoc.Author(sender: "user:\(me.id)", handle: me.login, name: me.name, avatar: me.avatarUrl)
        let messageId = newId()
        // Text-only: write on this turn (same as project `write`) so the push leaves
        // before we return. Deferring every send into a Task left bubbles faint when
        // the room ACK raced a still-queued commit.
        if attachments.isEmpty {
            commitPeople(chatId: chatId, doc: doc, messageId: messageId, text: text, author: author,
                         mentions: mentions, reply: reply, attachments: [])
            return
        }
        markUnacked(chatId: chatId, messageId: messageId)
        Task {
            var uploaded: [Attachment] = []
            for a in attachments {
                let key = AppModel.blobKey(for: a.name)
                do {
                    _ = try await api.putBlob(projectId: "\(peopleSlot):\(chatId)", key: key, mime: a.mime, data: a.data)
                    uploaded.append(Attachment(name: a.name, mime: a.mime, blobRef: key, kind: Self.attachmentKind(mime: a.mime, name: a.name)))
                } catch {
                    notice = Notice(text: "Attachment upload failed: \(error.localizedDescription)")
                }
            }
            commitPeople(chatId: chatId, doc: doc, messageId: messageId, text: text, author: author,
                         mentions: mentions, reply: reply, attachments: uploaded, alreadyUnacked: true)
        }
    }

    /// Insert + pushCommand + commit for a people chat. `alreadyUnacked` when the caller
    /// reserved the id before an attachment upload.
    private func commitPeople(chatId: String, doc: ChatDoc, messageId: Id, text: String,
                              author: ChatDoc.Author, mentions: [String], reply: String?,
                              attachments: [Attachment], alreadyUnacked: Bool = false) {
        if !alreadyUnacked { markUnacked(chatId: chatId, messageId: messageId) }
        do {
            try doc.insertUserMessage(id: messageId, text: text, deviceId: deviceId, attachments: attachments,
                                      author: author, mentions: mentions, replyTo: reply)
            try doc.pushCommand("send", payload: ["messageId": messageId], device: deviceId)
        } catch {
            clearUnacked(chatId: chatId, messageId: messageId)
            notice = Notice(text: "Could not write the message: \(error.localizedDescription)")
            return
        }
        doc.commit()
        announce(chatId: chatId, projectId: nil, messageId: messageId, text: text)
        if let i = peopleChats.firstIndex(where: { $0.id == chatId }) { peopleChats[i].lastActivityAt = nowMs() }
        peopleChats.sort { $0.lastActivityAt > $1.lastActivityAt }
    }

    /// `@slug` tokens that name a member: agents by their mention slug, people by handle.
    /// `@everyone` / `@all` and a leading "hey everyone" (etc.) address every agent in the chat.
    static func mentions(in text: String, members: [ChatMember]) -> [String] {
        var out: [String] = []
        let agents = members.filter(\.isAgent)
        func appendAgents() {
            for a in agents where !out.contains(a.member) { out.append(a.member) }
        }
        let tokens = text.split(whereSeparator: { $0.isWhitespace })
        for t in tokens where t.hasPrefix("@") {
            let slug = t.dropFirst().trimmingCharacters(in: CharacterSet(charactersIn: ".,!?:;")).lowercased()
            if slug == "everyone" || slug == "all" {
                appendAgents()
                continue
            }
            if slug == "arbos", agents.count == 1, let a = agents.first {
                if !out.contains(a.member) { out.append(a.member) }
                continue
            }
            if let m = members.first(where: { $0.handle.lowercased() == slug || ($0.isAgent && mentionSlug($0.project?.name ?? "") == slug) }) {
                if !out.contains(m.member) { out.append(m.member) }
            }
        }
        if out.isEmpty && AppModel.addressesEveryone(text) {
            appendAgents()
        }
        return out
    }

    /// "hey everyone" / "hi everyone" / … at the start of the message (no `@` required).
    static func addressesEveryone(_ text: String) -> Bool {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        for g in ["hey everyone", "hi everyone", "hello everyone", "yo everyone", "ok everyone", "okay everyone"] {
            if trimmed == g { return true }
            for sep in [" ", ",", "!", ":", "—", "-"] {
                if trimmed.hasPrefix(g + sep) { return true }
            }
        }
        return false
    }

    /// Human members addressed at the start of the text without `@` — "Allan", "Allan?",
    /// "Allan. we are…", "Yo Allan …". Mid-line names ("ask Allan later") do not count, so a
    /// sticky agent chip still receives "can you see @jo?".
    static func namedPeople(in text: String, members: [ChatMember]) -> [String] {
        let humans = members.filter { !$0.isAgent }
        guard !humans.isEmpty else { return [] }
        var lower = text.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !lower.isEmpty else { return [] }
        for g in ["yo ", "hey ", "hi ", "hello ", "ok ", "okay "] {
            if lower.hasPrefix(g) { lower = String(lower.dropFirst(g.count)); break }
        }
        let first = lower.split(whereSeparator: { $0.isWhitespace }).first.map(String.init) ?? ""
        let token = first.trimmingCharacters(in: CharacterSet(charactersIn: ".,!?:;"))
        guard token.count >= 2 else { return [] }
        var out: [String] = []
        for h in humans {
            var needles: [String] = [h.handle.lowercased()]
            if let name = h.name?.trimmingCharacters(in: .whitespacesAndNewlines), !name.isEmpty {
                let n = name.lowercased()
                needles.append(n)
                if let firstName = n.split(whereSeparator: { $0.isWhitespace }).first, firstName.count >= 2 {
                    needles.append(String(firstName))
                }
            }
            if Set(needles).contains(token), !out.contains(h.member) {
                out.append(h.member)
            }
        }
        return out
    }

    /// Mark a people chat read up to the relay head (cross-launch cursor).
    /// Always PUT /read so chat_events unread clears even when Loro read_seq is already caught up
    /// (notify can insert events after the rows were already on screen).
    func markPeopleRead(_ chatId: String) {
        peopleUnread.remove(chatId)
        if unreadCounts[chatId] != nil { unreadCounts[chatId] = 0 }
        PushManager.shared.clearDelivered(chatId: chatId)
        defer { applyBadge() }
        guard let chat = peopleChat(chatId) else { return }
        if chat.headSeq > chat.readSeq || chat.unread {
            if let i = peopleChats.firstIndex(where: { $0.id == chatId }) {
                peopleChats[i].readSeq = chat.headSeq
                peopleChats[i].unread = false
            }
        }
        Task { try? await api.markRead(chatId: chatId, seq: chat.headSeq) }
    }

    func addAgent(chatId: String, projectId: Id) {
        Task {
            do {
                let chat = try await api.addAgent(chatId: chatId, projectId: projectId)
                if let i = peopleChats.firstIndex(where: { $0.id == chatId }) { peopleChats[i] = chat }
            } catch {
                notice = Notice(text: (error as? RelayError)?.type == "own_project_only" ? "You can only add your own agents." : "Add agent failed: \(error.localizedDescription)")
            }
        }
    }

    func addPerson(chatId: String, handle: String) {
        Task {
            do {
                let chat = try await api.addMember(chatId: chatId, handle: handle)
                if let i = peopleChats.firstIndex(where: { $0.id == chatId }) { peopleChats[i] = chat }
            } catch {
                let e = error as? RelayError
                notice = Notice(text: e?.type == "already_member" ? "@\(handle) is already in this chat."
                                 : e?.type == "not_available" ? "You can't add this person."
                                 : (e?.status == 404 || e?.status == 405 || e?.status == 501) ? "Adding people is not on this relay yet."
                                 : "Add person failed: \(error.localizedDescription)")
            }
        }
    }

    func removeAgent(chatId: String, projectId: Id) {
        Task {
            do {
                try await api.removeAgent(chatId: chatId, projectId: projectId)
                if let i = peopleChats.firstIndex(where: { $0.id == chatId }) {
                    peopleChats[i].members.removeAll { $0.isAgent && $0.id == projectId }
                }
            } catch { notice = Notice(text: "Remove agent failed: \(error.localizedDescription)") }
        }
    }

    func setReplyPolicy(chatId: String, projectId: Id, policy: String) {
        if let i = peopleChats.firstIndex(where: { $0.id == chatId }),
           let j = peopleChats[i].members.firstIndex(where: { $0.isAgent && $0.id == projectId }) {
            peopleChats[i].members[j].replyPolicy = policy
        }
        Task { try? await api.setReplyPolicy(chatId: chatId, projectId: projectId, policy: policy) }
    }

    func leavePeopleChat(_ chatId: String) {
        peopleChats.removeAll { $0.id == chatId }
        rooms[chatId]?.stop(); rooms[chatId] = nil; docs[chatId] = nil; messages[chatId] = nil
        unacked[chatId] = nil
        forgetDoc(chatId)
        path = []
        Task { try? await api.leaveChat(chatId) }
    }

    /// Rename a people group and/or set its picture blob key (`PATCH /chats/{id}`).
    func patchPeopleChat(_ chatId: String, title: String? = nil, imageKey: String? = nil) async -> Bool {
        do {
            let chat = try await api.patchPeopleChat(chatId, title: title, imageKey: imageKey)
            if let i = peopleChats.firstIndex(where: { $0.id == chatId }) {
                peopleChats[i] = chat
            } else {
                peopleChats.insert(chat, at: 0)
            }
            return true
        } catch {
            notice = Notice(text: "Could not update group: \(error.localizedDescription)")
            return false
        }
    }

    /// Upload a group picture to `people:{chatId}` and patch `image_key`.
    func setGroupImage(chatId: String, jpeg: Data) async -> Bool {
        let key = "group-avatar-\(Int(Date().timeIntervalSince1970 * 1000)).jpg"
        do {
            _ = try await api.putBlob(projectId: "\(peopleSlot):\(chatId)", key: key, mime: "image/jpeg", data: jpeg)
            return await patchPeopleChat(chatId, imageKey: key)
        } catch {
            notice = Notice(text: "Could not upload group image: \(error.localizedDescription)")
            return false
        }
    }

    func deletePeopleChat(_ chatId: String) {
        peopleChats.removeAll { $0.id == chatId }
        rooms[chatId]?.stop(); rooms[chatId] = nil; docs[chatId] = nil; messages[chatId] = nil
        unacked[chatId] = nil
        forgetDoc(chatId)
        path = []
        Task {
            do { try await api.deleteChat(chatId) } catch { notice = Notice(text: "Delete failed: \(error.localizedDescription)") }
        }
    }

    func block(_ member: ChatMember) { block(handle: member.handle) }

    func block(handle: String) {
        Task {
            do { try await api.block(handle: handle); blocked = (try? await api.blocks()) ?? blocked }
            catch { notice = Notice(text: "Block failed: \(error.localizedDescription)") }
        }
    }

    // MARK: Profile pages

    /// The profile page the root view presents (person, group or agent).
    @Published var profile: ProfileTarget?
    /// A chat the next ChatView should open with its search bar up (profile "search").
    @Published var pendingSearchChat: String?
    /// Last presence time per user id, from room presence frames this session.
    @Published private(set) var lastSeen: [String: Int64] = [:]

    func showProfile(_ target: ProfileTarget) { profile = target }

    /// A sender avatar was tapped: resolve the handle through the chat's members.
    func openPersonProfile(handle: String, avatarUrl: String?, chatId: String) {
        if let m = peopleChat(chatId)?.members.first(where: { $0.handle == handle && !$0.isAgent }) {
            showProfile(.user(ProfilePerson(m)))
        } else if let p = directory.first(where: { $0.handle == handle }) {
            showProfile(.user(ProfilePerson(p)))
        } else {
            showProfile(.user(ProfilePerson(id: handle, handle: handle, name: nil, avatarUrl: avatarUrl)))
        }
    }

    /// An agent that is not in my project list: the project carried by a
    /// people-chat member row (someone else's agent in a DM).
    func foreignAgent(_ projectId: Id) -> Project? {
        for c in peopleChats {
            if let a = c.agents.first(where: { $0.id == projectId }), var p = a.project {
                if p.ownerHandle == nil { p.ownerHandle = a.ownerHandle }
                p.shared = true
                if p.description.isEmpty, let d = a.description { p.description = d }
                return p
            }
        }
        return nil
    }

    func notePresence(_ header: [String: Any]) {
        guard let user = header["user"] as? String else { return }
        let at = (header["at"] as? NSNumber)?.int64Value ?? nowMs()
        if (lastSeen[user] ?? 0) < at { lastSeen[user] = at }
    }

    /// `PUT /chats/{id}/settings {muted}` per member; local flag updated first.
    func setMuted(chatId: String, muted: Bool) {
        if let i = peopleChats.firstIndex(where: { $0.id == chatId }) { peopleChats[i].muted = muted }
        guard !isFixture else { return }
        Task { try? await api.chatSettings(chatId: chatId, muted: muted) }
    }

    /// `GET /users/{id}` for a fresh presence line on a profile page.
    func fetchUser(_ id: String) async -> Person? {
        guard credentials != nil, !isFixture else { return nil }
        return (try? await api.user(id)) ?? nil
    }

    /// Projects of mine (or shared with me) that this person is a member of.
    func sharedProjects(with person: ProfilePerson) async -> [Project] {
        guard !isFixture else { return projects.filter { $0.shared } }
        var out: [Project] = []
        for p in projects.prefix(24) {
            if p.ownerHandle == person.handle { out.append(p); continue }
            let members = await projectMembers(p.id)
            if members.contains(where: { $0.id == person.id || $0.handle == person.handle }) { out.append(p) }
        }
        return out
    }

    func unblock(_ person: Person) {
        blocked.removeAll { $0.id == person.id }
        Task { try? await api.unblock(userId: person.id) }
    }

    func refreshBlocks() async {
        if let list = try? await api.blocks() { blocked = list }
    }

    // Projects together (step 3)
    func projectMembers(_ projectId: Id) async -> [ProjectMember] {
        (try? await api.projectMembers(projectId)) ?? []
    }

    func inviteToProject(_ projectId: Id, handle: String) async -> [ProjectMember]? {
        do { return try await api.addProjectMember(projectId, handle: handle) }
        catch {
            notice = Notice(text: (error as? RelayError)?.type == "already_member" ? "@\(handle) is already a member." : "Invite failed: \(error.localizedDescription)")
            return nil
        }
    }

    func removeProjectMember(_ projectId: Id, userId: String) async {
        do { try await api.removeProjectMember(projectId, userId: userId) }
        catch { notice = Notice(text: "Remove failed: \(error.localizedDescription)") }
    }

    /// `/me/pushes`: unread flags, chat list refreshes and local notifications.
    func drainPushes() async {
        guard credentials != nil, !isFixture, let items = try? await api.pushes(), !items.isEmpty else { return }
        var needChats = false
        for p in items where !notifiedPushIds.contains(p.id) {
            notifiedPushIds.insert(p.id)
            pushLog.notice("push kind=\(p.kind, privacy: .public) keys=\(Array(p.raw.keys).sorted().joined(separator: ","), privacy: .public)")
            switch p.kind {
            case "people_message":
                if let cid = p.payload["chat_id"], !isWatching(cid) {
                    peopleUnread.insert(cid)
                    let from = p.sender["handle"] ?? p.payload["from_handle"] ?? "someone"
                    // §7c sender{} first; the chat's member list fills any gap.
                    let member = peopleChat(cid)?.members.first { $0.handle == from }
                    let agent = member?.isAgent == true ? member?.project : nil
                    let kind = p.sender["kind"] ?? (agent != nil ? "agent" : "user")
                    let info = MessagePush.userInfo(
                        chatId: cid, projectId: peopleSlot, title: p.sender["name"] ?? member?.displayName ?? "@\(from)",
                        body: p.payload["preview"] ?? p.payload["text"] ?? "New message",
                        senderKind: kind, senderId: p.sender["id"] ?? member?.id ?? from,
                        senderName: p.sender["name"] ?? member?.displayName ?? from, senderHandle: from,
                        avatar: p.sender["avatar"] ?? agent.flatMap { avatarSlug(for: $0) },
                        avatarColor: p.sender["avatar_color"] ?? agent?.avatarColor,
                        avatarUrl: p.sender["avatar_url"] ?? member?.avatarUrl)
                    Task { await PushManager.shared.postMessage(id: p.id, info: info) }
                    applyBadge()
                }
            case "people_chat_created":
                needChats = true
                if let by = p.payload["by_handle"] { PushManager.post(id: p.id, title: "@\(by)", body: "Started a chat with you", chatId: p.payload["chat_id"] ?? "") }
            case "people_agent_added", "people_agent_removed", "people_member_added", "people_member_removed":
                needChats = true
            case "call_started":
                if let c = Call.parse(p.raw["call"]), c.chatId != CallManager.shared.call?.chatId {
                    CallManager.shared.ring(c, from: CallPerson.parse(p.raw["sender"]) ?? c.startedBy, viaVoip: false)
                }
            case "call_missed":
                if let cid = p.payload["call_id"], let chat = p.payload["chat_id"] {
                    CallManager.shared.missedCall(callId: cid, chatId: chat, projectId: p.payload["project_id"] ?? peopleSlot,
                                                  from: CallPerson.parse(p.raw["from"]) ?? CallPerson.parse(p.raw["sender"]))
                }
            case "call_ended":
                if let cid = p.payload["call_id"], let chat = p.payload["chat_id"] {
                    CallManager.shared.remoteEnded(callId: cid, chatId: chat, durationS: Int(p.payload["duration_s"] ?? "") ?? 0,
                                                   writerDevice: p.payload["writer_device"])
                }
            case "people_project_shared":
                if let by = p.payload["by_handle"] { PushManager.post(id: p.id, title: "@\(by)", body: "Shared a project with you", chatId: "") }
                await refreshRegistry()
            case "project_member_removed":
                await refreshRegistry()
            case "chat_archived":
                applyArchivePush(p)
            default:
                break
            }
        }
        if needChats { await refreshPeople() }
        if notifiedPushIds.count > 2000 { notifiedPushIds.removeAll() }
    }

    // MARK: Lookups

    /// Relay blob URL with the device token as a query parameter (the relay
    /// accepts `?token=` on every route), so AVPlayer and Safari can open it.
    func blobURL(projectId: Id, key: String) -> URL? {
        let base = api.endpoint.httpUrl
        guard !base.isEmpty else { return nil }
        // Build via URLComponents so a people-slot `:` and JWT characters stay valid;
        // leave the token value un-encoded (`=` padding) — the relay does not decode.
        guard var comps = URLComponents(string: base) else { return nil }
        comps.percentEncodedPath = "/blob/\(Self.blobPathSegment(projectId))/\(Self.blobPathSegment(key))"
        let token = credentials?.deviceToken ?? ""
        comps.percentEncodedQuery = token.isEmpty ? nil : "token=\(token)"
        return comps.url
    }

    /// Path segment for `/blob/{project}/{key}`: encode spaces etc., keep `:` for `people:{chat}`.
    private static func blobPathSegment(_ raw: String) -> String {
        var allowed = CharacterSet.urlPathAllowed
        allowed.insert(":")
        return raw.addingPercentEncoding(withAllowedCharacters: allowed) ?? raw
    }

    var authHeaders: [String: String] {
        ["Authorization": "Bearer \(credentials?.deviceToken ?? "")"]
    }

    // MARK: One chat per agent (D-single-chat)

    /// Multi-chat UI (new chat / chat switcher / thread swipes / chat list /
    /// chat-level archive) is off by default; Settings → Debug flips it. The
    /// data stays: extra chats remain on the box and the relay.
    @Published var multiChatEnabled: Bool = (UserDefaults.standard.object(forKey: "multiChatEnabled") as? Bool) ?? true {
        didSet { UserDefaults.standard.set(multiChatEnabled, forKey: "multiChatEnabled") }
    }

    // MARK: Chat archive (§9c: per user, synced through the relay; nothing deleted)

    /// Archived chats hide from the chats sheet's main list and never badge or
    /// notify here. History stays in the agent's context/memory tree and
    /// searchable history; nothing goes to agentd. The relay keeps the flag per
    /// user per chat (`archived_at`): the sheet writes it with
    /// `PUT /chats/{id}/settings {archived}`, every listing seeds it and the
    /// `chat_archived` push updates it live. The UserDefaults copy is only an
    /// offline cache plus the writes that have not reached the relay yet.
    @Published private(set) var archivedChatIds: Set<Id> = Set(UserDefaults.standard.stringArray(forKey: "archivedChats") ?? [])

    /// Archive writes the relay has not confirmed (offline, or a relay without §9c yet).
    private var pendingArchiveWrites: [Id: Bool] {
        get { (UserDefaults.standard.dictionary(forKey: "archivePendingWrites") as? [String: Bool]) ?? [:] }
        set { UserDefaults.standard.set(newValue, forKey: "archivePendingWrites") }
    }
    private var archiveFlushing = false

    /// The pinned "Main" chat never counts as archived, whatever the relay says (Mac rule).
    func isChatArchived(_ chatId: Id) -> Bool {
        archivedChatIds.contains(chatId) && chat(chatId)?.isPinned != true
    }

    func archiveChat(_ chatId: Id) {
        guard chat(chatId)?.isPinned != true else { return }
        setArchived(chatId, true)
    }

    func unarchiveChat(_ chatId: Id) { setArchived(chatId, false) }

    private func setArchived(_ chatId: Id, _ archived: Bool) {
        if archived { archivedChatIds.insert(chatId) } else { archivedChatIds.remove(chatId) }
        setChatArchivedAt(chatId, archived ? Int64(Date().timeIntervalSince1970 * 1000) : nil)
        saveArchiveCache()
        if archived {
            PushManager.shared.clearDelivered(chatId: chatId)
            applyBadge()
        }
        var pending = pendingArchiveWrites
        pending[chatId] = archived
        pendingArchiveWrites = pending
        Task { await flushArchiveWrites() }
    }

    private func saveArchiveCache() {
        UserDefaults.standard.set(Array(archivedChatIds), forKey: "archivedChats")
    }

    private func setChatArchivedAt(_ chatId: Id, _ at: Int64?) {
        for (pid, list) in chats {
            guard let i = list.firstIndex(where: { $0.id == chatId }) else { continue }
            var updated = list
            updated[i].archivedAt = at
            chats[pid] = updated
        }
    }

    /// Send pending archive writes to the relay; a write that fails stays
    /// pending (the local state already shows it) and is retried on the next
    /// registry refresh. 404/405 means the relay has no §9c yet: keep local.
    func flushArchiveWrites() async {
        guard !archiveFlushing, credentials != nil, !isFixture else { return }
        let pending = pendingArchiveWrites
        guard !pending.isEmpty else { return }
        archiveFlushing = true
        defer { archiveFlushing = false }
        for (chatId, archived) in pending {
            // The user may have flipped it again meanwhile: send the current value.
            let want = pendingArchiveWrites[chatId] ?? archived
            do {
                let at = try await api.setChatArchived(chatId: chatId, archived: want)
                if want, let at { setChatArchivedAt(chatId, at) }
                var p = pendingArchiveWrites
                if p[chatId] == want { p[chatId] = nil }
                pendingArchiveWrites = p
            } catch {
                pushLog.notice("archive write \(chatId, privacy: .public)=\(want) failed: \(error.localizedDescription, privacy: .public)")
            }
        }
    }

    /// First launch after the §9c update: the chats this device had archived
    /// locally go UP to the relay (union), after which the relay is the source
    /// of truth and the local list is only a cache.
    private func migrateLocalArchiveToRelay() {
        let key = "archiveMigratedToRelay"
        guard !UserDefaults.standard.bool(forKey: key) else { return }
        var pending = pendingArchiveWrites
        for id in archivedChatIds where pending[id] == nil { pending[id] = true }
        pendingArchiveWrites = pending
        UserDefaults.standard.set(true, forKey: key)
    }

    /// A relay listing with `archived_at` seeds the set: relay state, plus the
    /// writes still on their way up (so a flip made offline does not snap back).
    private func adoptRelayArchiveState(_ lists: [Id: [Chat]]) {
        let all = lists.values.flatMap { $0 }
        guard all.contains(where: { $0.archiveSynced }) else { return }
        migrateLocalArchiveToRelay()
        var next = archivedChatIds
        let pending = pendingArchiveWrites
        for c in all where c.archiveSynced && pending[c.id] == nil {
            if c.archivedAt != nil { next.insert(c.id) } else { next.remove(c.id) }
        }
        if next != archivedChatIds {
            archivedChatIds = next
            saveArchiveCache()
            applyBadge()
        }
    }

    /// `chat_archived {chat_id, project_id, archived_at}` from another device.
    private func applyArchivePush(_ p: PushItem) {
        guard let chatId = p.payload["chat_id"] else { return }
        let at = (p.raw["archived_at"] as? NSNumber)?.int64Value
        if pendingArchiveWrites[chatId] != nil { return }
        if at != nil { archivedChatIds.insert(chatId) } else { archivedChatIds.remove(chatId) }
        setChatArchivedAt(chatId, at)
        saveArchiveCache()
        if at != nil { PushManager.shared.clearDelivered(chatId: chatId) }
        applyBadge()
    }

    /// The chat the user last opened per agent on this device (tapping the agent returns there).
    private var lastOpenedChat: [Id: Id] {
        get { (UserDefaults.standard.dictionary(forKey: "lastOpenedChat") as? [String: String]) ?? [:] }
        set { UserDefaults.standard.set(newValue, forKey: "lastOpenedChat") }
    }

    func rememberOpened(_ chat: Chat) {
        guard chat.projectId != peopleSlot else { return }
        var m = lastOpenedChat
        m[chat.projectId] = chat.id
        lastOpenedChat = m
    }

    /// Tapping an agent: the last chat used here, if it still exists and is
    /// not archived; a never-opened agent lands on `primary_chat_id`.
    func landingChat(of projectId: Id) -> Chat? {
        if let id = lastOpenedChat[projectId], let c = chat(id), !isChatArchived(c.id) { return c }
        return primaryChat(of: projectId)
    }

    /// The one chat an agent shows. Rule: the relay's `primary_chat` when it
    /// sends one; else the chat with the most messages, ties → the earliest
    /// created (ULID order); before any messages have loaded, the pinned
    /// ("main") chat, else the first.
    func primaryChat(of projectId: Id) -> Chat? {
        let all = chats(of: projectId)
        // Agent-hosted group chats are their own rows on Home and the "projects"
        // system chat is in the menu; the primary is a plain thread (§9b).
        let plain = all.filter { !$0.isGroup && !$0.isSystem }
        let list = plain.isEmpty ? all : plain
        guard !list.isEmpty else { return nil }
        if let pid = project(projectId)?.primaryChatId, let c = list.first(where: { $0.id == pid }) { return c }
        let counted = list.map { ($0, messages(of: $0.id).count) }
        if counted.contains(where: { $0.1 > 0 }) {
            return counted.max { a, b in a.1 != b.1 ? a.1 < b.1 : a.0.id > b.0.id }?.0
        }
        return list.first { $0.isPinned } ?? list.min { $0.id < $1.id }
    }

    /// Agent-hosted group chats (F6) across my agents — always visible, flag or not.
    var agentGroupChats: [Chat] {
        chats.values.flatMap { $0 }.filter(\.isGroup)
    }

    /// Pinned ("main") chat of a project — with multi-chat on; the primary chat otherwise.
    func mainChat(of projectId: Id) -> Chat? {
        if !multiChatEnabled { return primaryChat(of: projectId) }
        return chats(of: projectId).first { $0.isPinned } ?? chats(of: projectId).first
    }

    func project(_ id: Id) -> Project? { projects.first { $0.id == id } }

    func chat(_ id: Id) -> Chat? {
        for list in chats.values {
            if let c = list.first(where: { $0.id == id }) { return c }
        }
        return nil
    }

    func chats(of projectId: Id) -> [Chat] { chats[projectId] ?? [] }

    func messages(of chatId: Id) -> [Message] { messages[chatId] ?? [] }

    /// Paint any room doc that already has rows but no published transcript yet
    /// (checkpoint landed, catch-up still in flight). Home search reads
    /// `messages`, so this must run when the search field becomes active.
    func paintTranscriptsForSearch() {
        for (key, doc) in docs {
            guard rooms[key]?.roomChat != projectDocChatId else { continue }
            if messages[key] == nil, doc.count > 0 {
                emitSnapshot(key)
            }
        }
    }

    /// Lowercased blob of every loaded message in a chat (for home search).
    /// Falls back to a one-shot doc read when the published array is still nil.
    func searchableMessageText(of chatId: Id) -> String {
        let list: [Message]
        if let published = messages[chatId] {
            list = published
        } else if let doc = docs[chatId], doc.count > 0 {
            list = doc.readAll()
        } else {
            list = []
        }
        return list.map(\.plainText).joined(separator: "\n").lowercased()
    }

    /// S8: the last assistant message asked a question and nothing was sent since.
    func chatNeedsAnswer(_ chatId: Id) -> Bool {
        guard let last = messages(of: chatId).last, last.role == .assistant, !last.isStreaming else { return false }
        // A choice card is answered from its rows, so it does not switch the composer.
        if let q = last.openQuestion { return !q.hasChoices }
        return last.askUserText != nil
    }

    /// A turn is running: the last assistant message streams, or any recent
    /// one does. A message left `streaming` by a box that died hours ago
    /// must not pin the indicator forever.
    /// The running turn's current activity for the working line: the
    /// message's `activity`, else the latest working/step status part with a
    /// real message; nil → "<name> is working".
    func currentActivity(_ chatId: Id) -> String? {
        guard let m = messages(of: chatId).last(where: { $0.role == .assistant && $0.isStreaming }) else { return nil }
        if let a = m.activity?.trimmingCharacters(in: .whitespacesAndNewlines), !a.isEmpty { return a }
        for part in m.parts.reversed() {
            if case .status(_, let code, let text) = part, code == .working || code == .step {
                let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
                if !t.isEmpty, t.lowercased() != "working", t.lowercased() != "thinking" { return t }
            }
        }
        return nil
    }

    func isStreaming(_ chatId: Id) -> Bool {
        let list = messages(of: chatId)
        if let last = list.last(where: { $0.role == .assistant }), last.isStreaming { return true }
        let recent = nowMs() - 15 * 60 * 1000
        return list.contains { $0.role == .assistant && $0.isStreaming && $0.createdAt >= recent }
    }

    func isStreaming(project projectId: Id) -> Bool {
        chats(of: projectId).contains { isStreaming($0.id) }
    }

    // MARK: Commands (crdt-sync §6.2: the client writes rows and commands)

    /// An attachment staged in the composer, uploaded on send.
    struct PendingAttachment: Identifiable, Equatable {
        let id = UUID()
        var name: String
        var mime: String
        var data: Data
    }

    func send(chatId: Id, text: String, attachments: [PendingAttachment] = []) {
        if attachments.isEmpty {
            write(chatId: chatId, text: text, command: "send", attachments: [])
            return
        }
        guard let chat = chat(chatId) else { return }
        if isFixture {
            write(chatId: chatId, text: text, command: "send", attachments: attachments.map {
                Attachment(name: $0.name, mime: $0.mime, blobRef: nil, kind: Self.attachmentKind(mime: $0.mime, name: $0.name))
            })
            return
        }
        Task {
            var uploaded: [Attachment] = []
            for a in attachments {
                let key = AppModel.blobKey(for: a.name)
                let kind = Self.attachmentKind(mime: a.mime, name: a.name)
                do {
                    _ = try await api.putBlob(projectId: chat.projectId, key: key, mime: a.mime, data: a.data)
                    uploaded.append(Attachment(name: a.name, mime: a.mime, blobRef: key, kind: kind))
                } catch {
                    notice = Notice(text: "Attachment upload failed: \(error.localizedDescription)")
                    uploaded.append(Attachment(name: a.name, mime: a.mime, blobRef: nil, kind: kind))
                }
            }
            write(chatId: chatId, text: text, command: "send", attachments: uploaded)
        }
    }

    /// Attachment `kind` hint written into the doc (matches agentd / Mac).
    static func attachmentKind(mime: String, name: String) -> String? {
        let probe = Attachment(name: name, mime: mime, blobRef: nil)
        if probe.isImage { return "image" }
        if mime.lowercased().hasPrefix("audio/") { return "audio" }
        if mime.lowercased().hasPrefix("video/") { return "video" }
        return nil
    }

    /// Voice memo → text through the relay. The audio is only an STT input; callers send
    /// the transcript (or put it in the draft) and do not attach the file. `nil` when the
    /// relay cannot transcribe; the user sees why.
    func transcribe(projectId: Id?, memo: PendingAttachment) async -> String? {
        if isFixture { return nil }
        do {
            let text = try await api.transcribe(projectId: projectId, data: memo.data, mime: memo.mime)
            return text.isEmpty ? nil : text
        } catch {
            notice = Notice(text: "Could not transcribe the memo: \(error.localizedDescription)")
            return nil
        }
    }

    /// Blob object name for `PUT /blob/{project}/{key}`. The relay's `safe_key` only
    /// allows ASCII alnum plus `-_.:` and caps length at 128 — match Mac/`agentd`.
    static func blobKey(for name: String) -> String {
        let prefix = "att-\(newId().lowercased())-"
        let budget = max(0, 128 - prefix.count)
        return prefix + String(safeName(name).prefix(budget))
    }

    /// Strip to relay-safe ASCII (Mac `safe_name`). Swift `isLetter`/`isNumber` keep
    /// Unicode (e.g. locale photo titles like `2026年10月15日.jpg`) which the relay
    /// rejects with HTTP 400 `bad key`.
    static func safeName(_ name: String) -> String {
        let cleaned = String(name.map { c -> Character in
            if c.isASCII && (c.isLetter || c.isNumber || c == "." || c == "-" || c == "_") {
                return c
            }
            return "_"
        })
        return cleaned.isEmpty ? "file" : cleaned
    }

    /// Typed answer from the composer (S8). When the open question is a `question` part
    /// the phone cannot draw as rows (text, date, form, or plain), the answer carries its
    /// `questionId` so agentd flips the part to answered.
    func answer(chatId: Id, text: String) {
        var extra: [String: Any] = [:]
        if let q = messages(of: chatId).last?.openQuestion, !q.hasChoices {
            extra = ["questionId": q.questionId, "value": text]
            markQuestionAnswered(chatId: chatId, questionId: q.questionId, answer: [text])
        }
        write(chatId: chatId, text: text, command: "answer", attachments: [], extra: extra)
    }

    /// Answer from a question card row: the label becomes the user's bubble at once
    /// (optimistic, same words as the Mac), the card flips to answered, and the structured
    /// value travels in the `answer` command for agentd to render for the model.
    func answerQuestion(chatId: Id, questionId: String, value: Any, label: String) {
        let ids: [String] = (value as? [String]) ?? [(value as? String) ?? ""]
        markQuestionAnswered(chatId: chatId, questionId: questionId, answer: ids)
        write(chatId: chatId, text: "Answer: \(label)", command: "answer", attachments: [],
              extra: ["questionId": questionId, "value": value])
    }

    private func markQuestionAnswered(chatId: Id, questionId: String, answer: [String]) {
        guard var list = messages[chatId] else { return }
        for i in list.indices {
            for j in list[i].parts.indices {
                guard case .question(let id, var q) = list[i].parts[j], q.questionId == questionId else { continue }
                q.open = false
                q.answer = answer
                list[i].parts[j] = .question(id: id, question: q)
            }
        }
        messages[chatId] = list
    }

    private func write(chatId: Id, text: String, command: String, attachments: [Attachment], extra: [String: Any] = [:]) {
        guard let doc = docs[chatId], let chat = chat(chatId) else {
            #if DEBUG
            if isFixture { fixtureAppend(chatId: chatId, text: text, attachments: attachments) }
            #endif
            return
        }
        let messageId = newId()
        markUnacked(chatId: chatId, messageId: messageId)
        do {
            try doc.insertUserMessage(id: messageId, text: text, deviceId: deviceId, attachments: attachments)
            try doc.pushCommand(command, payload: extra.merging(["messageId": messageId]) { _, new in new }, device: deviceId)
            if !chat.isPinned && chat.title == "New chat" {
                let title = AppModel.title(from: text)
                try doc.pushCommand("retitle", payload: ["title": title], device: deviceId)
                retitleLocally(chatId: chatId, title: title)
            }
        } catch {
            clearUnacked(chatId: chatId, messageId: messageId)
            notice = Notice(text: "Could not write the message: \(error.localizedDescription)")
            return
        }
        doc.commit()
        markActivity(chatId: chatId)
        if command == "send" { announce(chatId: chatId, projectId: chat.projectId, messageId: messageId, text: text) }
        if let p = project(chat.projectId), p.state.isAsleep || p.state == .error {
            wake(projectId: p.id, reason: "send")
        }
    }

    /// §7c: tell the relay about the row we just pushed so other members'
    /// phones get a real preview instead of "New message". Best effort.
    private func announce(chatId: Id, projectId: Id?, messageId: String, text: String) {
        guard !isFixture else { return }
        let api = self.api
        Task { try? await api.notifyMessage(chatId: chatId, projectId: projectId, messageId: messageId, text: text) }
    }

    func stop(chatId: Id) {
        guard let doc = docs[chatId] else { return }
        try? doc.pushCommand("stop", payload: [:], device: deviceId)
        doc.commit()
    }

    func retry(chatId: Id, messageId: Id) {
        guard let doc = docs[chatId] else { return }
        try? doc.pushCommand("retry", payload: ["messageId": messageId], device: deviceId)
        doc.commit()
    }

    private func retitleLocally(chatId: Id, title: String) {
        for (pid, list) in chats {
            if let i = list.firstIndex(where: { $0.id == chatId }) {
                var updated = list
                updated[i].title = title
                chats[pid] = updated
                return
            }
        }
    }

    static func title(from text: String) -> String {
        let line = text.split(separator: "\n", omittingEmptySubsequences: true).first.map(String.init) ?? ""
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty { return "New chat" }
        if trimmed.count > 34 { return String(trimmed.prefix(34)) + "…" }
        return trimmed
    }

    func wake(projectId: Id, reason: String) {
        rooms[projectDocKey(projectId)]?.wake(reason: reason)
        setProjectState(projectId, .waking, detail: nil)
        machine.state = machine.coerced(.waking)
        machine.since = nowMs()
        Task {
            do {
                try await api.wake(projectId: projectId)
            } catch {
                notice = Notice(text: "Wake failed: \(error.localizedDescription)")
                setProjectState(projectId, .error, detail: error.localizedDescription)
                machine.state = .error
            }
            await refreshMachine()
        }
    }

    /// Returns the new chat once the relay has created it.
    func newChat(projectId: Id) async -> Chat? {
        do {
            let chat = try await api.createChat(projectId: projectId)
            var list = chats[projectId] ?? []
            list.append(chat)
            chats[projectId] = AppModel.sorted(list)
            ensureRoom(projectId: projectId, key: chat.id, roomChat: chat.id)
            return chat
        } catch {
            notice = Notice(text: "New chat failed: \(error.localizedDescription)")
            return nil
        }
    }

    func closeChat(_ chat: Chat) {
        guard !chat.isPinned else { return }
        chats[chat.projectId] = (chats[chat.projectId] ?? []).filter { $0.id != chat.id }
        rooms[chat.id]?.stop()
        rooms[chat.id] = nil
        docs[chat.id] = nil
        messages[chat.id] = nil
        forgetDoc(chat.id)
        Task {
            do {
                try await api.closeChat(projectId: chat.projectId, chatId: chat.id)
            } catch {
                if (error as? RelayError)?.status == 409 {
                    notice = Notice(text: "The pinned chat cannot be closed")
                } else if (error as? RelayError)?.status != 404 {
                    notice = Notice(text: "Close chat: \(error.localizedDescription)")
                }
                await refreshRegistry()
            }
        }
    }

    func markRead(chatId: Id) {
        let hadRemote = (unreadCounts[chatId] ?? 0) > 0
        if unreadCounts[chatId] != nil { unreadCounts[chatId] = 0 }
        PushManager.shared.clearDelivered(chatId: chatId)
        var hadLocal = false
        for (pid, list) in chats {
            if let i = list.firstIndex(where: { $0.id == chatId }), list[i].unread {
                var updated = list
                updated[i].unread = false
                chats[pid] = updated
                hadLocal = true
            }
        }
        if hadRemote || hadLocal {
            applyBadge()
            // §7c: PUT /chats/{id}/read takes project chat ids too; the relay
            // clears the user's events for it and badges their other devices.
            if !isFixture, chat(chatId) != nil {
                let api = self.api
                let seq = Int64(messages(of: chatId).count)
                Task { try? await api.markRead(chatId: chatId, seq: seq) }
            }
        }
    }

    func setModel(projectId: Id, model: String, effort: Effort) {
        if let doc = docs[projectDocKey(projectId)] {
            try? doc.setModelSettings(model: model, effort: effort)
            doc.commit()
        }
        if let i = projects.firstIndex(where: { $0.id == projectId }) {
            projects[i].model = model
            projects[i].effort = effort
        }
        Task {
            do {
                try await api.setModel(projectId: projectId, model: model, effort: effort)
            } catch {
                notice = Notice(text: "Model change not saved: \(error.localizedDescription)")
            }
        }
    }

    func createProject(name: String, avatar: String? = nil, avatarColor: String? = nil, color: String? = nil) {
        Task {
            do {
                try await api.createProject(name: name, avatar: avatar, avatarColor: avatarColor, color: color)
                await refreshRegistry()
            } catch {
                notice = Notice(text: "New project failed: \(error.localizedDescription)")
            }
        }
    }

    /// Grok Bot "Duplicate": a new top-level agent with the same profile, look,
    /// model, and instructions — not the chat history. Own agents only.
    func duplicateProject(_ project: Project) {
        guard !project.shared else { return }
        let name = uniqueCopyName(of: project.name)
        let avatar = avatarSlug(for: project)
        let colorName = project.avatarColor ?? avatarColorOverrides[project.id]
        let modelId = project.model
        let effort = project.effort
        Task {
            do {
                // Do not pass `models` on create: a partial map would drop the
                // relay's default subagent/decision slots. Set the talking
                // model after the copy exists.
                try await api.createProject(
                    name: name,
                    avatar: avatar,
                    avatarColor: colorName,
                    color: project.color,
                    description: project.description,
                    instructions: project.instructions
                )
                await refreshRegistry()
                if let copy = projects.first(where: { $0.name == name && !$0.shared }) {
                    if copy.model != modelId || copy.effort != effort {
                        setModel(projectId: copy.id, model: modelId, effort: effort)
                    }
                    open(project: copy)
                }
            } catch {
                notice = Notice(text: "Duplicate failed: \(error.localizedDescription)")
            }
        }
    }

    /// Soft-delete an owned agent from every list (`DELETE /projects/{id}`).
    func deleteProject(_ projectId: Id) {
        guard let p = project(projectId), !p.shared else { return }
        if path.contains(where: {
            if case .chat(_, let pid) = $0 { return pid == projectId }
            return false
        }) {
            path.removeAll()
        }
        projects.removeAll { $0.id == projectId }
        chats[projectId] = nil
        if archivedProjectIds.contains(projectId) {
            archivedProjectIds.remove(projectId)
            UserDefaults.standard.set(Array(archivedProjectIds), forKey: "archivedProjects")
        }
        unpinHome(projectId: projectId)
        Task {
            do {
                try await api.deleteProject(projectId)
                await refreshRegistry()
            } catch {
                notice = Notice(text: "Delete failed: \(error.localizedDescription)")
                await refreshRegistry()
            }
        }
    }

    /// "`Name` copy", then "`Name` copy 2"… so the relay's unique-name check passes.
    private func uniqueCopyName(of name: String) -> String {
        let owned = Set(projects.filter { !$0.shared }.map(\.name))
        let base = "\(name) copy"
        if !owned.contains(base) { return base }
        for i in 2...99 {
            let candidate = "\(name) copy \(i)"
            if !owned.contains(candidate) { return candidate }
        }
        return "\(name) copy \(String(UUID().uuidString.prefix(4)))"
    }
}
