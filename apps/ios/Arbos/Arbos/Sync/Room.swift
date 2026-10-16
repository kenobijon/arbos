// One WebSocket per room on `arbos-relay` (crdt-sync §7–§8): dial, HELLO,
// STATE, checkpoint-on-connect, ROWS_REQ backfill, then live ROW / PUSH /
// ACK / STATUS / WAKE, forever, with backoff. The Loro doc lives in `ChatDoc`;
// this type only moves bytes.

import Foundation

@MainActor
final class Room {
    let projectId: Id
    let key: Id
    let roomChat: String
    let doc: ChatDoc

    var onLink: ((Link) -> Void)?
    var onBoxState: ((BoxState, String?) -> Void)?
    var onCaughtUp: (() -> Void)?
    var onAcked: (() -> Void)?
    /// Another peer's presence header (typing, `in_call`, …).
    var onPresence: (([String: Any]) -> Void)?
    /// My user id + handle, carried in presence so peers can show "online / last seen".
    var presenceUser: () -> (id: String, handle: String)? = { nil }
    /// The call id this device is in for this chat; sent as `in_call` in presence.
    var inCall: String? {
        didSet { if inCall != oldValue { Task { [weak self] in await self?.sendPresence() } } }
    }

    private let api: RelayAPI
    private let deviceId: () -> String
    /// Last relay seq applied to the doc; restored from the on-device store
    /// so a relaunch asks only for rows after it.
    private(set) var cursor: Int64 = 0
    /// Pushes the relay has not `ACK`ed yet: `(batchId, bytes)`. Persisted so
    /// a message written just before the app was killed still reaches the relay.
    private var pending: [(String, Data)] = []
    /// The pending list changed (push or ACK): the owner persists it.
    var onPendingChanged: (([(String, Data)]) -> Void)?
    /// The cursor moved without a doc change (ROWS_DONE, ACK): save point.
    var onCursorMoved: (() -> Void)?
    private var socket: URLSessionWebSocketTask?
    private var loop: Task<Void, Never>?
    private var presence: Task<Void, Never>?
    private var wakeQueue: [String] = []
    private let session: URLSession

    /// The chat is on screen *and* the app is active: presence carries
    /// `focused_chat` so the relay does not push a notification for it.
    /// Cleared when the scene leaves `.active` (lock / home) so APNs can fire.
    var focused = false {
        didSet {
            guard focused != oldValue else { return }
            Task { [weak self] in await self?.sendPresence() }
        }
    }

    private static let helloDeadline: Double = 15
    private static let presenceEvery: Double = 15
    private static let backoffMin: Double = 0.25
    private static let backoffMax: Double = 16
    private static let backoffResetAfter: Double = 30

    init(projectId: Id, key: Id, roomChat: String, doc: ChatDoc, api: RelayAPI,
         cursor: Int64 = 0, pending: [(String, Data)] = [], deviceId: @escaping () -> String) {
        self.projectId = projectId
        self.key = key
        self.roomChat = roomChat
        self.doc = doc
        self.api = api
        self.cursor = cursor
        self.pending = pending
        self.deviceId = deviceId
        let config = URLSessionConfiguration.default
        config.waitsForConnectivity = true
        session = URLSession(configuration: config)
        doc.onLocalUpdate { [weak self] bytes in
            Task { @MainActor in self?.push(bytes) }
        }
    }

    func start() {
        guard loop == nil else { return }
        loop = Task { [weak self] in
            await self?.run()
        }
    }

    func stop() {
        loop?.cancel()
        loop = nil
        presence?.cancel()
        socket?.cancel(with: .goingAway, reason: nil)
        socket = nil
    }

    /// `WAKE{reason}` on the project doc room (§8.4); queued until the socket is up.
    func wake(reason: String) {
        wakeQueue.append(reason)
        flushWake()
    }

    // MARK: Session loop

    private func run() async {
        var backoff = Room.backoffMin
        while !Task.isCancelled {
            let connectedAt = Date()
            do {
                try await runSession()
            } catch {
                if Task.isCancelled { return }
                NSLog("relay room \(projectId)/\(roomChat): \(error.localizedDescription)")
            }
            presence?.cancel()
            socket?.cancel(with: .goingAway, reason: nil)
            socket = nil
            onLink?(.reconnecting)
            if Date().timeIntervalSince(connectedAt) > Room.backoffResetAfter {
                backoff = Room.backoffMin
            }
            try? await Task.sleep(nanoseconds: UInt64(backoff * 1_000_000_000))
            backoff = min(backoff * 2, Room.backoffMax)
        }
    }

    private func runSession() async throws {
        let room = roomName(projectId, roomChat)
        guard let url = api.roomURL(room: room) else {
            throw RelayError(status: 0, message: "bad room URL")
        }
        let ws = session.webSocketTask(with: url)
        ws.maximumMessageSize = rowMax + 8192
        socket = ws
        ws.resume()

        try await send(Frame(.hello, header: [
            "cursor": cursor,
            "device": deviceId(),
            "room": room,
            "proto": protoVersion,
        ]))

        // A local push can race the handshake (user sends while we reconnect).
        // Apply ACKs here so a batch is not left in `pending` forever when the
        // only ACK arrived before STATE and the relay later treats a resend as dup.
        let state = try await withTimeout(Room.helloDeadline) { [self] in
            while true {
                let frame = try await self.receive(ws)
                if frame.type == .state { return frame }
                if frame.type == .ack { self.applyAck(frame) }
            }
        }
        if let b = state.string("box").flatMap(BoxState.fromWire) {
            onBoxState?(b, nil)
        }
        let checkpointSeq = state.int("checkpointSeq") ?? 0

        // Checkpoint-on-connect (§7.3). A replica restored from the on-device
        // store usually already contains the checkpoint frontier: then the rows
        // the relay compacted away are ours already and only rows after
        // `max(cursor, checkpointSeq)` are needed. Otherwise fetch the checkpoint.
        if checkpointSeq > 0 {
            if doc.contains(frontier: state.payload) {
                cursor = max(cursor, checkpointSeq)
            } else {
                do {
                    let (seq, bytes) = try await api.checkpoint(room: room)
                    try doc.importUpdate(bytes)
                    cursor = max(cursor, max(seq, checkpointSeq))
                } catch {
                    NSLog("relay room \(room): checkpoint failed: \(error.localizedDescription)")
                }
            }
        }

        // Own rows are requested too: a row pushed by this device but saved
        // before its ACK is only on the relay (a people chat has no box to re-publish them).
        try await send(Frame(.rowsReq, header: ["after": cursor, "excludeOwn": false]))
        for (batchId, bytes) in pending {
            try await send(Frame(.push, header: ["batchId": batchId], payload: bytes))
        }
        flushWake()

        presence?.cancel()
        presence = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: UInt64(Room.presenceEvery * 1_000_000_000))
                guard let self, !Task.isCancelled else { return }
                await self.sendPresence()
            }
        }

        while !Task.isCancelled {
            let frame = try await receive(ws)
            switch frame.type {
            case .row:
                do {
                    try doc.importUpdate(frame.payload)
                } catch {
                    NSLog("relay room \(room): import failed: \(error.localizedDescription)")
                }
                cursor = max(cursor, frame.int("seq") ?? cursor)
            case .rowsDone:
                cursor = max(cursor, frame.int("headSeq") ?? cursor)
                onLink?(.live)
                onCaughtUp?()
                onCursorMoved?()
            case .ack:
                applyAck(frame)
            case .status:
                if let b = frame.string("box").flatMap(BoxState.fromWire) {
                    onBoxState?(b, frame.string("detail"))
                }
            case .error:
                let code = frame.string("code") ?? ""
                if code == "lagging" {
                    try await send(Frame(.rowsReq, header: ["after": cursor, "excludeOwn": false]))
                } else {
                    throw RelayError(status: 0, message: "relay error \(code): \(frame.string("message") ?? "")")
                }
            case .presence:
                if frame.string("device") != deviceId() { onPresence?(frame.header) }
            case .hello, .state, .rowsReq, .push, .probe, .probeOk, .wake:
                break
            }
        }
    }

    private func receive(_ ws: URLSessionWebSocketTask) async throws -> Frame {
        while true {
            let message = try await ws.receive()
            switch message {
            case .data(let data):
                if let frame = Frame.decode(data) { return frame }
            case .string(let s):
                if let data = Data(base64Encoded: s), let frame = Frame.decode(data) { return frame }
            @unknown default:
                continue
            }
        }
    }

    private func sendPresence() async {
        var header: [String: Any] = ["device": deviceId(), "at": nowMs()]
        if let u = presenceUser() { header["user"] = u.id; header["handle"] = u.handle }
        if focused { header["focused_chat"] = roomChat }
        if let inCall { header["in_call"] = inCall }
        try? await send(Frame(.presence, header: header))
    }

    private func send(_ frame: Frame) async throws {
        guard let ws = socket else { throw RelayError(status: 0, message: "socket closed") }
        try await ws.send(.data(frame.encode()))
    }

    private func applyAck(_ frame: Frame) {
        let batchId = frame.string("batchId") ?? ""
        let before = pending.count
        pending.removeAll { $0.0 == batchId }
        cursor = max(cursor, frame.int("seq") ?? cursor)
        onAcked?()
        if pending.count != before { onPendingChanged?(pending) }
        onCursorMoved?()
    }

    private func push(_ bytes: Data) {
        if bytes.count > rowMax {
            NSLog("relay room \(roomChat): local update \(bytes.count) bytes exceeds row cap; dropped")
            return
        }
        let batchId = newId()
        pending.append((batchId, bytes))
        onPendingChanged?(pending)
        let frame = Frame(.push, header: ["batchId": batchId], payload: bytes)
        Task { [weak self] in
            guard let self else { return }
            do {
                try await self.send(frame)
            } catch {
                // Keep the batch in `pending` and drop the socket so `run()`
                // reconnects and re-sends it. A silent `try?` left messages
                // stuck faint (S3 pending) until some other read error reconnects.
                NSLog("relay room \(self.roomChat): push failed: \(error.localizedDescription)")
                self.socket?.cancel(with: .goingAway, reason: nil)
                self.socket = nil
            }
        }
    }

    private func flushWake() {
        guard socket != nil, !wakeQueue.isEmpty else { return }
        let reasons = wakeQueue
        wakeQueue.removeAll()
        Task { [weak self] in
            for reason in reasons {
                try? await self?.send(Frame(.wake, header: ["reason": reason]))
            }
        }
    }
}

struct TimeoutError: Error, LocalizedError {
    var errorDescription: String? { "timed out" }
}

func withTimeout<T>(_ seconds: Double, _ body: @escaping () async throws -> T) async throws -> T {
    try await withThrowingTaskGroup(of: T.self) { group in
        group.addTask { try await body() }
        group.addTask {
            try await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
            throw TimeoutError()
        }
        guard let first = try await group.next() else { throw TimeoutError() }
        group.cancelAll()
        return first
    }
}
