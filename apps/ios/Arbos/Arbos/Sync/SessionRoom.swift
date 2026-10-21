// Driven by ArbosFFI.RoomSession over URLSession (crdt-sync §7.2a).
// The Loro doc stays in loro-swift (`ChatDoc`); this type only moves bytes and actions.

import Foundation
import ArbosFFI

/// Test seam for the WebSocket: production uses `URLSessionWebSocketTask`.
protocol RoomSocket: AnyObject {
    var onOpen: (() -> Void)? { get set }
    func resume()
    func send(_ data: Data) async throws
    func receive() async throws -> Data
    func cancel()
}

@MainActor
final class SessionRoom {
    let projectId: Id
    let key: Id
    let roomChat: String
    let doc: ChatDoc

    var onLink: ((Link) -> Void)?
    var onBoxState: ((BoxState, String?) -> Void)?
    var onCaughtUp: (() -> Void)?
    var onAcked: (() -> Void)?
    var onDocPaint: (() -> Void)?
    var onPresence: (([String: Any]) -> Void)?
    var presenceUser: () -> (id: String, handle: String)? = { nil }
    var inCall: String? {
        didSet {
            guard inCall != oldValue else { return }
            Task { [weak self] in self?.sendPresence() }
        }
    }

    private(set) var cursor: Int64 = 0
    /// How many times the session asked us to reconnect (test surface).
    private(set) var reconnects = 0
    /// How many `state_timeout` session errors we saw (test surface).
    private(set) var stateTimeouts = 0
    var onPendingChanged: (([(String, Data)]) -> Void)?
    var onCursorMoved: (() -> Void)?

    var focused = false {
        didSet {
            guard focused != oldValue else { return }
            Task { [weak self] in self?.sendPresence() }
        }
    }

    private let deviceId: () -> String
    private let roomURL: (String) -> URL?
    private let makeSocket: (URL) -> RoomSocket
    private let fetchCheckpoint: (String) async throws -> (Int64, Data)

    private let session: ArbosFFI.RoomSession
    private var urlSession: URLSession?
    private var socket: RoomSocket?
    private var generation = 0
    private var started = false
    private var stopped = false
    private var isDraining = false

    private var receiveTask: Task<Void, Never>?
    private var tickTask: Task<Void, Never>?
    private var presenceTask: Task<Void, Never>?
    private var reconnectTask: Task<Void, Never>?
    private var checkpointTask: Task<Void, Never>?
    private var sendChain: Task<Void, Never>?

    /// Production path: wiring `RelayAPI` into the three closures the
    /// designated init uses for tests.
    convenience init(
        projectId: Id, key: Id, roomChat: String, doc: ChatDoc, api: RelayAPI,
        cursor: Int64 = 0, pending: [(String, Data)] = [], deviceId: @escaping () -> String
    ) {
        let openDelegate = RoomSocketOpenDelegate()
        let config = URLSessionConfiguration.default
        config.waitsForConnectivity = true
        let urlSession = URLSession(configuration: config, delegate: openDelegate, delegateQueue: nil)
        self.init(
            projectId: projectId,
            key: key,
            roomChat: roomChat,
            doc: doc,
            roomURL: { api.roomURL(room: $0) },
            makeSocket: { url in
                let task = urlSession.webSocketTask(with: url)
                task.maximumMessageSize = rowMax + 8192
                let socket = URLSessionRoomSocket(task: task)
                openDelegate.socket = socket
                return socket
            },
            fetchCheckpoint: { try await api.checkpoint(room: $0) },
            cursor: cursor,
            pending: pending,
            deviceId: deviceId
        )
        self.urlSession = urlSession
    }

    /// Designated init: inject URL / socket / checkpoint for tests.
    init(
        projectId: Id, key: Id, roomChat: String, doc: ChatDoc,
        roomURL: @escaping (String) -> URL?,
        makeSocket: @escaping (URL) -> RoomSocket,
        fetchCheckpoint: @escaping (String) async throws -> (Int64, Data),
        cursor: Int64 = 0, pending: [(String, Data)] = [],
        deviceId: @escaping () -> String
    ) {
        self.projectId = projectId
        self.key = key
        self.roomChat = roomChat
        self.doc = doc
        self.roomURL = roomURL
        self.makeSocket = makeSocket
        self.fetchCheckpoint = fetchCheckpoint
        self.cursor = cursor
        self.deviceId = deviceId
        let pushes = pending.map { ArbosFFI.PendingPush(batchId: $0.0, bytes: $0.1) }
        self.session = ArbosFFI.RoomSession(
            deviceId: deviceId(),
            room: roomName(projectId, roomChat),
            cursor: cursor,
            pending: pushes
        )
        doc.onLocalUpdate { [weak self] bytes in
            Task { @MainActor in
                self?.session.onLocalCommit(bytes: bytes)
                self?.drain()
            }
        }
    }

    func start() {
        guard !started else { return }
        started = true
        connect()
    }

    func stop() {
        stopped = true
        generation += 1
        cancelTasks()
        socket?.cancel()
        socket = nil
        urlSession?.invalidateAndCancel()
        urlSession = nil
    }

    deinit {
        urlSession?.invalidateAndCancel()
    }

    func wake(reason: String) {
        session.wake(reason: reason)
        drain()
    }

    // MARK: - Connect

    private func connect() {
        guard !stopped else { return }
        generation += 1
        let gen = generation
        cancelTasks()
        socket?.cancel()
        socket = nil

        let room = roomName(projectId, roomChat)
        guard let url = roomURL(room) else {
            session.onClose(nowMs: sessionNowMs())
            drain()
            return
        }

        let sock = makeSocket(url)
        socket = sock
        sock.onOpen = { [weak self] in
            Task { @MainActor [weak self] in
                guard let self, self.generation == gen else { return }
                self.handleOpen(gen: gen)
            }
        }
        sock.resume()
        startReceive(gen: gen, socket: sock)
    }

    private func handleOpen(gen: Int) {
        session.onOpen(nowMs: sessionNowMs())
        // Presence right after onOpen — core holds it until Live.
        sendPresence()
        tickTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 15_000_000_000)
            guard let self, self.generation == gen, !Task.isCancelled else { return }
            self.session.onTick(nowMs: self.sessionNowMs())
            self.drain()
        }
        presenceTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 15_000_000_000)
                guard let self, self.generation == gen, !Task.isCancelled else { return }
                self.sendPresence()
            }
        }
    }

    private func startReceive(gen: Int, socket: RoomSocket) {
        receiveTask = Task { [weak self] in
            while !Task.isCancelled {
                do {
                    let data = try await socket.receive()
                    guard let self, self.generation == gen else { return }
                    self.session.onFrame(bytes: data)
                    self.drain()
                } catch {
                    guard let self, self.generation == gen else { return }
                    self.session.onClose(nowMs: self.sessionNowMs())
                    self.drain()
                    return
                }
            }
        }
    }

    // MARK: - Drain

    private func drain() {
        guard !isDraining else { return }
        isDraining = true
        defer { isDraining = false }
        while true {
            let actions = session.drainActions()
            if actions.isEmpty { return }
            for action in actions {
                apply(action)
            }
        }
    }

    private func apply(_ action: ArbosFFI.RoomAction) {
        switch action {
        case .sendFrame(let bytes):
            enqueueSend(bytes)
        case .fetchCheckpoint(let room, let frontier, _):
            applyFetchCheckpoint(room: room, frontier: frontier)
        case .applyBytes(let bytes):
            do {
                try doc.importUpdate(bytes)
            } catch {
                NSLog("relay room \(roomChat): import failed: \(error.localizedDescription)")
            }
        case .cursor(let seq):
            cursor = seq
        case .pending(let batches):
            onPendingChanged?(batches.map { ($0.batchId, $0.bytes) })
        case .boxState(let state, let detail, _):
            onBoxState?(mapBoxState(state), detail)
        case .linkLive:
            onLink?(.live)
            onCaughtUp?()
            onCursorMoved?()
        case .scheduleReconnect(let afterMs):
            applyScheduleReconnect(afterMs: afterMs)
        case .sessionError(let code, let message):
            if code == "state_timeout" { stateTimeouts += 1 }
            NSLog("relay room \(roomChat): error \(code): \(message)")
        case .presence(_, _, _, _, let mine, let headerJson):
            guard !mine else { return }
            if let data = headerJson.data(using: .utf8),
               let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
                onPresence?(obj)
            }
        case .droppedOversize(let bytes):
            NSLog("relay room \(roomChat): local update \(bytes) bytes exceeds row cap; dropped")
        case .ack(_, _):
            onAcked?()
            onCursorMoved?()
        }
    }

    private func applyFetchCheckpoint(room: String, frontier: Data) {
        if doc.contains(frontier: frontier) {
            session.onCheckpointSkipped()
            if !doc.isEmpty { onDocPaint?() }
            return
        }
        let gen = generation
        checkpointTask?.cancel()
        checkpointTask = Task { [weak self] in
            guard let self else { return }
            do {
                let (seq, bytes) = try await self.fetchCheckpoint(room)
                guard self.generation == gen, !Task.isCancelled else { return }
                try self.doc.importUpdate(bytes)
                guard self.generation == gen else { return }
                self.session.onCheckpoint(seq: seq)
                self.onDocPaint?()
                self.drain()
            } catch {
                guard self.generation == gen, !Task.isCancelled else { return }
                NSLog("relay room \(room): checkpoint failed: \(error.localizedDescription)")
                self.session.onCheckpointFailed()
                self.drain()
            }
        }
    }

    private func applyScheduleReconnect(afterMs: UInt64) {
        reconnects += 1
        onLink?(.reconnecting)
        checkpointTask?.cancel()
        checkpointTask = nil
        tickTask?.cancel()
        tickTask = nil
        presenceTask?.cancel()
        presenceTask = nil
        receiveTask?.cancel()
        receiveTask = nil
        sendChain?.cancel()
        sendChain = nil
        socket?.cancel()
        socket = nil
        reconnectTask?.cancel()
        reconnectTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: afterMs * 1_000_000)
            guard let self, !self.stopped, !Task.isCancelled else { return }
            self.session.prepareReconnect()
            self.connect()
        }
    }

    // MARK: - Send / presence

    private func enqueueSend(_ bytes: Data) {
        let gen = generation
        let previous = sendChain
        sendChain = Task { [weak self] in
            await previous?.value
            guard let self, self.generation == gen, !Task.isCancelled else { return }
            do {
                try await self.socket?.send(bytes)
            } catch {
                guard self.generation == gen else { return }
                self.session.onClose(nowMs: self.sessionNowMs())
                self.drain()
            }
        }
    }

    private func sendPresence() {
        var header: [String: Any] = ["device": deviceId(), "at": nowMs()]
        if let u = presenceUser() { header["user"] = u.id; header["handle"] = u.handle }
        if focused { header["focused_chat"] = roomChat }
        if let inCall { header["in_call"] = inCall }
        if let data = try? JSONSerialization.data(withJSONObject: header),
           let json = String(data: data, encoding: .utf8) {
            session.presence(presenceHeader: json)
        }
        drain()
    }

    private func mapBoxState(_ state: ArbosFFI.BoxState) -> BoxState {
        switch state {
        case .awake: return .awake
        case .waking: return .waking
        case .asleep: return .hibernatedLight
        case .error: return .error
        }
    }

    private func sessionNowMs() -> UInt64 {
        UInt64(Date().timeIntervalSince1970 * 1000)
    }

    private func cancelTasks() {
        reconnectTask?.cancel()
        reconnectTask = nil
        checkpointTask?.cancel()
        checkpointTask = nil
        tickTask?.cancel()
        tickTask = nil
        presenceTask?.cancel()
        presenceTask = nil
        receiveTask?.cancel()
        receiveTask = nil
        sendChain?.cancel()
        sendChain = nil
    }
}

// MARK: - URLSession socket

/// Separate NSObject so `didOpenWithProtocol` can fire `onOpen` without the
/// socket retaining the session. Held weakly from here.
private final class RoomSocketOpenDelegate: NSObject, URLSessionWebSocketDelegate {
    weak var socket: URLSessionRoomSocket?

    func urlSession(
        _ session: URLSession,
        webSocketTask: URLSessionWebSocketTask,
        didOpenWithProtocol protocol: String?
    ) {
        // Only the open for the current socket — an older cancelled task must
        // not start the session for a newer one.
        guard webSocketTask === socket?.task else { return }
        socket?.handleDidOpen()
    }
}

private final class URLSessionRoomSocket: RoomSocket {
    var onOpen: (() -> Void)?
    fileprivate let task: URLSessionWebSocketTask

    init(task: URLSessionWebSocketTask) {
        self.task = task
    }

    fileprivate func handleDidOpen() {
        onOpen?()
    }

    func resume() {
        task.resume()
    }

    func send(_ data: Data) async throws {
        try await task.send(.data(data))
    }

    func receive() async throws -> Data {
        while true {
            let message = try await task.receive()
            switch message {
            case .data(let data):
                return data
            case .string(let s):
                if let data = Data(base64Encoded: s) { return data }
            @unknown default:
                continue
            }
        }
    }

    func cancel() {
        task.cancel(with: .goingAway, reason: nil)
    }
}
