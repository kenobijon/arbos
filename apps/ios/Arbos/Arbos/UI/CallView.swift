// Call screens, Grok Bot style: title at the top, a big rounded card with
// the other person (their video when the camera is on, else their avatar),
// name + status, glass controls and a red Hang up. Groups get a 2-column
// grid. `IncomingCallView` is the ringing screen while the app is open;
// `CallBar` is the "On a call · Join" strip in a chat.

import SwiftUI
import LiveKit

/// A participant's picture: coloured tree for agents, GitHub picture (or initial) for people.
struct CallPersonAvatar: View {
    let person: CallPerson
    var size: CGFloat
    @EnvironmentObject private var model: AppModel

    var body: some View {
        if person.isAgent, let slug = person.avatar, TreeAvatars.shared.has(slug) {
            TreeCharacterView(slug: slug, size: size, color: person.avatarColor.flatMap { TreeAvatars.shared.color(named: $0) }, still: true)
        } else {
            PersonAvatar(url: person.avatarUrl, handle: person.handle, size: size)
        }
    }
}

struct CallScreen: View {
    @ObservedObject private var calls = CallManager.shared
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    /// Who we expected to talk to (shown before they join). Groups pass every other member + agents.
    var expected: [CallPerson] = []

    private var isVideo: Bool { calls.call?.kind == .video }
    private var title: String { isVideo ? "Video call" : "Voice call" }
    private var primary: CallTile? { calls.tiles.first }
    private var primaryPerson: CallPerson? {
        primary?.person ?? expected.first ?? calls.call?.participants.first { $0.id != model.user?.id } ?? calls.call?.startedBy
    }
    /// Group chat (people group or shared project with several expected people).
    private var isGroupCall: Bool {
        if expected.count >= 2 { return true }
        guard let id = calls.call?.chatId, let chat = model.peopleChat(id) else { return false }
        return chat.isGroup
    }
    private var groupTitle: String? {
        guard let id = calls.call?.chatId, let chat = model.peopleChat(id), chat.isGroup else { return nil }
        return chat.title.isEmpty ? "Group" : chat.title
    }
    /// Expected people who have not joined LiveKit yet (humans still ringing, agents until Phase 2).
    private var waitingExpected: [CallPerson] {
        expected.filter { e in !calls.tiles.contains { $0.person.id == e.id } }
    }
    private var useGroupLayout: Bool { calls.tiles.count + waitingExpected.count >= 2 || (isGroupCall && !waitingExpected.isEmpty && calls.tiles.count <= 1) }

    var body: some View {
        ZStack {
            theme.bg.ignoresSafeArea()
            VStack(spacing: 0) {
                ZStack {
                    TimelineView(.periodic(from: .now, by: 1)) { _ in
                        Text(headline)
                            .font(.system(size: Theme.small, weight: .semibold))
                            .foregroundStyle(theme.muted)
                    }
                    HStack {
                        Button { collapse() } label: {
                            GlassCircle(size: 40) {
                                Image(systemName: "chevron.down").font(.system(size: 15, weight: .semibold)).foregroundStyle(theme.text)
                            }
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Back to chat")
                        .accessibilityHint("Keeps the call going in a small bar over the chat")
                        Spacer()
                    }
                    .padding(.horizontal, 16)
                }
                .padding(.top, 8)
                Spacer(minLength: 12)
                if useGroupLayout {
                    grid
                } else {
                    oneToOne
                }
                Spacer(minLength: 12)
                controls.padding(.bottom, 40)
            }
            if let err = calls.error {
                VStack {
                    Spacer()
                    Text(err).font(.system(size: Theme.tiny)).foregroundStyle(theme.danger)
                        .multilineTextAlignment(.center).padding(.horizontal, 32).padding(.bottom, 150)
                }
            }
        }
        .statusBarHidden(false)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(title)
        // Pull down anywhere on the screen: same as the chevron.
        .simultaneousGesture(
            DragGesture(minimumDistance: 40)
                .onEnded { v in
                    if v.translation.height > 120, v.translation.height > abs(v.translation.width) * 1.5 { collapse() }
                }
        )
    }

    /// Collapse into the in-call pill and make sure the call's chat is on screen.
    private func collapse() {
        guard calls.isActive, let c = calls.call else { return }
        if let pc = model.peopleChat(c.chatId) {
            if !model.isShowing(chatId: pc.id) { model.open(peopleChat: pc) }
        } else if let chat = model.chat(c.chatId) {
            if !model.isShowing(chatId: chat.id) { model.open(chat: chat) }
        }
        calls.minimize()
    }

    private var headline: String {
        switch calls.phase {
        case .connected: return "\(title) · \(CallScreen.clock(calls.duration))"
        case .reconnecting: return "\(title) · Reconnecting…"
        default: return title
        }
    }

    private var status: String {
        switch calls.phase {
        case .connecting: return "Calling…"
        case .reconnecting: return "Reconnecting…"
        case .connected:
            if calls.tiles.isEmpty {
                if let line = ringingStatus { return line }
                return "Ringing…"
            }
            return primary?.micOn == false ? "Muted" : "Connected"
        case .ringing: return "Incoming…"
        case .idle: return calls.error ?? "Call ended"
        }
    }

    /// Status while nobody has joined yet. Several callees join with " · " (Mac `reachable_line`).
    private var ringingStatus: String? {
        let reach = calls.call?.reachable ?? []
        if !reach.isEmpty {
            let lines: [String] = reach.compactMap { r in
                if r.anyLive { return nil }
                if r.canRingClosed { return "Ringing \(r.displayName)'s iPhone" }
                return "\(r.displayName)'s Arbos isn't open — they'll see a missed call"
            }
            guard !lines.isEmpty else { return nil }
            if lines.count <= 2 { return lines.joined(separator: " · ") }
            return "\(lines[0]) · \(lines[1]) · +\(lines.count - 2) more"
        }
        if calls.calleeUnreachable {
            if isGroupCall { return "Nobody's Arbos is open — they'll see a missed call" }
            let name = calls.calleeName?(calls.call?.chatId ?? "") ?? "Their"
            return "\(name)'s Arbos isn't open — they'll see a missed call"
        }
        return nil
    }

    // MARK: 1:1

    private var oneToOne: some View {
        VStack(spacing: 0) {
            ZStack(alignment: .topTrailing) {
                RoundedRectangle(cornerRadius: 28, style: .continuous)
                    .fill(theme.card)
                    .overlay(RoundedRectangle(cornerRadius: 28, style: .continuous).strokeBorder(theme.fieldBorder, lineWidth: 1))
                    .overlay {
                        if let track = primary?.videoTrack, primary?.cameraOn == true {
                            SwiftUIVideoView(track, layoutMode: .fill)
                                .clipShape(RoundedRectangle(cornerRadius: 28, style: .continuous))
                        } else {
                            VStack(spacing: 16) {
                                if let p = primaryPerson {
                                    CallPersonAvatar(person: p, size: 108)
                                        .overlay(speakingRing(primary?.isSpeaking == true, size: 108))
                                } else {
                                    GlassCircle(size: 108) { Image(systemName: "person.fill").font(.system(size: 40)).foregroundStyle(theme.muted) }
                                }
                                Image(systemName: isVideo ? (primary?.cameraOn == true ? "video.fill" : "video.slash.fill") : "waveform")
                                    .font(.system(size: 22, weight: .semibold))
                                    .foregroundStyle(theme.muted)
                            }
                        }
                    }
                    .frame(width: 270, height: 352)
                if calls.cameraOn, let local = calls.localVideoTrack {
                    SwiftUIVideoView(local, layoutMode: .fill, mirrorMode: .mirror)
                        .frame(width: 84, height: 112)
                        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
                        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(theme.bg, lineWidth: 2))
                        .padding(10)
                }
            }
            Text(groupTitle ?? primaryPerson?.displayName ?? calls.call?.startedBy?.displayName ?? "Call")
                .font(.system(size: 28, weight: .semibold))
                .foregroundStyle(theme.text)
                .padding(.top, 28)
            Text(status)
                .font(.system(size: Theme.body))
                .foregroundStyle(theme.muted)
                .padding(.top, 6)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)
        }
    }

    // MARK: Group

    private var grid: some View {
        VStack(spacing: 14) {
            LazyVGrid(columns: [GridItem(.flexible(), spacing: 10), GridItem(.flexible(), spacing: 10)], spacing: 10) {
                ForEach(calls.tiles) { tile in
                    personTile(tile.person, videoTrack: tile.videoTrack, cameraOn: tile.cameraOn, micOn: tile.micOn, speaking: tile.isSpeaking)
                }
                ForEach(waitingExpected) { person in
                    personTile(person, videoTrack: nil, cameraOn: false, micOn: true, speaking: false)
                }
                if calls.cameraOn, let local = calls.localVideoTrack {
                    SwiftUIVideoView(local, layoutMode: .fill, mirrorMode: .mirror)
                        .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
                        .aspectRatio(3 / 4, contentMode: .fit)
                }
            }
            .padding(.horizontal, 20)
            if let groupTitle {
                Text(groupTitle).font(.system(size: 22, weight: .semibold)).foregroundStyle(theme.text)
            }
            Text(status)
                .font(.system(size: Theme.body))
                .foregroundStyle(theme.muted)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 24)
            if !calls.tiles.isEmpty {
                Text("\(calls.tiles.count + 1) in the call").font(.system(size: Theme.small)).foregroundStyle(theme.muted)
            }
        }
    }

    private func personTile(_ person: CallPerson, videoTrack: VideoTrack?, cameraOn: Bool, micOn: Bool, speaking: Bool) -> some View {
        ZStack(alignment: .bottomLeading) {
            RoundedRectangle(cornerRadius: 20, style: .continuous).fill(theme.card)
                .overlay(RoundedRectangle(cornerRadius: 20, style: .continuous)
                    .strokeBorder(speaking ? theme.online : theme.fieldBorder, lineWidth: speaking ? 2 : 1))
            if let track = videoTrack, cameraOn {
                SwiftUIVideoView(track, layoutMode: .fill).clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
            } else {
                CallPersonAvatar(person: person, size: 64).frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            HStack(spacing: 5) {
                if !micOn { Image(systemName: "mic.slash.fill").font(.system(size: 10, weight: .bold)) }
                Text(person.displayName).font(.system(size: 12, weight: .semibold)).lineLimit(1)
            }
            .foregroundStyle(.white)
            .padding(.horizontal, 8).padding(.vertical, 4)
            .background(Capsule().fill(.black.opacity(0.45)))
            .padding(8)
        }
        .aspectRatio(3 / 4, contentMode: .fit)
    }

    // MARK: Controls

    private var controls: some View {
        HStack(spacing: 24) {
            control(icon: calls.muted ? "mic.slash.fill" : "mic.fill", label: calls.muted ? "Unmute" : "Mute", on: calls.muted) {
                Task { await calls.toggleMute() }
            }
            if isVideo {
                control(icon: calls.cameraOn ? "video.fill" : "video.slash.fill", label: calls.cameraOn ? "Camera off" : "Camera on", on: !calls.cameraOn) {
                    Task { await calls.toggleCamera() }
                }
                control(icon: "arrow.triangle.2.circlepath.camera", label: "Flip", on: false) { Task { await calls.flipCamera() } }
                    .disabled(!calls.cameraOn)
                    .opacity(calls.cameraOn ? 1 : 0.4)
            } else {
                control(icon: calls.speakerOn ? "speaker.wave.3.fill" : "speaker.fill", label: "Speaker", on: calls.speakerOn) { calls.toggleSpeaker() }
            }
            Button { Task { await calls.hangUp() } } label: {
                VStack(spacing: 8) {
                    Circle().fill(theme.danger).frame(width: 64, height: 64)
                        .overlay(Image(systemName: "phone.down.fill").font(.system(size: 24, weight: .semibold)).foregroundStyle(.white))
                    Text("Hang up").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                }
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Hang up")
        }
    }

    private func control(icon: String, label: String, on: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            VStack(spacing: 8) {
                ZStack {
                    if on { Circle().fill(theme.text).frame(width: 64, height: 64) } else { GlassCircle(size: 64) { EmptyView() } }
                    Image(systemName: icon).font(.system(size: 22, weight: .medium)).foregroundStyle(on ? theme.bg : theme.text)
                }
                Text(label).font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
            }
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
    }

    @ViewBuilder
    private func speakingRing(_ speaking: Bool, size: CGFloat) -> some View {
        if speaking {
            Circle().strokeBorder(theme.online, lineWidth: 3).frame(width: size + 10, height: size + 10)
        }
    }

    static func clock(_ t: TimeInterval) -> String {
        let s = Int(t)
        return s >= 3600 ? String(format: "%d:%02d:%02d", s / 3600, (s / 60) % 60, s % 60) : String(format: "%02d:%02d", s / 60, s % 60)
    }
}

/// Ringing while the app is open (CallKit shows the system screen otherwise).
struct IncomingCallView: View {
    @ObservedObject private var calls = CallManager.shared
    @Environment(\.theme) private var theme

    var body: some View {
        ZStack {
            theme.bg.ignoresSafeArea()
            if let inc = calls.incoming {
                VStack(spacing: 0) {
                    Text(inc.call.kind == .video ? "Incoming video call" : "Incoming voice call")
                        .font(.system(size: Theme.small, weight: .semibold)).foregroundStyle(theme.muted)
                        .padding(.top, 14)
                    Spacer()
                    if let p = inc.from ?? inc.call.startedBy {
                        CallPersonAvatar(person: p, size: 128)
                            .overlay(Circle().strokeBorder(theme.fieldBorder, lineWidth: 1).frame(width: 150, height: 150))
                        Text(p.displayName).font(.system(size: 28, weight: .semibold)).foregroundStyle(theme.text).padding(.top, 28)
                        Text("@\(p.handle)").font(.system(size: Theme.body)).foregroundStyle(theme.muted).padding(.top, 4)
                    }
                    Spacer()
                    HStack(spacing: 72) {
                        VStack(spacing: 8) {
                            Button { calls.decline() } label: {
                                Circle().fill(theme.danger).frame(width: 72, height: 72)
                                    .overlay(Image(systemName: "phone.down.fill").font(.system(size: 26, weight: .semibold)).foregroundStyle(.white))
                            }
                            .buttonStyle(.plain)
                            Text("Decline").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                        }
                        VStack(spacing: 8) {
                            Button { calls.acceptFromUI() } label: {
                                Circle().fill(theme.online).frame(width: 72, height: 72)
                                    .overlay(Image(systemName: inc.call.kind == .video ? "video.fill" : "phone.fill").font(.system(size: 26, weight: .semibold)).foregroundStyle(.white))
                            }
                            .buttonStyle(.plain)
                            Text("Accept").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                        }
                    }
                    .padding(.bottom, 56)
                }
            }
        }
    }
}

/// "On a call · Join" under a chat header while a call is active there.
struct CallBar: View {
    let call: Call
    let onJoin: () -> Void
    @Environment(\.theme) private var theme
    @ObservedObject private var calls = CallManager.shared

    var body: some View {
        Button(action: onJoin) {
            HStack(spacing: 10) {
                Image(systemName: call.kind == .video ? "video.fill" : "phone.fill").font(.system(size: 13, weight: .semibold)).foregroundStyle(theme.online)
                Text(calls.call?.callId == call.callId ? "On this call" : "On a call")
                    .font(.system(size: Theme.small, weight: .semibold)).foregroundStyle(theme.text)
                if !call.participants.isEmpty {
                    Text("· \(call.participants.count)").font(.system(size: Theme.small)).foregroundStyle(theme.muted)
                }
                Spacer()
                Text(calls.call?.callId == call.callId ? "Return" : "Join")
                    .font(.system(size: Theme.tiny, weight: .semibold)).foregroundStyle(theme.onPrimary)
                    .padding(.horizontal, 12).frame(height: 28)
                    .background(Capsule().fill(theme.primary))
            }
            .padding(.horizontal, 14).frame(height: 44)
            .background(Capsule().fill(theme.card))
            .overlay(Capsule().strokeBorder(theme.fieldBorder, lineWidth: 1))
        }
        .buttonStyle(.plain)
    }
}
