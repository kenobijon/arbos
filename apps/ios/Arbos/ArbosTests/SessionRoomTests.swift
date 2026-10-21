import XCTest
@testable import Arbos
import ArbosFFI

/// Fake WebSocket for `SessionRoom`: records outbound frames, delivers inbound
/// frames (or errors) into `receive()`, and fires `onOpen` on demand.
@MainActor
final class FakeSocket: RoomSocket {
    var onOpen: (() -> Void)?
    var httpStatus: Int?
    private(set) var sent: [Data] = []
    private var receiveWaiters: [CheckedContinuation<Data, Error>] = []
    private var pending: [Result<Data, Error>] = []

    func resume() {}

    func send(_ data: Data) async throws {
        sent.append(data)
    }

    func receive() async throws -> Data {
        if !pending.isEmpty {
            return try pending.removeFirst().get()
        }
        return try await withCheckedThrowingContinuation { cont in
            receiveWaiters.append(cont)
        }
    }

    func cancel() {
        let waiters = receiveWaiters
        receiveWaiters.removeAll()
        for w in waiters {
            w.resume(throwing: URLError(.cancelled))
        }
    }

    func fireOpen() {
        onOpen?()
    }

    func deliver(_ data: Data) {
        if !receiveWaiters.isEmpty {
            receiveWaiters.removeFirst().resume(returning: data)
        } else {
            pending.append(.success(data))
        }
    }

    func failReceive(_ error: Error = URLError(.networkConnectionLost), httpStatus: Int? = nil) {
        if let httpStatus { self.httpStatus = httpStatus }
        if !receiveWaiters.isEmpty {
            receiveWaiters.removeFirst().resume(throwing: error)
        } else {
            pending.append(.failure(error))
        }
    }
}

@MainActor
final class SessionRoomTests: XCTestCase {
    func testNoHelloBeforeOpen() async throws {
        let sockets = SocketFactory()
        let room = SessionRoom(
            projectId: "proj",
            key: "chat",
            roomChat: "chat",
            doc: CoreChatDoc(key: "chat"),
            roomURL: { _ in URL(string: "wss://example.test/r")! },
            makeSocket: { sockets.make($0) },
            fetchCheckpoint: { _ in (0, Data()) },
            deviceId: { "dev:me" }
        )
        room.start()
        try await waitUntil { sockets.current != nil }
        let sock = try XCTUnwrap(sockets.current)
        XCTAssertTrue(sock.sent.isEmpty, "nothing sent before onOpen")

        sock.fireOpen()
        try await waitUntil { !sock.sent.isEmpty }

        let first = try XCTUnwrap(Frame.decode(sock.sent[0]))
        XCTAssertEqual(first.type, .hello, "first frame after open must be HELLO")
        room.stop()
    }

    func testStaleCheckpointIgnored() async throws {
        let sockets = SocketFactory()
        let fetches = FetchGate()

        let other = CoreChatDoc(key: "other")
        try other.insertUserMessage(
            id: "m1", text: "x", deviceId: "d", author: .init(sender: "u")
        )
        other.commit()
        let frontier = other.oplogVersion
        let staleBytes = try other.exportSnapshot()

        let doc = CoreChatDoc(key: "chat")
        let room = SessionRoom(
            projectId: "proj",
            key: "chat",
            roomChat: "chat",
            doc: doc,
            roomURL: { _ in URL(string: "wss://example.test/r")! },
            makeSocket: { sockets.make($0) },
            fetchCheckpoint: { _ in try await fetches.next() },
            deviceId: { "dev:me" }
        )
        room.start()
        try await waitUntil { sockets.sockets.count == 1 }
        let sock1 = sockets.sockets[0]
        sock1.fireOpen()
        try await waitUntil { !sock1.sent.isEmpty }

        let state5 = Frame(.state, header: stateHeader(checkpointSeq: 5, frontier: frontier), payload: frontier).encode()
        sock1.deliver(state5)
        try await waitUntil { fetches.waiting != nil }
        let firstFetch = try XCTUnwrap(fetches.takeWaiting())

        sock1.failReceive()
        try await waitUntil { sockets.sockets.count == 2 }
        let sock2 = sockets.sockets[1]
        sock2.fireOpen()
        try await waitUntil { !sock2.sent.isEmpty }

        let state9 = Frame(.state, header: stateHeader(checkpointSeq: 9, frontier: Data()), payload: Data()).encode()
        sock2.deliver(state9)
        try await waitUntil { fetches.waiting != nil }

        firstFetch.resume(returning: (5, staleBytes))
        // Stale resume must not import; give the cancelled task a turn.
        for _ in 0..<20 {
            await Task.yield()
            try await Task.sleep(nanoseconds: 5_000_000)
        }

        XCTAssertTrue(doc.isEmpty, "stale checkpoint must not be imported after reconnect")
        XCTAssertLessThan(room.cursor, Int64(9), "cursor must stay below the newer checkpointSeq")
        room.stop()
    }

    func testHttp403DoesNotReconnect() async throws {
        let sockets = SocketFactory()
        let room = SessionRoom(
            projectId: "people",
            key: "chat-gone",
            roomChat: "chat-gone",
            doc: CoreChatDoc(key: "chat-gone"),
            roomURL: { _ in URL(string: "wss://example.test/r")! },
            makeSocket: { sockets.make($0) },
            fetchCheckpoint: { _ in (0, Data()) },
            deviceId: { "dev:me" }
        )
        room.start()
        try await waitUntil { sockets.sockets.count == 1 }
        let sock = try XCTUnwrap(sockets.current)
        sock.failReceive(httpStatus: 403)
        try await waitUntil { room.lastError != nil }
        // Brief settle: must not dial again after the terminal 403.
        for _ in 0..<20 {
            await Task.yield()
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTAssertEqual(sockets.sockets.count, 1, "HTTP 403 must not open a second socket")
        let err = try XCTUnwrap(room.lastError)
        XCTAssertTrue(err.contains("HTTP 403"), "lastError=\(err)")
        room.stop()
    }

    func testPlainReceiveErrorStillReconnects() async throws {
        let sockets = SocketFactory()
        let room = SessionRoom(
            projectId: "proj",
            key: "chat",
            roomChat: "chat",
            doc: CoreChatDoc(key: "chat"),
            roomURL: { _ in URL(string: "wss://example.test/r")! },
            makeSocket: { sockets.make($0) },
            fetchCheckpoint: { _ in (0, Data()) },
            deviceId: { "dev:me" }
        )
        room.start()
        try await waitUntil { sockets.sockets.count == 1 }
        let sock = try XCTUnwrap(sockets.current)
        sock.fireOpen()
        try await waitUntil { !sock.sent.isEmpty }
        sock.failReceive()
        try await waitUntil { sockets.sockets.count == 2 }
        room.stop()
    }

    // MARK: - Helpers

    private func stateHeader(checkpointSeq: Int64, frontier: Data) -> [String: Any] {
        [
            "headSeq": Int64(10),
            "seqFloor": Int64(0),
            "checkpointSeq": checkpointSeq,
            "checkpointSize": Int64(frontier.count),
            "rowCount": Int64(0),
            "rowBytes": Int64(0),
            "box": "awake",
        ]
    }

    private func waitUntil(
        timeout: TimeInterval = 3,
        file: StaticString = #filePath,
        line: UInt = #line,
        _ predicate: @escaping @MainActor () -> Bool
    ) async throws {
        let deadline = Date().addingTimeInterval(timeout)
        while !predicate() {
            if Date() > deadline {
                XCTFail("timed out waiting", file: file, line: line)
                throw WaitTimeout()
            }
            await Task.yield()
            try await Task.sleep(nanoseconds: 10_000_000)
        }
    }
}

private struct WaitTimeout: Error {}

@MainActor
private final class SocketFactory {
    private(set) var sockets: [FakeSocket] = []
    var current: FakeSocket? { sockets.last }

    func make(_ url: URL) -> RoomSocket {
        let s = FakeSocket()
        sockets.append(s)
        return s
    }
}

@MainActor
private final class FetchGate {
    private(set) var waiting: CheckedContinuation<(Int64, Data), Error>?

    func next() async throws -> (Int64, Data) {
        try await withCheckedThrowingContinuation { cont in
            waiting = cont
        }
    }

    func takeWaiting() -> CheckedContinuation<(Int64, Data), Error>? {
        let c = waiting
        waiting = nil
        return c
    }
}
