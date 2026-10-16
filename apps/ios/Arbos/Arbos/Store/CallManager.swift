// Calls v1: one LiveKit room per call, CallKit for the native in-call UI,
// audio session and lock-screen controls, PushKit for ringing when the app
// is closed (switches on when the relay has an APNs key), and ringing from
// `/me/pushes` `call_started` while the app is open. The relay owns the
// call record; this type owns the media and the CallKit mirror of it.
//
// Lifecycle (contract §Adjustments): start → POST …/calls (201 = we started
// it → write "<name> started a … call"), connect; accept → POST
// /calls/{id}/join, connect; hang up → disconnect, POST /calls/{id}/leave →
// `{ended, duration_s}`; the client told `ended: true` writes "Call ended ·
// N min". A `call_ended` push names the `writer_device` for relay-ended
// calls.

import Foundation
import UIKit
import AVFoundation
import CallKit
import PushKit
import LiveKit
import os

private let log = Logger(subsystem: "com.unarbos.arbos.ios", category: "calls")

/// A remote participant as the UI needs it.
struct CallTile: Identifiable, Equatable {
    var id: String
    var person: CallPerson
    var isSpeaking: Bool
    var micOn: Bool
    var cameraOn: Bool
    var videoTrack: VideoTrack?

    static func == (a: CallTile, b: CallTile) -> Bool {
        a.id == b.id && a.person == b.person && a.isSpeaking == b.isSpeaking && a.micOn == b.micOn && a.cameraOn == b.cameraOn
            && (a.videoTrack?.sid == b.videoTrack?.sid)
    }
}

@MainActor
final class CallManager: NSObject, ObservableObject {
    static let shared = CallManager()

    enum Phase: Equatable { case idle, ringing, connecting, connected, reconnecting }

    @Published private(set) var phase: Phase = .idle
    @Published private(set) var call: Call?
    /// An incoming call ringing on this device (CallKit mirrors it).
    @Published private(set) var incoming: (call: Call, from: CallPerson?)?
    @Published private(set) var tiles: [CallTile] = []
    @Published private(set) var connectedAt: Date?
    @Published private(set) var error: String?
    @Published private(set) var muted = false
    @Published private(set) var cameraOn = false
    @Published private(set) var speakerOn = false
    @Published private(set) var frontCamera = true
    /// Full-screen call UI wanted (the root view presents it).
    @Published var isPresented = false
    /// `push` from the VoIP registration: false until the relay has an APNs key.
    @Published private(set) var voipPushEnabled: Bool?
    /// Active calls per chat (for the "On a call · Join" bar), from pushes, presence and GET /chats/{id}/call.
    @Published private(set) var activeByChat: [String: Call] = [:]

    let room = LiveKit.Room()
    var localVideoTrack: VideoTrack? { room.localParticipant.firstCameraVideoTrack }

    // Wiring from AppModel.
    var api: RelayAPI?
    var deviceId: () -> String = { "" }
    var me: () -> CallPerson? = { nil }
    var writeSystemLine: ((_ chatId: String, _ projectId: String, _ text: String, _ callId: String) -> Void)?
    var isFixture = false

    private let provider: CXProvider
    private let callController = CXCallController()
    private var cxUUID: UUID?
    private var voipRegistry: PKPushRegistry?
    private var voipToken: String?
    private var registeredVoipToken: String?
    private var ringTimeout: Task<Void, Never>?
    /// Outgoing wait: play a local ringback until someone joins (or we hang up).
    private var ringbackWanted = false
    private var ringbackPlayer: AVAudioPlayer?

    private override init() {
        let config = CXProviderConfiguration()
        config.supportsVideo = true
        config.maximumCallsPerCallGroup = 1
        config.maximumCallGroups = 1
        config.supportedHandleTypes = [.generic]
        config.includesCallsInRecents = true
        if let icon = UIImage(named: "AppIcon") ?? UIImage(systemName: "phone.fill") { config.iconTemplateImageData = icon.pngData() }
        provider = CXProvider(configuration: config)
        super.init()
        provider.setDelegate(self, queue: nil)
        room.add(delegate: self)
    }

    var isActive: Bool { phase == .connecting || phase == .connected || phase == .reconnecting }
    var duration: TimeInterval { connectedAt.map { Date().timeIntervalSince($0) } ?? 0 }

    // MARK: Permissions

    static func micGranted() async -> Bool {
        switch AVAudioApplication.shared.recordPermission {
        case .granted: return true
        case .denied: return false
        default: return await AVAudioApplication.requestRecordPermission()
        }
    }

    static func cameraGranted() async -> Bool {
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized: return true
        case .denied, .restricted: return false
        default: return await AVCaptureDevice.requestAccess(for: .video)
        }
    }

    // MARK: Start / join / accept / leave

    /// From the chat header: start (201) or join (200) the chat's call.
    func start(chatId: String, projectId: String?, kind: CallKind, title: String) async {
        guard !isActive else { isPresented = true; return }
        guard await CallManager.micGranted() else { error = "Microphone access is off in Settings."; return }
        if kind == .video { _ = await CallManager.cameraGranted() }
        guard let api else { return }
        error = nil
        phase = .connecting
        isPresented = true
        ringbackWanted = true
        syncRingback()
        do {
            let (started, isNew) = try await api.startCall(chatId: chatId, projectId: projectId, kind: kind)
            call = started
            if isNew, let who = me() {
                writeSystemLine?(chatId, started.projectId, "\(who.displayName) started a \(kind == .video ? "video" : "voice") call", started.callId)
            }
            activeByChat[chatId] = started
            reportOutgoing(title: title, video: kind == .video)
            try await connect(started, video: kind == .video)
        } catch {
            log.error("start failed: \(error.localizedDescription, privacy: .public)")
            self.error = CallManager.describe(error)
            await teardown(leave: false)
        }
    }

    /// From the "On a call · Join" bar.
    func join(_ existing: Call, title: String) async {
        guard !isActive, let api else { isPresented = true; return }
        guard await CallManager.micGranted() else { error = "Microphone access is off in Settings."; return }
        error = nil
        phase = .connecting
        isPresented = true
        ringbackWanted = true
        syncRingback()
        do {
            let fresh = try await api.joinCall(existing.callId)
            call = fresh
            reportOutgoing(title: title, video: fresh.kind == .video)
            try await connect(fresh, video: fresh.kind == .video)
        } catch {
            self.error = CallManager.describe(error)
            await teardown(leave: false)
        }
    }

    /// Ring: a `call_started` push while the app is open, or a VoIP push.
    func ring(_ incomingCall: Call, from: CallPerson?, viaVoip: Bool) {
        guard !isActive, incoming == nil, call?.callId != incomingCall.callId else { return }
        incoming = (incomingCall, from)
        #if DEBUG
        if DebugHooks.autoAcceptCall {
            Task { try? await Task.sleep(nanoseconds: 1_500_000_000); await self.accept() }
        }
        #endif
        phase = .ringing
        activeByChat[incomingCall.chatId] = incomingCall
        let uuid = UUID()
        cxUUID = uuid
        let update = CXCallUpdate()
        update.remoteHandle = CXHandle(type: .generic, value: from?.displayName ?? incomingCall.startedBy?.displayName ?? "Arbos")
        update.localizedCallerName = from?.displayName ?? incomingCall.startedBy?.displayName ?? "Arbos"
        update.hasVideo = incomingCall.kind == .video
        update.supportsHolding = false
        update.supportsGrouping = false
        update.supportsUngrouping = false
        update.supportsDTMF = false
        provider.reportNewIncomingCall(with: uuid, update: update) { [weak self] err in
            if let err {
                log.error("reportNewIncomingCall: \(err.localizedDescription, privacy: .public)")
                Task { @MainActor in self?.incoming = nil; self?.phase = .idle }
            }
        }
        ringTimeout?.cancel()
        ringTimeout = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 60_000_000_000)
            guard let self, !Task.isCancelled, self.phase == .ringing else { return }
            self.endIncoming(reason: .unanswered)
        }
        if viaVoip { Task { await verifyStillLive(incomingCall.callId) } }
    }

    /// A VoIP-woken phone checks the call did not end in the meantime.
    private func verifyStillLive(_ callId: String) async {
        guard let api, let live = try? await api.call(callId) else { return }
        if live.ended { endIncoming(reason: .remoteEnded) }
    }

    func accept() async {
        guard let inc = incoming, let api else { return }
        ringTimeout?.cancel()
        guard await CallManager.micGranted() else { error = "Microphone access is off in Settings."; endIncoming(reason: .failed); return }
        if inc.call.kind == .video { _ = await CallManager.cameraGranted() }
        incoming = nil
        phase = .connecting
        isPresented = true
        ringbackWanted = false
        stopRingback()
        do {
            let fresh = try await api.joinCall(inc.call.callId)
            call = fresh
            try await connect(fresh, video: fresh.kind == .video)
        } catch {
            self.error = CallManager.describe(error)
            await teardown(leave: false)
        }
    }

    func decline() {
        ringTimeout?.cancel()
        endIncoming(reason: .declinedElsewhere)
    }

    private func endIncoming(reason: CXCallEndedReason) {
        if let uuid = cxUUID { provider.reportCall(with: uuid, endedAt: Date(), reason: reason) }
        cxUUID = nil
        incoming = nil
        if phase == .ringing { phase = .idle }
    }

    func hangUp() async {
        await teardown(leave: true)
    }

    /// `call_ended` from the relay (push or alert): stop everything for that call.
    func remoteEnded(callId: String, chatId: String, durationS: Int, writerDevice: String?) {
        activeByChat[chatId] = nil
        if writerDevice == deviceId(), let c = call ?? incoming?.call ?? activeByChat[chatId], c.callId == callId || call?.callId == callId {
            writeSystemLine?(chatId, c.projectId, callEndedLine(durationS: durationS), callId)
        }
        if incoming?.call.callId == callId { endIncoming(reason: .remoteEnded) }
        if call?.callId == callId { Task { await teardown(leave: false, reason: .remoteEnded) } }
    }

    /// Someone's presence or GET /chats/{id}/call says a call is on in a chat.
    func noteActive(_ c: Call?, chatId: String) {
        if let c, !c.ended { activeByChat[chatId] = c } else { activeByChat[chatId] = nil }
    }

    #if DEBUG
    /// Dev: connect to a room with a token minted outside the relay.
    func debugJoin(url: String, token: String, kind: CallKind, chatId: String, projectId: String) async {
        guard !isActive else { isPresented = true; return }
        _ = await CallManager.micGranted()
        if kind == .video { _ = await CallManager.cameraGranted() }
        let c = Call(callId: "dev-\(chatId)", chatId: chatId, projectId: projectId, kind: kind, room: "call_dev01", livekitUrl: url,
                     token: token, tokenExpiresAt: nil, startedBy: me(), startedAt: nowMs(), endedAt: nil, reason: nil, participants: [])
        call = c
        phase = .connecting
        isPresented = true
        reportOutgoing(title: "Dev call", video: kind == .video)
        do { try await connect(c, video: kind == .video) } catch { self.error = CallManager.describe(error); await teardown(leave: false) }
    }
    #endif

    // MARK: Media

    private func connect(_ c: Call, video: Bool) async throws {
        guard let token = c.token, let url = URL(string: c.livekitUrl) else {
            throw RelayError(status: 0, message: "The call has no join token")
        }
        let roomOptions = RoomOptions(
            defaultCameraCaptureOptions: CameraCaptureOptions(position: frontCamera ? .front : .back, dimensions: .h720_169, fps: 30),
            defaultAudioCaptureOptions: AudioCaptureOptions(),
            adaptiveStream: true, dynacast: true
        )
        try await room.connect(url: url.absoluteString, token: token, connectOptions: ConnectOptions(autoSubscribe: true), roomOptions: roomOptions)
        phase = .connected
        connectedAt = Date()
        if let uuid = cxUUID { provider.reportOutgoingCall(with: uuid, connectedAt: Date()) }
        try? await room.localParticipant.setMicrophone(enabled: !muted)
        if video, AVCaptureDevice.authorizationStatus(for: .video) == .authorized {
            do { try await room.localParticipant.setCamera(enabled: true); cameraOn = true } catch { cameraOn = false }
        }
        speakerOn = video
        AudioManager.shared.isSpeakerOutputPreferred = speakerOn
        refreshTiles()
    }

    private func teardown(leave: Bool, reason: CXCallEndedReason = .remoteEnded) async {
        ringbackWanted = false
        stopRingback()
        let ended = call
        let wasConnected = connectedAt
        await room.disconnect()
        if let uuid = cxUUID {
            if leave {
                let action = CXEndCallAction(call: uuid)
                try? await callController.request(CXTransaction(action: action))
            } else {
                provider.reportCall(with: uuid, endedAt: Date(), reason: reason)
            }
        }
        cxUUID = nil
        if leave, let ended, let api, !isFixture {
            do {
                let r = try await api.leaveCall(ended.callId)
                if r.ended {
                    activeByChat[ended.chatId] = nil
                    writeSystemLine?(ended.chatId, ended.projectId, callEndedLine(durationS: r.durationS), ended.callId)
                }
            } catch {
                log.error("leave: \(error.localizedDescription, privacy: .public)")
            }
        }
        if leave, isFixture, let ended {
            activeByChat[ended.chatId] = nil
            writeSystemLine?(ended.chatId, ended.projectId, callEndedLine(durationS: Int(wasConnected.map { Date().timeIntervalSince($0) } ?? 0)), ended.callId)
        }
        call = nil
        tiles = []
        connectedAt = nil
        cameraOn = false
        muted = false
        phase = .idle
        isPresented = false
    }

    func toggleMute() async {
        muted.toggle()
        try? await room.localParticipant.setMicrophone(enabled: !muted)
        if let uuid = cxUUID { try? await callController.request(CXTransaction(action: CXSetMutedCallAction(call: uuid, muted: muted))) }
    }

    func toggleCamera() async {
        if !cameraOn, !(await CallManager.cameraGranted()) { error = "Camera access is off in Settings."; return }
        do {
            try await room.localParticipant.setCamera(enabled: !cameraOn)
            cameraOn.toggle()
        } catch {
            self.error = CallManager.describe(error)
        }
        objectWillChange.send()
    }

    func toggleSpeaker() {
        speakerOn.toggle()
        AudioManager.shared.isSpeakerOutputPreferred = speakerOn
    }

    func flipCamera() async {
        guard let capturer = (localVideoTrack as? LocalVideoTrack)?.capturer as? CameraCapturer else { return }
        frontCamera.toggle()
        _ = try? await capturer.switchCameraPosition()
        objectWillChange.send()
    }

    private func refreshTiles() {
        tiles = room.remoteParticipants.values
            .sorted { ($0.joinedAt ?? .distantPast) < ($1.joinedAt ?? .distantPast) }
            .map { p in
                let identity = p.identity?.stringValue ?? UUID().uuidString
                let person = CallPerson.parse(metadata: p.metadata) ?? CallPerson.placeholder(identity: identity, name: p.name)
                let video = p.videoTracks.first { $0.source == .camera }?.track as? VideoTrack
                return CallTile(id: identity, person: person, isSpeaking: p.isSpeaking,
                                micOn: p.isMicrophoneEnabled(), cameraOn: p.isCameraEnabled(), videoTrack: video)
            }
        syncRingback()
    }

    // MARK: Outgoing ringback

    /// Play while we wait for someone else ("Calling…" / "Ringing…"); stop when they join.
    private func syncRingback() {
        let waiting = ringbackWanted && tiles.isEmpty && (phase == .connecting || phase == .connected || phase == .reconnecting)
        if waiting { startRingback() } else { stopRingback() }
    }

    private func startRingback() {
        if let ringbackPlayer, ringbackPlayer.isPlaying { return }
        do {
            let player = try AVAudioPlayer(data: CallManager.ringbackWAV)
            player.numberOfLoops = -1
            player.volume = 0.55
            player.prepareToPlay()
            // CallKit / LiveKit own the session — do not retarget the category.
            _ = player.play()
            ringbackPlayer = player
        } catch {
            log.error("ringback: \(error.localizedDescription, privacy: .public)")
            ringbackPlayer = nil
        }
    }

    private func stopRingback() {
        ringbackPlayer?.stop()
        ringbackPlayer = nil
    }

    /// Dual-tone ringback (≈ North American): 440 + 480 Hz, 2 s on / 4 s off.
    private static let ringbackWAV: Data = {
        let sampleRate = 44_100
        let onSamples = sampleRate * 2
        let cycleSamples = sampleRate * 6
        var pcm = Data(count: cycleSamples * 2)
        pcm.withUnsafeMutableBytes { raw in
            guard let base = raw.bindMemory(to: Int16.self).baseAddress else { return }
            let amp = 0.28 * Double(Int16.max)
            for i in 0..<onSamples {
                let t = Double(i) / Double(sampleRate)
                let sample = (sin(2 * Double.pi * 440 * t) + sin(2 * Double.pi * 480 * t)) * 0.5
                base[i] = Int16(amp * sample)
            }
            // Remaining samples stay 0 (silence for the 4 s off gap).
        }
        var wav = Data()
        wav.reserveCapacity(44 + pcm.count)
        func u32(_ v: UInt32) { withUnsafeBytes(of: v.littleEndian) { wav.append(contentsOf: $0) } }
        func u16(_ v: UInt16) { withUnsafeBytes(of: v.littleEndian) { wav.append(contentsOf: $0) } }
        wav.append(contentsOf: [0x52, 0x49, 0x46, 0x46]) // RIFF
        u32(UInt32(36 + pcm.count))
        wav.append(contentsOf: [0x57, 0x41, 0x56, 0x45, 0x66, 0x6D, 0x74, 0x20]) // WAVEfmt
        u32(16) // PCM fmt chunk size
        u16(1) // PCM
        u16(1) // mono
        u32(UInt32(sampleRate))
        u32(UInt32(sampleRate * 2)) // byte rate
        u16(2) // block align
        u16(16) // bits
        wav.append(contentsOf: [0x64, 0x61, 0x74, 0x61]) // data
        u32(UInt32(pcm.count))
        wav.append(pcm)
        return wav
    }()

    // MARK: CallKit mirror

    private func reportOutgoing(title: String, video: Bool) {
        let uuid = UUID()
        cxUUID = uuid
        let handle = CXHandle(type: .generic, value: title)
        let action = CXStartCallAction(call: uuid, handle: handle)
        action.isVideo = video
        callController.request(CXTransaction(action: action)) { err in
            if let err { log.error("CXStartCallAction: \(err.localizedDescription, privacy: .public)") }
        }
        let update = CXCallUpdate()
        update.remoteHandle = handle
        update.localizedCallerName = title
        update.hasVideo = video
        provider.reportCall(with: uuid, updated: update)
    }

    // MARK: PushKit (VoIP)

    func prepareVoip() {
        guard voipRegistry == nil else { return }
        let registry = PKPushRegistry(queue: .main)
        registry.delegate = self
        registry.desiredPushTypes = [.voIP]
        voipRegistry = registry
    }

    func sendVoipTokenIfNeeded(force: Bool = false) async {
        guard let api, let token = voipToken, !isFixture else { return }
        if !force, token == registeredVoipToken { return }
        registeredVoipToken = token
        do {
            voipPushEnabled = try await api.registerVoip(deviceId: deviceId(), token: token, environment: PushManager.environment,
                                                         bundleId: Bundle.main.bundleIdentifier ?? "com.unarbos.arbos.ios")
        } catch {
            registeredVoipToken = nil
            log.error("voip register: \(error.localizedDescription, privacy: .public)")
        }
    }

    func forgetVoipRegistration() { registeredVoipToken = nil }

    static func describe(_ error: Error) -> String {
        if let e = error as? RelayError {
            switch (e.status, e.type ?? "") {
            case (503, _), (_, "calls_off"): return "Calls are not switched on for this relay yet."
            case (410, _), (_, "call_ended"): return "That call has ended."
            case (409, _), (_, "call_full"): return "The call is full."
            case (404, _), (405, _): return "Calls are not on this relay yet."
            case (403, _): return "You are not a member of this chat."
            default: break
            }
        }
        return error.localizedDescription
    }

    /// An ARBOS_CALL alert (tap or foreground): `event` started → ring, ended → stop.
    func handleCallAlert(_ info: [AnyHashable: Any]) {
        let event = (info["event"] as? String) ?? ""
        if event == "ended" {
            if let cid = (info["call"] as? [String: Any])?["call_id"] as? String ?? info["call_id"] as? String,
               let chat = info["chat_id"] as? String {
                remoteEnded(callId: cid, chatId: chat, durationS: 0, writerDevice: nil)
            }
        } else if let c = Call.parse(info["call"]) {
            ring(c, from: CallPerson.parse(info["sender"]) ?? c.startedBy, viaVoip: false)
        }
    }
}

// MARK: - LiveKit

extension CallManager: RoomDelegate {
    nonisolated func room(_ room: LiveKit.Room, didUpdateConnectionState connectionState: ConnectionState, from oldConnectionState: ConnectionState) {
        Task { @MainActor in
            switch connectionState {
            case .reconnecting: if self.phase == .connected { self.phase = .reconnecting }
            case .connected: if self.phase == .reconnecting { self.phase = .connected }
            case .disconnected:
                // Only a drop from a live session counts; the SDK also reports
                // .disconnected while a connect is still in flight.
                if (oldConnectionState == .connected || oldConnectionState == .reconnecting),
                   self.phase == .connected || self.phase == .reconnecting {
                    self.error = self.error ?? "Connection lost."
                    await self.teardown(leave: true, reason: .failed)
                }
            default: break
            }
            self.refreshTiles()
        }
    }

    nonisolated func room(_ room: LiveKit.Room, participantDidConnect participant: RemoteParticipant) {
        Task { @MainActor in self.refreshTiles() }
    }

    nonisolated func room(_ room: LiveKit.Room, participantDidDisconnect participant: RemoteParticipant) {
        Task { @MainActor in self.refreshTiles() }
    }

    nonisolated func room(_ room: LiveKit.Room, participant: RemoteParticipant, didSubscribeTrack publication: RemoteTrackPublication) {
        Task { @MainActor in self.refreshTiles() }
    }

    nonisolated func room(_ room: LiveKit.Room, participant: RemoteParticipant, didUnsubscribeTrack publication: RemoteTrackPublication) {
        Task { @MainActor in self.refreshTiles() }
    }

    nonisolated func room(_ room: LiveKit.Room, participant: Participant, didUpdateIsSpeaking isSpeaking: Bool) {
        Task { @MainActor in self.refreshTiles() }
    }

    nonisolated func room(_ room: LiveKit.Room, participant: Participant, didUpdateMetadata metadata: String?) {
        Task { @MainActor in self.refreshTiles() }
    }

    nonisolated func room(_ room: LiveKit.Room, participant: Participant, trackPublication: TrackPublication, didUpdateIsMuted isMuted: Bool) {
        Task { @MainActor in self.refreshTiles() }
    }
}

// MARK: - CallKit

extension CallManager: CXProviderDelegate {
    nonisolated func providerDidReset(_ provider: CXProvider) {
        Task { @MainActor in await self.teardown(leave: true, reason: .failed) }
    }

    nonisolated func provider(_ provider: CXProvider, perform action: CXStartCallAction) {
        action.fulfill()
    }

    nonisolated func provider(_ provider: CXProvider, perform action: CXAnswerCallAction) {
        Task { @MainActor in
            await self.accept()
            action.fulfill()
        }
    }

    nonisolated func provider(_ provider: CXProvider, perform action: CXEndCallAction) {
        Task { @MainActor in
            if self.incoming != nil {
                self.ringTimeout?.cancel()
                self.incoming = nil
                self.phase = .idle
                self.cxUUID = nil
            } else if self.isActive {
                // Ended from the system UI: leave the relay call too.
                self.cxUUID = nil
                await self.teardown(leave: true)
            }
            action.fulfill()
        }
    }

    nonisolated func provider(_ provider: CXProvider, perform action: CXSetMutedCallAction) {
        Task { @MainActor in
            if self.muted != action.isMuted {
                self.muted = action.isMuted
                try? await self.room.localParticipant.setMicrophone(enabled: !self.muted)
            }
            action.fulfill()
        }
    }

    nonisolated func provider(_ provider: CXProvider, didActivate audioSession: AVAudioSession) {
        log.notice("audio session activated by CallKit")
    }

    nonisolated func provider(_ provider: CXProvider, didDeactivate audioSession: AVAudioSession) {
        log.notice("audio session deactivated by CallKit")
    }
}

// MARK: - PushKit

extension CallManager: PKPushRegistryDelegate {
    nonisolated func pushRegistry(_ registry: PKPushRegistry, didUpdate pushCredentials: PKPushCredentials, for type: PKPushType) {
        let hex = pushCredentials.token.map { String(format: "%02x", $0) }.joined()
        Task { @MainActor in
            self.voipToken = hex
            await self.sendVoipTokenIfNeeded()
        }
    }

    nonisolated func pushRegistry(_ registry: PKPushRegistry, didInvalidatePushTokenFor type: PKPushType) {
        Task { @MainActor in self.voipToken = nil; self.registeredVoipToken = nil }
    }

    /// Payload `{call_id, chat_id, project_id, kind, room, livekit_url, caller: sender{}, started_at}`.
    /// CallKit must be told before this returns; the join token comes from POST /calls/{id}/join on accept.
    nonisolated func pushRegistry(_ registry: PKPushRegistry, didReceiveIncomingPushWith payload: PKPushPayload, for type: PKPushType, completion: @escaping () -> Void) {
        let dict = payload.dictionaryPayload
        var d: [String: Any] = [:]
        for (k, v) in dict { if let key = k as? String { d[key] = v } }
        let caller = CallPerson.parse(d["caller"] ?? d["sender"])
        var callDict = d
        callDict["started_by"] = d["caller"] ?? d["sender"]
        let parsed = Call.parse(callDict)
        Task { @MainActor in
            if let parsed {
                self.ring(parsed, from: caller, viaVoip: true)
            } else {
                // Report something anyway: iOS terminates apps that take a VoIP push and report no call.
                let uuid = UUID()
                let update = CXCallUpdate()
                update.remoteHandle = CXHandle(type: .generic, value: "Arbos")
                self.provider.reportNewIncomingCall(with: uuid, update: update) { _ in
                    self.provider.reportCall(with: uuid, endedAt: Date(), reason: .failed)
                }
            }
            completion()
        }
    }
}
