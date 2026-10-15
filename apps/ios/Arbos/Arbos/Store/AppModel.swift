// App state: the relay registry, one Loro replica per open room, and the
// view of them the screens read. Everything runs on the main actor; Loro
// frames are ~130 bytes every 120 ms, so there is nothing to move off it.

import Foundation
import SwiftUI
import UIKit

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

@MainActor
final class AppModel: ObservableObject {
    /// The hosted relay; self-hosters change it under Advanced.
    static let defaultRelayUrl = "https://relay.arbos.life"

    @Published var appearanceSetting: String = UserDefaults.standard.string(forKey: "appearance") ?? Appearance.system.rawValue {
        didSet { UserDefaults.standard.set(appearanceSetting, forKey: "appearance") }
    }
    @Published var surfaceSetting: String = UserDefaults.standard.string(forKey: "surface") ?? Surface.matte.rawValue {
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
    @Published private(set) var peopleChats: [PeopleChat] = []
    /// Chat ids with unread rows (per device, from head_seq > read_seq and pushes).
    @Published private(set) var peopleUnread: Set<String> = []
    @Published private(set) var blocked: [Person] = []
    /// Every Arbos user the relay knows (members + invited), for the New Message sheet.
    @Published var directory: [Person] = []
    private var directoryLoadedAt: Int64 = 0
    /// The people chat currently on screen; pushes for it never notify.
    var visibleChatId: String?
    private var notifiedPushIds: Set<String> = []
    @Published private(set) var authMode: AuthMode = .github
    @Published private(set) var signingIn = false
    @Published private(set) var signInError: String?
    private let webAuth = WebAuth()
    private var refreshTask: Task<Bool, Never>?
    @Published var projects: [Project] = []
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
    @Published var path: [Route] = []
    /// True when the Debug fixture replaced the relay (screenshots).
    var isFixture = false
    /// Debug demo tours run once per process, whichever view instance starts them.
    var demoStarted = false

    enum Route: Hashable {
        case chat(chatId: Id, projectId: Id)
    }

    func open(chat: Chat) {
        path = [.chat(chatId: chat.id, projectId: chat.projectId)]
    }

    func open(project: Project) {
        if let chat = mainChat(of: project.id) {
            open(chat: chat)
        } else {
            Task { if let c = await newChat(projectId: project.id) { open(chat: c) } }
        }
    }
    @Published private(set) var machineError: String?
    @Published var notice: Notice?
    @Published private(set) var pairingBusy = false
    @Published private(set) var pairingError: String?
    @Published private(set) var registryError: String?

    private var api: RelayAPI
    private var docs: [Id: ChatDoc] = [:]
    private var rooms: [Id: Room] = [:]
    /// Client-minted message ids whose push the relay has not `ACK`ed (S3).
    private var unacked: Set<Id> = []
    private var registryTask: Task<Void, Never>?

    var appearance: Appearance { Appearance(rawValue: appearanceSetting) ?? .system }
    var surface: Surface { Surface(rawValue: surfaceSetting) ?? .matte }
    var isPaired: Bool { credentials != nil }
    var relayUrl: String { api.endpoint.httpUrl }

    init() {
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
        NotificationCenter.default.addObserver(forName: LocalNotifier.openChat, object: nil, queue: .main) { [weak self] n in
            guard let self, let cid = n.userInfo?["chat_id"] as? String else { return }
            Task { @MainActor in
                if self.peopleChat(cid) != nil { self.path = [.chat(chatId: cid, projectId: peopleSlot)] }
            }
        }
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
        unacked = []
        peopleChats = []
        peopleUnread = []
        if forget, let creds = credentials {
            Keychain.delete(relayUrl: creds.relayUrl)
        }
        credentials = nil
        link = .connecting
    }

    private func startAfterPairing() {
        link = .connecting
        Task {
            if let ids = try? await api.models(), !ids.isEmpty {
                models = ids
            }
        }
        registryTask?.cancel()
        registryTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                await self.refreshRegistry()
                await self.refreshAccount()
                await self.refreshPeople()
                await self.drainPushes()
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

    static func sorted(_ chats: [Chat]) -> [Chat] {
        chats.sorted { a, b in
            if a.isPinned != b.isPinned { return a.isPinned }
            return a.lastMessageAt > b.lastMessageAt
        }
    }

    private func projectDocKey(_ projectId: Id) -> String { "proj:\(projectId)" }

    // MARK: Rooms

    private func ensureRoom(projectId: Id, key: Id, roomChat: String) {
        guard rooms[key] == nil else { return }
        let doc = ChatDoc(key: key)
        let isProjectDoc = roomChat == projectDocChatId
        docs[key] = doc
        doc.onChange { [weak self] touched in
            Task { @MainActor in
                guard let self else { return }
                if isProjectDoc {
                    self.applyProjectDoc(projectId)
                } else if touched.listChanged {
                    self.emitSnapshot(key)
                    self.markActivity(chatId: key)
                } else if !touched.messages.isEmpty {
                    self.emitMessages(key, indexes: touched.messages)
                }
            }
        }
        let room = Room(projectId: projectId, key: key, roomChat: roomChat, doc: doc, api: api) { [weak self] in
            self?.deviceId ?? "ios:unpaired"
        }
        room.onLink = { [weak self] l in self?.link = l }
        room.onBoxState = { [weak self] state, detail in
            self?.setProjectState(projectId, state, detail: detail)
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
            guard let self, !self.unacked.isEmpty else { return }
            self.unacked.removeAll()
            if !isProjectDoc { self.emitSnapshot(key) }
        }
        rooms[key] = room
        room.start()
    }

    private func emitSnapshot(_ chatId: Id) {
        guard let doc = docs[chatId] else { return }
        var list = doc.readAll()
        for i in list.indices where list[i].role == .user {
            list[i].pending = unacked.contains(list[i].id)
        }
        messages[chatId] = list
    }

    private func emitMessages(_ chatId: Id, indexes: Set<Int>) {
        guard let doc = docs[chatId] else { return }
        var list = messages[chatId] ?? []
        var needSnapshot = false
        for ix in indexes.sorted() {
            guard var m = doc.readMessage(ix) else { continue }
            if m.role == .user { m.pending = unacked.contains(m.id) }
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

    // MARK: People chats (D72)

    var myUserId: String? { user.map { "user:\($0.id)" } }

    func peopleChat(_ id: String) -> PeopleChat? { peopleChats.first { $0.id == id } }

    func isPeopleChat(_ id: String) -> Bool { peopleChat(id) != nil }

    func refreshPeople() async {
        guard credentials != nil, !isFixture else { return }
        do {
            let list = try await api.peopleChats()
            peopleChats = list
            for c in list where c.unread { peopleUnread.insert(c.id) }
            for c in list where !c.unread && visibleChatId != c.id { peopleUnread.remove(c.id) }
            for c in list { ensureRoom(projectId: peopleSlot, key: c.id, roomChat: c.id) }
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

    /// People group (v2 on the relay; the app is ready for it).
    func createGroup(handles: [String]) async -> Bool {
        do {
            let chat = try await api.createGroup(handles: handles)
            if !peopleChats.contains(where: { $0.id == chat.id }) { peopleChats.insert(chat, at: 0) }
            ensureRoom(projectId: peopleSlot, key: chat.id, roomChat: chat.id)
            path = [.chat(chatId: chat.id, projectId: peopleSlot)]
            return true
        } catch {
            let e = error as? RelayError
            notice = Notice(text: (e?.status == 404 || e?.status == 405 || e?.status == 501) ? "Groups are not on this relay yet — DMs work today." : "Could not create the group: \(error.localizedDescription)")
            return false
        }
    }

    /// Agent profile: rename / recolour a project (`PATCH /projects/{id}`).
    func updateProject(_ projectId: Id, name: String? = nil, color: String? = nil, icon: String? = nil) {
        if let i = projects.firstIndex(where: { $0.id == projectId }) {
            if let name { projects[i].name = name }
            if let color { projects[i].color = color }
            if let icon { projects[i].icon = icon }
        }
        guard !isFixture else { return }
        Task {
            do { try await api.patchProject(projectId, name: name, color: color, icon: icon) }
            catch { notice = Notice(text: "Could not save: \(error.localizedDescription)") }
        }
    }

    /// Open (or create) the DM with a handle and navigate to it.
    func openDM(handle: String) async -> PeopleChat? {
        do {
            let chat = try await api.openDM(handle: handle)
            if !peopleChats.contains(where: { $0.id == chat.id }) { peopleChats.insert(chat, at: 0) }
            ensureRoom(projectId: peopleSlot, key: chat.id, roomChat: chat.id)
            path = [.chat(chatId: chat.id, projectId: peopleSlot)]
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
    func sendPeople(chatId: String, text: String, replyTo: String?, attachments: [PendingAttachment] = []) {
        guard let doc = docs[chatId], let chat = peopleChat(chatId), let me = user else { return }
        let mentions = AppModel.mentions(in: text, members: chat.members)
        let author = ChatDoc.Author(sender: "user:\(me.id)", handle: me.login, name: me.name, avatar: me.avatarUrl)
        let messageId = newId()
        unacked.insert(messageId)
        Task {
            var uploaded: [Attachment] = []
            for a in attachments {
                let key = "att-\(newId().lowercased())-\(AppModel.safeName(a.name))"
                do {
                    _ = try await api.putBlob(projectId: "\(peopleSlot):\(chatId)", key: key, mime: a.mime, data: a.data)
                    uploaded.append(Attachment(name: a.name, mime: a.mime, blobRef: key))
                } catch {
                    notice = Notice(text: "Attachment upload failed: \(error.localizedDescription)")
                }
            }
            do {
                try doc.insertUserMessage(id: messageId, text: text, deviceId: deviceId, attachments: uploaded,
                                          author: author, mentions: mentions, replyTo: replyTo)
                try doc.pushCommand("send", payload: ["messageId": messageId], device: deviceId)
            } catch {
                notice = Notice(text: "Could not write the message: \(error.localizedDescription)")
                return
            }
            doc.commit()
            if let i = peopleChats.firstIndex(where: { $0.id == chatId }) { peopleChats[i].lastActivityAt = nowMs() }
            peopleChats.sort { $0.lastActivityAt > $1.lastActivityAt }
        }
    }

    /// `@slug` tokens that name a member: agents by their mention slug, people by handle.
    static func mentions(in text: String, members: [ChatMember]) -> [String] {
        var out: [String] = []
        let tokens = text.split(whereSeparator: { $0.isWhitespace })
        for t in tokens where t.hasPrefix("@") {
            let slug = t.dropFirst().trimmingCharacters(in: CharacterSet(charactersIn: ".,!?:;")).lowercased()
            if slug == "arbos", members.filter(\.isAgent).count == 1, let a = members.first(where: \.isAgent) {
                if !out.contains(a.member) { out.append(a.member) }
                continue
            }
            if let m = members.first(where: { $0.handle.lowercased() == slug || ($0.isAgent && mentionSlug($0.project?.name ?? "") == slug) }) {
                if !out.contains(m.member) { out.append(m.member) }
            }
        }
        return out
    }

    /// Mark a people chat read up to the relay head (cross-launch cursor).
    func markPeopleRead(_ chatId: String) {
        peopleUnread.remove(chatId)
        guard let chat = peopleChat(chatId), chat.headSeq > chat.readSeq || chat.unread else { return }
        if let i = peopleChats.firstIndex(where: { $0.id == chatId }) {
            peopleChats[i].readSeq = chat.headSeq
            peopleChats[i].unread = false
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
        path = []
        Task { try? await api.leaveChat(chatId) }
    }

    func deletePeopleChat(_ chatId: String) {
        peopleChats.removeAll { $0.id == chatId }
        rooms[chatId]?.stop(); rooms[chatId] = nil; docs[chatId] = nil; messages[chatId] = nil
        path = []
        Task {
            do { try await api.deleteChat(chatId) } catch { notice = Notice(text: "Delete failed: \(error.localizedDescription)") }
        }
    }

    func block(_ member: ChatMember) {
        Task {
            do { try await api.block(handle: member.handle); blocked = (try? await api.blocks()) ?? blocked }
            catch { notice = Notice(text: "Block failed: \(error.localizedDescription)") }
        }
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
            switch p.kind {
            case "people_message":
                if let cid = p.payload["chat_id"], cid != visibleChatId {
                    peopleUnread.insert(cid)
                    let from = p.payload["from_handle"] ?? "someone"
                    LocalNotifier.post(id: p.id, title: "@\(from)", body: "New message", chatId: cid)
                }
            case "people_chat_created":
                needChats = true
                if let by = p.payload["by_handle"] { LocalNotifier.post(id: p.id, title: "@\(by)", body: "Started a chat with you", chatId: p.payload["chat_id"] ?? "") }
            case "people_agent_added", "people_agent_removed", "people_member_added", "people_member_removed":
                needChats = true
            case "people_project_shared":
                if let by = p.payload["by_handle"] { LocalNotifier.post(id: p.id, title: "@\(by)", body: "Shared a project with you", chatId: "") }
                await refreshRegistry()
            case "project_member_removed":
                await refreshRegistry()
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
        return URL(string: "\(base)/blob/\(projectId)/\(key)?token=\(credentials?.deviceToken ?? "")")
    }

    var authHeaders: [String: String] {
        ["Authorization": "Bearer \(credentials?.deviceToken ?? "")"]
    }

    /// Pinned ("main") chat of a project, the one the agent list opens.
    func mainChat(of projectId: Id) -> Chat? {
        chats(of: projectId).first { $0.isPinned } ?? chats(of: projectId).first
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

    /// S8: the last assistant message asked a question and nothing was sent since.
    func chatNeedsAnswer(_ chatId: Id) -> Bool {
        guard let last = messages(of: chatId).last, last.role == .assistant, !last.isStreaming else { return false }
        return last.askUserText != nil
    }

    /// A turn is running: the last assistant message streams, or any recent
    /// one does. A message left `streaming` by a box that died hours ago
    /// must not pin the indicator forever.
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
            write(chatId: chatId, text: text, command: "send", attachments: attachments.map { Attachment(name: $0.name, mime: $0.mime, blobRef: nil) })
            return
        }
        Task {
            var uploaded: [Attachment] = []
            for a in attachments {
                let key = "att-\(newId().lowercased())-\(AppModel.safeName(a.name))"
                do {
                    _ = try await api.putBlob(projectId: chat.projectId, key: key, mime: a.mime, data: a.data)
                    uploaded.append(Attachment(name: a.name, mime: a.mime, blobRef: key))
                } catch {
                    notice = Notice(text: "Attachment upload failed: \(error.localizedDescription)")
                    uploaded.append(Attachment(name: a.name, mime: a.mime, blobRef: nil))
                }
            }
            write(chatId: chatId, text: text, command: "send", attachments: uploaded)
        }
    }

    /// Voice memo → text through the relay, so the agent reads what was said (the audio
    /// still travels as an attachment). `nil` when the relay cannot transcribe; the user
    /// sees why and the memo is sent as audio only.
    func transcribe(projectId: Id?, memo: PendingAttachment) async -> String? {
        if isFixture { return nil }
        do {
            let text = try await api.transcribe(projectId: projectId, data: memo.data, filename: memo.name, mime: memo.mime)
            return text.isEmpty ? nil : text
        } catch {
            notice = Notice(text: "Could not transcribe the memo: \(error.localizedDescription)")
            return nil
        }
    }

    static func safeName(_ name: String) -> String {
        String(name.map { $0.isLetter || $0.isNumber || $0 == "." || $0 == "-" || $0 == "_" ? $0 : "_" })
    }

    func answer(chatId: Id, text: String) {
        write(chatId: chatId, text: text, command: "answer", attachments: [])
    }

    private func write(chatId: Id, text: String, command: String, attachments: [Attachment]) {
        guard let doc = docs[chatId], let chat = chat(chatId) else {
            #if DEBUG
            if isFixture { fixtureAppend(chatId: chatId, text: text, attachments: attachments) }
            #endif
            return
        }
        let messageId = newId()
        unacked.insert(messageId)
        do {
            try doc.insertUserMessage(id: messageId, text: text, deviceId: deviceId, attachments: attachments)
            try doc.pushCommand(command, payload: ["messageId": messageId], device: deviceId)
            if !chat.isPinned && chat.title == "New chat" {
                let title = AppModel.title(from: text)
                try doc.pushCommand("retitle", payload: ["title": title], device: deviceId)
                retitleLocally(chatId: chatId, title: title)
            }
        } catch {
            notice = Notice(text: "Could not write the message: \(error.localizedDescription)")
            return
        }
        doc.commit()
        markActivity(chatId: chatId)
        if let p = project(chat.projectId), p.state.isAsleep || p.state == .error {
            wake(projectId: p.id, reason: "send")
        }
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
        for (pid, list) in chats {
            if let i = list.firstIndex(where: { $0.id == chatId }), list[i].unread {
                var updated = list
                updated[i].unread = false
                chats[pid] = updated
                return
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

    func createProject(name: String) {
        Task {
            do {
                try await api.createProject(name: name)
                await refreshRegistry()
            } catch {
                notice = Notice(text: "New project failed: \(error.localizedDescription)")
            }
        }
    }
}
