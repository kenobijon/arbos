// One conversation, Grok Bot layout: floating glass header (back circle,
// avatar + name pill, "…" circle), messages scrolling underneath with a top
// fade, "<Project> is working" while a turn runs, and the composer ("+",
// "Ask <Project>" field with mic, white call button) floating above the
// keyboard.

import PhotosUI
import SwiftUI
import UIKit
import UniformTypeIdentifiers

struct ChatView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    let chatId: Id
    let projectId: Id

    @State private var draft = ""
    @State private var rows: [Row] = []
    @State private var showModelPicker = false
    @State private var showSettings = false
    @State private var sentOnce = false
    @State private var transcribing = false
    @State private var pending: [AppModel.PendingAttachment] = []
    @State private var photoItem: PhotosPickerItem?
    @State private var showPhotos = false
    @State private var showFiles = false
    @State private var showAttach = false
    @State private var scroller: ScrollViewProxy?
    /// Scrolled more than ~120 pt above the end: show Grok's round chevron.
    @State private var awayFromBottom = false
    @State private var demoRan = false
    @State private var demoQueue: [String] = []
    @StateObject private var recorder = MemoRecorder()
    /// Finger is down on the mic (hold-to-talk or click-to-toggle).
    @State private var memoHoldActive = false
    /// Second tap while recording: stop on press and ignore the matching release.
    @State private var memoIgnoreRelease = false
    /// Hold ended before mic permission returned; finish once recording starts.
    @State private var memoFinishWhenPermission = false
    /// When the current mic press began (for hold-vs-tap).
    @State private var memoPressStartedAt: Date?
    @FocusState private var composerFocused: Bool

    @State private var showMembers = false
    @State private var showShare = false
    @State private var showProfile = false
    @State private var showSearch = false
    @State private var searchText = ""
    @FocusState private var searchFocused: Bool
    @ObservedObject private var calls = CallManager.shared

    /// Rows after the in-chat search filter (profile "search").
    private var visibleRows: [Row] {
        let q = searchText.trimmingCharacters(in: .whitespaces)
        guard showSearch, !q.isEmpty else { return rows }
        return rows.filter { m_text($0).localizedCaseInsensitiveContains(q) }
    }

    /// Who the CallKit card names: the other person in a DM, else the group/project.
    private var callTitle: String {
        if let people, people.isGroup {
            return people.title.isEmpty ? "Group" : people.title
        }
        if let c = counterpart { return c.displayName }
        return project?.name ?? people?.title ?? name
    }

    /// Header tap: DM → the other person; group / shared project → the group page; own agent → its page.
    private func openHeaderProfile() {
        if let people {
            if people.kind == "dm", people.humans.count <= 2, let other = counterpart {
                model.showProfile(.user(ProfilePerson(other)))
            } else {
                model.showProfile(.group(chatId: chatId, projectId: nil))
            }
        } else if project?.shared == true {
            model.showProfile(.group(chatId: chatId, projectId: projectId))
        } else {
            model.showProfile(.agent(projectId: projectId))
        }
    }

    /// Profile "search" tile: raise the in-chat search bar once for this chat.
    private func openPendingSearchIfNeeded() {
        guard model.pendingSearchChat == chatId else { return }
        model.pendingSearchChat = nil
        showSearch = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { searchFocused = true }
    }

    /// Tokenised blob URL for a custom group picture, if the chat has one.
    private func groupImageURL(_ people: PeopleChat) -> String? {
        guard let key = people.imageKey, !key.isEmpty else { return nil }
        return model.blobURL(projectId: "\(peopleSlot):\(people.id)", key: key)?.absoluteString
    }

    private func groupHeaderMembers(_ people: PeopleChat) -> [ChatMember] {
        let others = people.humans.filter { $0.id != model.user?.id }
        return others.isEmpty ? people.humans : others
    }

    private func groupSubtitle(_ people: PeopleChat) -> String {
        let n = people.humans.count
        var s = "\(n) member\(n == 1 ? "" : "s")"
        if !people.agents.isEmpty {
            s += " · \(people.agents.count) agent\(people.agents.count == 1 ? "" : "s")"
        }
        return s
    }

    /// A call in this chat we are not in (the one we are in shows as the in-call pill instead).
    private var hasCallBar: Bool {
        guard let active = calls.activeByChat[chatId], !active.ended else { return false }
        return calls.call?.callId != active.callId
    }
    /// Our own call, collapsed into the header bar of this chat (voice; video floats as PiP).
    private var hasInCallBar: Bool {
        calls.isActive && !calls.isPresented && calls.call?.kind == .voice && calls.call?.chatId == chatId
    }
    private var headerExtra: CGFloat { (hasCallBar ? 52 : 0) + (hasInCallBar ? 56 : 0) + (showSearch ? 46 : 0) }

    private func startCall(_ kind: CallKind) {
        Task { await calls.start(chatId: chatId, projectId: isPeople ? nil : projectId, kind: kind, title: callTitle) }
    }
    @State private var replyTo: Message?
    @State private var mentionQuery: String?
    /// Chip-picked addressee in a people chat: `agent:…`, `user:…`, or `"everyone"`.
    /// Nil means no chip: agent-only DMs may still infer the last agent; human-peer DMs stay
    /// with the person until a chip / `@` / reply picks an agent.
    @State private var talkTo: String?

    private var project: Project? { model.project(projectId) }
    private var chat: Chat? { model.chat(chatId) }
    private var people: PeopleChat? { model.peopleChat(chatId) }
    private var isPeople: Bool { people != nil }
    private var counterpart: ChatMember? { people?.counterpart(me: model.user?.id) }
    private var messages: [Message] { model.messages(of: chatId) }
    private var needsAnswer: Bool { model.chatNeedsAnswer(chatId) }
    /// True while a turn runs, or right after the user sends (before the
    /// agent opens a streaming row) — visual cheat so a simple first reply
    /// does not sit on a dead transcript.
    private var streaming: Bool { model.isStreaming(chatId) || model.isAwaitingReply(chatId) }
    private var name: String {
        if let people { return counterpart.map { "@\($0.handle)" } ?? people.title }
        if let chat, chat.isGroup, !chat.title.isEmpty { return chat.title }
        if let chat, chat.isSystem { return "\(project?.name ?? "Arbos") · Projects" }
        return project?.name ?? "Arbos"
    }
    /// Who is working: the streaming agent's handle in a people chat.
    private var workingName: String {
        if isPeople, let m = messages.last(where: { $0.role == .assistant && $0.isStreaming }), let h = m.senderHandle { return h }
        return name
    }

    /// Who the next people-chat message is for — chip pick, reply-to, or (agent-only DM) the
    /// agent who last spoke. A DM with a human peer (Allan + bob) leaves the addressee unset so
    /// plain lines stay with the person; tap the agent chip or `@` to talk to the agent. Groups
    /// leave the addressee unset so the decision model on the box can choose who replies, or none.
    private var effectiveTalkTo: String? {
        if let talkTo { return talkTo }
        if let r = replyTo, r.role == .assistant, let member = agentMember(for: r) {
            return member.member
        }
        // Agent-only DMs keep the last-agent follow-up; a human peer must not inherit @bob.
        let hasHumanPeer = people?.humans.contains { $0.id != model.user?.id } == true
        if people?.kind != "group", !hasHumanPeer,
           let last = messages.last(where: { $0.role == .assistant }),
           let member = agentMember(for: last) {
            return member.member
        }
        return nil
    }

    /// Composer prompt that names the addressee so it is obvious who will answer.
    private var composerPrompt: String {
        if transcribing { return "Transcribing…" }
        if !isPeople { return needsAnswer ? "Answer \(name)" : "Ask \(name)" }
        if AppModel.addressesEveryone(draft) || talkTo == "everyone" { return "Message everyone" }
        // Bare person name in the draft ("Allan?") overrides a sticky agent chip.
        if let people {
            let named = AppModel.namedPeople(in: draft, members: people.members)
            if let member = people.members.first(where: { named.contains($0.member) }) {
                return "Message \(member.displayName)"
            }
        }
        if let member = people?.members.first(where: { $0.member == effectiveTalkTo }) {
            let slug = member.isAgent ? mentionSlug(member.project?.name ?? member.handle) : member.handle
            return "Message @\(slug)"
        }
        // No chip pick: match the header name ("Message Kenobi"), not "@handle".
        if let people {
            return "Message \(counterpart?.displayName ?? people.title)"
        }
        return "Message \(name)"
    }

    private func agentMember(for message: Message) -> ChatMember? {
        guard let h = message.senderHandle?.lowercased() else { return nil }
        return people?.agents.first {
            mentionSlug($0.project?.name ?? "").lowercased() == h || $0.handle.lowercased() == h
        }
    }

    /// Rebuild transcript rows, including "@agent joined" lines for people-chat agents.
    private func refreshRows() {
        // Agents are always added after the chat exists (never founders), so every agent
        // gets a centred "@handle joined" line at its membership time.
        let joins: [Transcript.Join] = (people?.agents ?? []).compactMap { a in
            guard a.joinedAt > 0 else { return nil }
            let handle = mentionSlug(a.project?.name ?? a.handle)
            guard !handle.isEmpty else { return nil }
            return Transcript.Join(id: a.id, handle: handle, at: a.joinedAt)
        }
        rows = Transcript.rows(messages, projectId: isPeople ? "\(peopleSlot):\(chatId)" : projectId,
                               me: model.myUserId, people: isPeople, joins: joins)
    }

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .top) {
                theme.bg.ignoresSafeArea()
                transcript(width: geo.size.width, height: geo.size.height)
                headerBlur
                VStack(spacing: 8) {
                    header
                    if let people { memberChips(people).padding(.horizontal, 16) }
                    if showSearch {
                        HStack(spacing: 8) {
                            Image(systemName: "magnifyingglass").font(.system(size: 15, weight: .medium)).foregroundStyle(theme.muted)
                            TextField("", text: $searchText, prompt: Text("Search in chat").foregroundStyle(theme.placeholder))
                                .font(.system(size: Theme.body)).foregroundStyle(theme.text)
                                .focused($searchFocused)
                                .autocorrectionDisabled()
                            Button { showSearch = false; searchText = "" } label: {
                                Image(systemName: "xmark.circle.fill").foregroundStyle(theme.muted)
                            }
                            .buttonStyle(.plain)
                        }
                        .padding(.horizontal, 12).frame(height: 38)
                        .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(theme.field))
                        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(theme.fieldBorder, lineWidth: 1))
                        .padding(.horizontal, 16)
                    }
                    if hasInCallBar, let c = calls.call {
                        InCallOverlay(expected: model.expectedCallPeople(c), inline: true)
                            .frame(height: 48)
                    }
                    if hasCallBar, let active = calls.activeByChat[chatId] {
                        CallBar(call: active) {
                            Task { await calls.join(active, title: callTitle) }
                        }
                        .padding(.horizontal, 16)
                    }
                }
            }
        }
        .overlay(alignment: .bottomTrailing) {
            if awayFromBottom {
                scrollToBottomButton
                    .padding(.trailing, composerSideInset)
                    .padding(.bottom, 10)
                    .transition(.opacity.combined(with: .scale(scale: 0.8)))
            }
        }
        .animation(.easeOut(duration: 0.2), value: awayFromBottom)
        .safeAreaInset(edge: .bottom, spacing: 0) {
            composer
                // Grok: 31 pt side inset with the keyboard down, 19 pt with it up.
                .padding(.horizontal, composerSideInset)
                .animation(.easeOut(duration: 0.25), value: composerFocused)
                .padding(.top, 8)
                .padding(.bottom, composerFocused ? 0 : 2)
                .background(alignment: .top) {
                    LinearGradient(colors: [theme.bg.opacity(0), theme.bg], startPoint: .top, endPoint: .bottom)
                        .frame(height: 24)
                        .offset(y: -24)
                }
                .background(theme.bg)
        }
        .toolbar(.hidden, for: .navigationBar)
        .navigationBarBackButtonHidden(true)
        // Hiding the system back button also disables the edge-swipe pop; re-enable it.
        .background(InteractivePopGestureEnabler())
        // Full-chat swipes: right → previous agent chat (or Home), left → next (or create), up → archive + previous.
        .simultaneousGesture(chatNavSwipe)
        .sheet(isPresented: $showModelPicker, onDismiss: demoDismissed) {
            if let project {
                ModelPickerView(project: project).environmentObject(model).environment(\.theme, theme)
            }
        }
        .sheet(isPresented: $showSettings) {
            SettingsView().environmentObject(model).environment(\.theme, theme)
        }
        .sheet(isPresented: $showMembers) {
            MembersSheet(chatId: chatId).environmentObject(model).environment(\.theme, theme)
        }
        .sheet(isPresented: $showShare) {
            if let project {
                ProjectMembersSheet(project: project).environmentObject(model).environment(\.theme, theme)
            }
        }
        .sheet(isPresented: $showProfile) {
            AgentEditSheet(projectId: projectId).environmentObject(model).environment(\.theme, theme)
        }
        .task {
            guard DebugHooks.liveDemo, !model.demoStarted, DebugHooks.openChat == chatId || DebugHooks.openChat == nil, let text = DebugHooks.autoSend else { return }
            model.demoStarted = true
            while !model.caughtUp.contains(chatId) { try? await Task.sleep(nanoseconds: 200_000_000) }
            try? await Task.sleep(nanoseconds: 2_000_000_000)
            composerFocused = true
            try? await Task.sleep(nanoseconds: 800_000_000)
            for ch in text { draft.append(ch); try? await Task.sleep(nanoseconds: 45_000_000) }
            try? await Task.sleep(nanoseconds: 600_000_000)
            send()
            try? await Task.sleep(nanoseconds: 400_000_000)
            composerFocused = false
            // Wait for the turn: streaming starts, then ends (cap 45 s).
            var waited = 0
            while !streaming && waited < 150 { try? await Task.sleep(nanoseconds: 200_000_000); waited += 1 }
            while streaming && waited < 225 { try? await Task.sleep(nanoseconds: 200_000_000); waited += 1 }
            try? await Task.sleep(nanoseconds: 2_000_000_000)
            if rows.count > 8 {
                withAnimation(.easeInOut(duration: 1.2)) { scroller?.scrollTo(rows[rows.count - 8].id, anchor: .top) }
                try? await Task.sleep(nanoseconds: 2_400_000_000)
                withAnimation(.easeInOut(duration: 1.0)) { scroller?.scrollTo("bottom", anchor: .bottom) }
                try? await Task.sleep(nanoseconds: 1_600_000_000)
            }
            demoQueue = ["model", "threads", "attach", "voice", "settings"]
            demoPresentNext()
        }
        .task {
            guard DebugHooks.demo, model.isFixture, !model.demoStarted else { return }
            model.demoStarted = true
            try? await Task.sleep(nanoseconds: 1_800_000_000)
            composerFocused = true
            try? await Task.sleep(nanoseconds: 900_000_000)
            for ch in "Also keep your replies short, please." {
                draft.append(ch)
                try? await Task.sleep(nanoseconds: 55_000_000)
            }
            try? await Task.sleep(nanoseconds: 500_000_000)
            send()
            try? await Task.sleep(nanoseconds: 300_000_000)
            composerFocused = false
            try? await Task.sleep(nanoseconds: 7_500_000_000)
            dismiss()
        }
        .onAppear {
            model.markRead(chatId: chatId)
            model.visibleChatId = chatId
            if isPeople { model.markPeopleRead(chatId) }
            openPendingSearchIfNeeded()
            Task { await model.refreshActiveCall(chatId: chatId, projectId: isPeople ? nil : projectId) }
            if let show = DebugHooks.show {
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) {
                    switch show {
                    case "keyboard": composerFocused = true
                    case "settings": showSettings = true
                    case "voice": startCall(.voice)
                    case "model": showModelPicker = true
                    case "profile": openHeaderProfile()
                    case "edit": showProfile = true
                    case "userprofile": if let c = counterpart { model.showProfile(.user(ProfilePerson(c))) }
                    case "groupprofile": model.showProfile(.group(chatId: chatId, projectId: isPeople ? nil : projectId))
                    case "feedback": FeedbackCenter.shared.open(screen: "chat", chatId: chatId, projectId: isPeople ? nil : projectId)
                    case "call":
                        #if DEBUG
                        if let url = DebugHooks.callUrl, let token = DebugHooks.callToken {
                            Task { await calls.debugJoin(url: url, token: token, kind: DebugHooks.callKind == "video" ? .video : .voice,
                                                         chatId: chatId, projectId: isPeople ? peopleSlot : projectId) }
                        } else {
                            startCall(DebugHooks.callKind == "video" ? .video : .voice)
                        }
                        #else
                        startCall(.voice)
                        #endif
                    case "recording": recorder.start()
                    case "scrollup":
                        if let first = rows.first { withAnimation { scroller?.scrollTo(first.id, anchor: .top) } }
                    case "pending":
                        pending = [.init(name: "Q3 board deck v5.pdf", mime: "application/pdf", data: Data()),
                                   .init(name: "Photo Oct 15.jpg", mime: "image/jpeg", data: Data())]
                        composerFocused = true
                    default: break
                    }
                }
            }
        }
        .onDisappear {
            // Catch notifies that landed after the last onChange mark-read while we were still in the chat
            // (including rows that arrived while the app was inactive on a call — isWatching was false).
            if isPeople { model.markPeopleRead(chatId) }
            else { model.markRead(chatId: chatId) }
            if model.visibleChatId == chatId { model.visibleChatId = nil }
        }
        // Profile "search" sets pendingSearchChat while this chat is already on screen
        // (sheet over us), so onAppear alone never sees it — watch the published flag.
        .onChange(of: model.pendingSearchChat) { _, _ in openPendingSearchIfNeeded() }
        .onChange(of: messages, initial: true) { _, _ in
            refreshRows()
            model.markRead(chatId: chatId)
            if isPeople { model.markPeopleRead(chatId) }
        }
        // Adding an agent updates membership without a new message — rebuild so "@bob joined" appears.
        .onChange(of: people?.agents.map { "\($0.id):\($0.joinedAt)" } ?? []) { _, _ in
            refreshRows()
        }
        .onChange(of: model.caughtUp.contains(chatId), initial: true) { _, ready in
            if ready, !sentOnce, !DebugHooks.liveDemo, let text = DebugHooks.autoSend, DebugHooks.openChat == chatId {
                // Several messages separated by " ||| " go out a few seconds apart
                // (multitasking checks: the composer never waits for a reply).
                let parts = text.components(separatedBy: " ||| ").filter { !$0.isEmpty }
                draft = parts.first ?? text
                send()
                if parts.count > 1 {
                    Task {
                        for p in parts.dropFirst() {
                            try? await Task.sleep(nanoseconds: 3_000_000_000)
                            draft = p
                            send()
                        }
                    }
                }
            }
        }
    }

    /// Demo tour: present the next sheet; each one auto-dismisses and its
    /// onDismiss schedules the next, so presentations never overlap.
    private func demoPresentNext() {
        guard !demoQueue.isEmpty else { return }
        let step = demoQueue.removeFirst()
        let hold: Double = step == "voice" ? 3.5 : 3.0
        switch step {
        case "model": showModelPicker = true
        case "attach": showAttach = true
        case "voice": startCall(.voice)
        case "settings": showSettings = true
        default: break
        }
        guard step != "settings" else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + hold) {
            switch step {
            case "model": showModelPicker = false
            case "attach": showAttach = false
            case "voice": break
            default: break
            }
        }
    }

    private func demoDismissed() {
        guard DebugHooks.liveDemo, !demoQueue.isEmpty else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) { demoPresentNext() }
    }

    // MARK: Header (floating glass)

    /// Strip the messages scroll under (ref-02): frosted on Glass, solid fade on Matte.
    private var headerBlur: some View {
        ZStack {
            if theme.isGlass {
                Rectangle().fill(.ultraThinMaterial)
            }
            theme.bg.opacity(theme.isGlass ? 0.78 : 1)
        }
        .frame(height: (isPeople ? 156 : 118) + headerExtra)
        .mask(LinearGradient(colors: [.black, .black, .clear], startPoint: .top, endPoint: .bottom))
        .ignoresSafeArea(edges: .top)
        .allowsHitTesting(false)
    }

    private var header: some View {
        HStack {
            Button { dismiss() } label: {
                GlassCircle {
                    Image(systemName: "chevron.left")
                        .font(.system(size: 17, weight: .semibold))
                        .foregroundStyle(theme.text)
                }
            }
            .buttonStyle(.plain)
            Spacer()
            GlassPill {
                HStack(spacing: 6) {
                    if let people {
                        HStack(spacing: 6) {
                            if people.isGroup {
                                AvatarCluster(members: groupHeaderMembers(people), size: Theme.avatar,
                                              imageUrl: groupImageURL(people))
                                VStack(alignment: .leading, spacing: 0) {
                                    Text(people.title)
                                        .font(.system(size: Theme.body, weight: .semibold))
                                        .foregroundStyle(theme.text)
                                        .lineLimit(1)
                                    Text(groupSubtitle(people)).font(.system(size: 10)).foregroundStyle(theme.muted)
                                }
                            } else {
                                PersonAvatar(url: counterpart?.avatarUrl, handle: counterpart?.handle ?? "?", size: Theme.avatar)
                                VStack(alignment: .leading, spacing: 0) {
                                    Text(counterpart?.displayName ?? people.title)
                                        .font(.system(size: Theme.body, weight: .semibold))
                                        .foregroundStyle(theme.text)
                                        .lineLimit(1)
                                    if people.pending {
                                        Text("Invited · opens when they sign in").font(.system(size: 10)).foregroundStyle(theme.muted)
                                    } else if !people.agents.isEmpty {
                                        Text("\(people.agents.count) agent\(people.agents.count == 1 ? "" : "s")").font(.system(size: 10)).foregroundStyle(theme.muted)
                                    }
                                }
                            }
                        }
                        .contentShape(Rectangle())
                        .onTapGesture { openHeaderProfile() }
                        .accessibilityAddTraits(.isButton)
                        .accessibilityHint(people.isGroup ? "Opens group info" : "Opens the profile")
                    } else {
                        HStack(spacing: 8) {
                            if let project {
                                AgentAvatar(project: project, theme: theme, size: Theme.avatar, dot: machineDotColor(model.machine.state, theme: theme), working: streaming)
                            }
                            Text(name)
                                .font(.system(size: Theme.body, weight: .semibold))
                                .foregroundStyle(theme.text)
                                .lineLimit(1)
                        }
                        .contentShape(Rectangle())
                        .onTapGesture { openHeaderProfile() }
                        .accessibilityAddTraits(.isButton)
                        .accessibilityHint("Opens the agent profile")
                    }
                }
                .padding(.leading, 10)
                .padding(.trailing, 16)
                .frame(height: Theme.control)
                .contentShape(Capsule())
            }
            Spacer()
            if isPeople || project?.shared == true {
                Button { showMembers = true } label: {
                    GlassCircle {
                        Image(systemName: "person.badge.plus")
                            .font(.system(size: 15, weight: .semibold))
                            .foregroundStyle(theme.text)
                    }
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Members and agents")
            }
            Menu {
                if isPeople {
                    // Calls always start audio-only; turn the camera on from the call screen.
                    Button { startCall(.voice) } label: { Label("Call", systemImage: "phone") }
                    Button { openHeaderProfile() } label: { Label(people?.kind == "dm" ? "Profile" : "Group info", systemImage: "person.crop.circle") }
                    Button { showSearch = true; DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { searchFocused = true } } label: { Label("Search in chat", systemImage: "magnifyingglass") }
                    Button { showMembers = true } label: { Label("Members & agents", systemImage: "person.2") }
                    if let c = counterpart {
                        Button(role: .destructive) { model.block(c) } label: { Label("Block @\(c.handle)", systemImage: "hand.raised") }
                    }
                } else {
                    // One chat per agent (D78); its "projects" system chat and its groups stay reachable here.
                    if let sys = model.chats(of: projectId).first(where: { $0.isSystem }), sys.id != chatId {
                        Button { model.open(chat: sys) } label: { Label("Projects", systemImage: "square.grid.2x2") }
                    }
                    if chat?.isGroup == true || chat?.isSystem == true, let single = model.singleChat(of: projectId), single.id != chatId {
                        Button { model.open(chat: single) } label: { Label("Chat with \(project?.name ?? "agent")", systemImage: "bubble.left") }
                    }
                    ForEach(model.chats(of: projectId).filter { $0.isGroup && $0.id != chatId }) { g in
                        Button { model.open(chat: g) } label: { Label(g.title.isEmpty ? "Group" : g.title, systemImage: "person.2") }
                    }
                    Button { showModelPicker = true } label: { Label("Model · \(project?.shortModel ?? "")", systemImage: "cpu") }
                    // Calls Phase 2: every project chat is callable; the agent joins the room
                    // through the voice worker (1:1 = a call with the agent; shared
                    // projects = people plus the agent). Always start audio-only.
                    Button { startCall(.voice) } label: { Label(project?.shared == true ? "Call" : "Call \(project?.name ?? "agent")", systemImage: "phone") }
                    Button { showShare = true } label: { Label(project?.shared == true ? "Members" : "Share project…", systemImage: "person.2.badge.plus") }
                    if streaming {
                        Button(role: .destructive) { stop() } label: { Label("Stop \(name)", systemImage: "stop.circle") }
                    }
                }
                Divider()
                Button { FeedbackCenter.shared.open(screen: "chat", chatId: chatId, projectId: isPeople ? nil : projectId) } label: {
                    Label("Send feedback", systemImage: "exclamationmark.bubble")
                }
                Button { showSettings = true } label: { Label("Settings", systemImage: "gearshape") }
            } label: {
                GlassCircle {
                    Image(systemName: "ellipsis")
                        .font(.system(size: 17, weight: .semibold))
                        .foregroundStyle(theme.text)
                }
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, 16)
        .padding(.top, 6)
    }

    private var composerSideInset: CGFloat {
        composerFocused ? Theme.composerInsetKeyboard : Theme.composerInset
    }

    /// Grok's scroll-to-bottom control: a 36 pt field-coloured circle with a
    /// chevron, right-aligned with the composer, 18 pt above it.
    private var scrollToBottomButton: some View {
        Button {
            withAnimation(.easeOut(duration: 0.3)) { scroller?.scrollTo("bottom", anchor: .bottom) }
        } label: {
            ZStack {
                Circle().fill(theme.field)
                Circle().strokeBorder(theme.fieldBorder, lineWidth: 1)
                Image(systemName: "chevron.down")
                    .font(.system(size: 15, weight: .medium))
                    .foregroundStyle(theme.text)
            }
            .frame(width: Theme.voiceButton, height: Theme.voiceButton)
            .contentShape(Circle().inset(by: -(44 - Theme.voiceButton) / 2))
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Scroll to bottom")
    }

    // MARK: Chat navigation swipes

    /// Swipe right goes back to Home (#916/#925). There is no swipe between
    /// chats: the chats sheet (bubble icon to the right of the agent pill) switches chats.
    private var chatNavSwipe: some Gesture {
        DragGesture(minimumDistance: 50, coordinateSpace: .local)
            .onEnded { value in handleChatSwipe(value) }
    }

    private func handleChatSwipe(_ value: DragGesture.Value) {
        let dx = value.translation.width
        let absX = abs(dx), absY = abs(value.translation.height)
        let vx = value.velocity.width, vy = value.velocity.height
        if dx > 0, absX > 70, absX > absY * 1.25, abs(vx) >= abs(vy) || absX > 120 {
            composerFocused = false
            dismiss()
        }
    }

    // MARK: Transcript

    private func transcript(width: CGFloat, height: CGFloat) -> some View {
        ScrollViewReader { proxy in
            ScrollView {
                // VStack (not LazyVStack): a growing vertical TextField resizes the
                // bottom safe-area inset, and LazyVStack can drop every visible row
                // for a white transcript until the next layout pass.
                VStack(alignment: .leading, spacing: 0) {
                    Color.clear.frame(height: (isPeople ? 104 : 64) + headerExtra).onAppear { scroller = proxy }
                    if let banner = machineBannerText {
                        // Grok (Mac refs 69/70): a quiet notice, not a red strip — the
                        // title in the text colour, the reason muted.
                        VStack(spacing: 3) {
                            HStack(spacing: 6) {
                                if model.machine.state == .waking { ProgressView().controlSize(.mini).tint(theme.muted) }
                                if model.machine.state == .error {
                                    Image(systemName: "desktopcomputer").font(.system(size: 12))
                                }
                                Text(model.machine.state == .error ? "Can't reach your machine" : banner)
                            }
                            .font(.system(size: Theme.small, weight: model.machine.state == .error ? .semibold : .regular))
                            .foregroundStyle(model.machine.state == .error ? theme.text : theme.muted)
                            if model.machine.state == .error {
                                Text(banner)
                                    .font(.system(size: Theme.tiny))
                                    .foregroundStyle(theme.muted)
                                    .multilineTextAlignment(.center)
                            }
                        }
                        .frame(maxWidth: .infinity)
                        .padding(.bottom, 16)
                    }
                    if !model.caughtUp.contains(chatId) && messages.isEmpty && !model.isFixture {
                        // Known-empty (optimistic new chat seeds `messages[id] = []`):
                        // show the empty hint at once — no spinner flash while the
                        // room catches up on a thread we already know has no rows.
                        if model.hasKnownEmptyTranscript(chatId) {
                            if !isPeople { emptyHint }
                        } else {
                            HStack { Spacer(); ProgressView().tint(theme.muted); Spacer() }.padding(.top, 60)
                        }
                    } else if rows.isEmpty, !isPeople {
                        // People chats stay blank (iMessage-style); the agent empty
                        // copy would say "Ask @handle anything…" which is wrong here.
                        emptyHint
                    }
                    ForEach(Array(visibleRows.enumerated()), id: \.element.id) { i, row in
                        RowView(row: row, chatId: chatId, width: width)
                            .padding(.top, gap(before: i))
                            .id(row.id)
                            .contextMenu {
                                if isPeople, let m = messages.first(where: { $0.id == row.messageId }) {
                                    Button {
                                        replyTo = m
                                        talkTo = agentMember(for: m)?.member
                                        composerFocused = true
                                    } label: { Label("Reply", systemImage: "arrowshape.turn.up.left") }
                                    if m.role == .assistant, let h = m.senderHandle {
                                        Button {
                                            draft = "@\(h) " + draft
                                            talkTo = agentMember(for: m)?.member
                                            composerFocused = true
                                        } label: { Label("Mention @\(h)", systemImage: "at") }
                                    }
                                }
                                Button { UIPasteboard.general.string = m_text(row) } label: { Label("Copy", systemImage: "doc.on.doc") }
                            }
                    }
                    if model.isStreaming(chatId) {
                        workingLine.padding(.top, 20)
                    } else if model.isAwaitingReply(chatId) {
                        // Tick once a second so the awaiting-reply cheat expires
                        // (isAwaitingReply is time-capped) without a stuck line.
                        TimelineView(.periodic(from: .now, by: 1)) { _ in
                            if model.isAwaitingReply(chatId) {
                                workingLine.padding(.top, 20)
                            }
                        }
                    }
                    // Clears the 24 pt fade above the composer, so the last row
                    // (and "<name> is working") is never under the chat bar.
                    Color.clear.frame(height: 28).id("bottom")
                }
                .padding(.horizontal, 16)
                // Short threads: fill the viewport and pin rows to the bottom so
                // two bubbles cannot float mid-screen (scroll-anchor alone was
                // centering them on iOS 26). Empty-state copy keeps its own
                // top padding; tall threads are unchanged.
                .frame(
                    minHeight: (visibleRows.isEmpty && !streaming) ? 0 : height,
                    alignment: .bottom
                )
                // Fill the short-thread frame so taps on empty chrome hit this view.
                .contentShape(Rectangle())
            }
            .defaultScrollAnchor(.bottom)
            .scrollDismissesKeyboard(.interactively)
            // Tap empty chat chrome to dismiss the keyboard and free the transcript.
            .onTapGesture {
                composerFocused = false
                searchFocused = false
            }
            .modifier(AwayFromBottomReporter(away: $awayFromBottom))
            // Mask stays in the scroll view's bounds — `ignoresSafeArea` on the
            // mask mis-samples the fade when the composer/keyboard inset changes
            // and can blank the whole transcript (white screen mid-type).
            .mask {
                VStack(spacing: 0) {
                    LinearGradient(colors: [.clear, .black], startPoint: .top, endPoint: .bottom).frame(height: 110)
                    Color.black
                }
            }
            .onAppear { settleToBottom(proxy) }
            .onChange(of: rows.last?.id) { old, _ in
                if old == nil {
                    settleToBottom(proxy)
                } else {
                    withAnimation(.easeOut(duration: 0.2)) { proxy.scrollTo("bottom", anchor: .bottom) }
                }
            }
            .onChange(of: rows.count) { old, new in
                // The first snapshot can land as many rows at once after the
                // view is up (tap-open before the room's backfill, debug hook).
                if old == 0, new > 0 { settleToBottom(proxy) }
            }
            .onChange(of: model.caughtUp.contains(chatId)) { _, ready in
                if ready { settleToBottom(proxy) }
            }
            .onChange(of: streaming) { _, _ in
                withAnimation(.easeOut(duration: 0.2)) { proxy.scrollTo("bottom", anchor: .bottom) }
            }
            .onChange(of: composerFocused) { _, focused in
                guard focused else { return }
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) {
                    withAnimation(.easeOut(duration: 0.25)) { proxy.scrollTo("bottom", anchor: .bottom) }
                }
            }
        }
    }

    /// Pin the transcript to its end once the rows have a height: the first
    /// `scrollTo` runs before layout knows the content size, so repeat after
    /// the next runloop turn and once more after the rows settle.
    private func settleToBottom(_ proxy: ScrollViewProxy) {
        proxy.scrollTo("bottom", anchor: .bottom)
        DispatchQueue.main.async { proxy.scrollTo("bottom", anchor: .bottom) }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) { proxy.scrollTo("bottom", anchor: .bottom) }
    }

    private func m_text(_ row: Row) -> String {
        messages.first { $0.id == row.messageId }?.plainText ?? ""
    }

    /// Spacing: 8 pt between bubbles of one sender, 24 pt around sender changes
    /// and system lines (measured on the references).
    private func gap(before i: Int) -> CGFloat {
        guard i > 0 else { return 0 }
        let prev = rows[i - 1], cur = rows[i]
        if case .system = prev.kind { return 20 }
        if case .system = cur.kind { return 20 }
        return prev.role == cur.role ? 8 : 24
    }

    private var machineBannerText: String? {
        switch model.machine.state {
        case .waking: return "Waking your machine…"
        case .error: return model.machineDetail ?? (model.machine.alwaysOn ? "Your agents are safe — their machine just can't be reached right now." : "Could not wake your machine")
        case .awake, .hibernatedLight, .hibernatedDeep, .stoppedByUser: return model.link == .reconnecting ? "Reconnecting…" : nil
        }
    }

    private var emptyHint: some View {
        VStack(spacing: 10) {
            if let project {
                AgentAvatar(project: project, theme: theme, size: 64)
            }
            Text(name)
                .font(.system(size: 20, weight: .semibold))
                .foregroundStyle(theme.text)
            Text("Ask \(name) anything. It plans, delegates, or answers.")
                .font(.system(size: Theme.small))
                .foregroundStyle(theme.muted)
                .multilineTextAlignment(.center)
        }
        .frame(maxWidth: .infinity)
        .padding(.top, 140)
        .padding(.bottom, 40)
    }

    /// Avatars + handles of everyone in a people chat; agents show their owner.
    /// Tap a chip to talk to that person or agent without typing `@` — the composer
    /// prompt and the next send follow the selection.
    private func memberChips(_ people: PeopleChat) -> some View {
        // Skip me — you don't address yourself; keeps a 1:1 DM to one chip + Agent.
        let others = people.members.filter { $0.id != model.user?.id }
        return ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(others) { m in
                    let selected = effectiveTalkTo == m.member
                    Button {
                        // Tapping the selected chip clears it. Do not promote an inferred /
                        // reply-only selection into a sticky talkTo — that locked @bob on
                        // every later line in Allan+bob DMs.
                        if effectiveTalkTo == m.member {
                            talkTo = nil
                        } else {
                            talkTo = m.member
                        }
                        replyTo = nil
                    } label: {
                        HStack(spacing: 6) {
                            if m.isAgent, let p = m.project {
                                AgentAvatar(project: p, theme: theme, size: 18, still: true)
                            } else {
                                PersonAvatar(url: m.avatarUrl, handle: m.handle, size: 18)
                            }
                            Text(m.isAgent ? "@\(mentionSlug(m.project?.name ?? m.handle))" : "@\(m.handle)")
                                .font(.system(size: Theme.tiny, weight: .medium))
                                .foregroundStyle(theme.text)
                            if m.isAgent, let o = m.ownerHandle {
                                Text("· @\(o)").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                            }
                        }
                        .padding(.leading, 6).padding(.trailing, 10).padding(.vertical, 5)
                        .background(Capsule().fill(selected ? theme.bubbleUser.opacity(theme.isDark ? 0.55 : 0.18) : theme.bubbleAssistant))
                        .overlay(Capsule().strokeBorder(selected ? theme.text.opacity(0.35) : Color.clear, lineWidth: 1))
                        .contentShape(Capsule().inset(by: -8))
                    }
                    .buttonStyle(.plain)
                    .contextMenu {
                        Button {
                            if m.isAgent { model.showProfile(.agent(projectId: m.id)) } else { model.showProfile(.user(ProfilePerson(m))) }
                        } label: { Label("Profile", systemImage: "person.crop.circle") }
                    }
                    .accessibilityLabel(selected ? "Talking to \(m.displayName)" : "Talk to \(m.displayName)")
                }
                Button { showMembers = true } label: {
                    HStack(spacing: 4) {
                        Image(systemName: "plus").font(.system(size: 11, weight: .bold))
                        Text("Agent").font(.system(size: Theme.tiny, weight: .medium))
                    }
                    .foregroundStyle(theme.text)
                    .padding(.horizontal, 10).padding(.vertical, 6)
                    .background(Capsule().fill(theme.field))
                    .overlay(Capsule().strokeBorder(theme.fieldBorder, lineWidth: 1))
                }
                .buttonStyle(.plain)
            }
            // Room for avatar + padding so a horizontal ScrollView does not clip chips.
            .padding(.vertical, 4)
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    /// "Cloning RaoFoundation/subtensor" when the harness says what it is doing, else "<name> is working".
    private var workingText: String {
        if let a = model.currentActivity(chatId) { return a }
        return "\(workingName) is working"
    }

    private var workingLine: some View {
        HStack(spacing: 10) {
            if isPeople, let m = messages.last(where: { $0.role == .assistant && $0.isStreaming }),
               let agent = people?.agents.first(where: { mentionSlug($0.project?.name ?? "") == m.senderHandle || $0.handle == m.senderHandle }),
               let p = agent.project {
                AgentAvatar(project: p, theme: theme, size: 26, working: true)
            } else if let project {
                AgentAvatar(project: project, theme: theme, size: 26, working: true)
            }
            WorkingStatusText(text: workingText, color: theme.muted)
                .id(workingText)
            Spacer(minLength: 0)
        }
        .padding(.leading, 12)
        .accessibilityLabel(workingText)
        .animation(.easeInOut(duration: 0.25), value: workingText)
    }

    // MARK: Composer

    private var composer: some View {
        VStack(spacing: 8) {
            if let q = mentionQuery, isPeople, let people {
                mentionList(people, query: q)
            }
            if let r = replyTo {
                HStack(spacing: 8) {
                    Image(systemName: "arrowshape.turn.up.left").font(.system(size: 12)).foregroundStyle(theme.muted)
                    Text("Replying to @\(r.senderHandle ?? (r.role == .assistant ? "agent" : "you"))").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                    Text(r.plainText).font(.system(size: Theme.tiny)).foregroundStyle(theme.text).lineLimit(1)
                    Spacer()
                    Button { replyTo = nil } label: { Image(systemName: "xmark").font(.system(size: 11, weight: .bold)).foregroundStyle(theme.muted) }
                        .buttonStyle(.plain)
                }
                .padding(.horizontal, 14).padding(.vertical, 8)
                .background(RoundedRectangle(cornerRadius: 14, style: .continuous).fill(theme.bubbleAssistant))
                .padding(.leading, Theme.plusButton + 12)
            }
            if !pending.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(pending) { a in
                            HStack(spacing: 6) {
                                Image(systemName: a.mime.hasPrefix("image/") ? "photo" : (a.mime.hasPrefix("audio/") ? "waveform" : "doc"))
                                    .font(.system(size: 12))
                                Text(a.name).font(.system(size: Theme.tiny)).lineLimit(1)
                                Button { pending.removeAll { $0.id == a.id } } label: {
                                    Image(systemName: "xmark").font(.system(size: 10, weight: .bold))
                                }
                                .buttonStyle(.plain)
                            }
                            .foregroundStyle(theme.text)
                            .padding(.horizontal, 10).padding(.vertical, 6)
                            .background(Capsule().fill(theme.bubbleAssistant))
                        }
                    }
                    .padding(.leading, Theme.plusButton + 12)
                }
            }
            // Bottom-align the "+" with a multi-line field so growth expands upward.
            HStack(alignment: .bottom, spacing: 12) {
                Button { showAttach = true } label: {
                    ZStack {
                        Circle().fill(theme.field)
                        Circle().strokeBorder(theme.fieldBorder.opacity(0.6), lineWidth: 1)
                        Image(systemName: "plus")
                            .font(.system(size: 21, weight: .regular))
                            .foregroundStyle(theme.text)
                    }
                    .frame(width: Theme.plusButton, height: Theme.plusButton)
                    .contentShape(Circle().inset(by: -(44 - Theme.plusButton) / 2))
                }
                .buttonStyle(.plain)
                .sheet(isPresented: $showAttach, onDismiss: demoDismissed) {
                    AttachSheet(name: name) {
                        showAttach = false
                        DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { showPhotos = true }
                    } onFile: {
                        showAttach = false
                        DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { showFiles = true }
                    } onVoice: {
                        showAttach = false
                        DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { startCall(.voice) }
                    }
                    .environment(\.theme, theme)
                }
                .photosPicker(isPresented: $showPhotos, selection: $photoItem, matching: .any(of: [.images, .videos]))
                .fileImporter(isPresented: $showFiles, allowedContentTypes: [.item], allowsMultipleSelection: false) { result in
                    if case .success(let urls) = result, let url = urls.first {
                        let ok = url.startAccessingSecurityScopedResource()
                        defer { if ok { url.stopAccessingSecurityScopedResource() } }
                        if let data = try? Data(contentsOf: url) {
                            let mime = UTType(filenameExtension: url.pathExtension)?.preferredMIMEType ?? "application/octet-stream"
                            pending.append(.init(name: url.lastPathComponent, mime: mime, data: data))
                        }
                    }
                }
                .onChange(of: photoItem) { _, item in
                    guard let item else { return }
                    Task {
                        if let data = try? await item.loadTransferable(type: Data.self) {
                            let type = item.supportedContentTypes.first
                            let picked = ChatView.imageForUpload(data, type: type)
                            // POSIX stamp (no locale letters, no `:` — voice memos use the same idea).
                            let stamp = ChatView.photoStamp.string(from: Date())
                            pending.append(.init(name: "Photo \(stamp).\(picked.ext)", mime: picked.mime, data: picked.data))
                        }
                        photoItem = nil
                    }
                }

                textField
            }
        }
    }

    /// Max height for the growing composer field.
    ///
    /// A vertical `TextField` fills the height its parent proposes (up to
    /// `lineLimit`), so an empty field inside `safeAreaInset` was stretching to
    /// eight lines and covering the transcript. Hug content with `fixedSize`,
    /// and still cap at 8 lines (Grok's 22 pt body pitch) for wrap glitches.
    private var composerFieldMaxHeight: CGFloat {
        Theme.fieldHeight + 7 * 22
    }

    /// Composer field: type normally, or hold the mic to dictate. The mic stays
    /// mounted while recording so release can finish and send without a second tap.
    private var textField: some View {
        HStack(spacing: 8) {
            if recorder.recording {
                Button {
                    memoHoldActive = false
                    memoIgnoreRelease = false
                    memoFinishWhenPermission = false
                    memoPressStartedAt = nil
                    recorder.cancel()
                } label: {
                    Image(systemName: "xmark").font(.system(size: 15, weight: .semibold)).foregroundStyle(theme.icon)
                }
                .buttonStyle(.plain)
                .padding(.leading, 14)
                Circle().fill(theme.danger).frame(width: 8, height: 8)
                // Level bars drawn into whatever width is left (newest on the right),
                // so the recorder row never pushes the composer past the screen edge.
                Canvas { ctx, size in
                    let pitch: CGFloat = 5, w: CGFloat = 3
                    let levels = recorder.levels
                    let n = min(levels.count, max(0, Int(size.width / pitch)))
                    for i in 0..<n {
                        let l = CGFloat(levels[levels.count - n + i])
                        let h = min(size.height, 4 + l * 20)
                        let x = size.width - CGFloat(n - i) * pitch
                        let rect = CGRect(x: x, y: (size.height - h) / 2, width: w, height: h)
                        ctx.fill(Path(roundedRect: rect, cornerRadius: w / 2), with: .color(theme.text))
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: 24)
                Text(recorder.timeLabel)
                    .font(.system(size: Theme.small, design: .monospaced))
                    .foregroundStyle(theme.text)
            } else {
                TextField("", text: $draft, prompt: Text(composerPrompt).foregroundStyle(theme.placeholder), axis: .vertical)
                    .lineLimit(1...8)
                    // Size to the typed text (one line when empty), not the offered height.
                    .fixedSize(horizontal: false, vertical: true)
                    .font(.system(size: Theme.body))
                    .foregroundStyle(theme.text)
                    .tint(Color(hex: "#1084FE")!)
                    .focused($composerFocused)
                    .padding(.leading, 16)
                    .padding(.vertical, 10)
                    .onChange(of: draft) { _, d in
                        // `@` completion: the token the caret is in (approximated by the last token).
                        guard isPeople, let last = d.split(separator: " ", omittingEmptySubsequences: false).last, last.hasPrefix("@"), !d.hasSuffix(" ") else {
                            mentionQuery = nil
                            return
                        }
                        mentionQuery = String(last.dropFirst()).lowercased()
                    }
            }
            if draft.isEmpty || recorder.recording {
                // Press/release like Mac: tap toggles recording on; hold ≥0.25s then
                // release sends. highPriorityGesture so a focused TextField cannot
                // steal the touch. The mic stays mounted while recording so release
                // can finish without a second tap.
                Image(systemName: "mic")
                    .font(.system(size: 17))
                    .foregroundStyle(recorder.recording ? theme.danger : theme.icon)
                    .frame(width: 44, height: Theme.fieldHeight)
                    .contentShape(Rectangle())
                    .padding(.trailing, 2)
                    .id("memo-mic")
                    .highPriorityGesture(
                        DragGesture(minimumDistance: 0)
                            .onChanged { _ in
                                guard !memoHoldActive else { return }
                                memoHoldActive = true
                                memoPressStartedAt = Date()
                                memoFinishWhenPermission = false
                                if recorder.recording {
                                    memoIgnoreRelease = true
                                    finishMemoHold()
                                } else {
                                    memoIgnoreRelease = false
                                    Haptics.recordStart()
                                    recorder.start()
                                }
                            }
                            .onEnded { _ in
                                let held = Date().timeIntervalSince(memoPressStartedAt ?? Date())
                                memoHoldActive = false
                                memoPressStartedAt = nil
                                if memoIgnoreRelease {
                                    memoIgnoreRelease = false
                                    return
                                }
                                // Hold-to-talk: release after the threshold sends.
                                if held >= 0.25 {
                                    if recorder.recording {
                                        finishMemoHold()
                                    } else {
                                        // Permission still pending; finish when it starts.
                                        memoFinishWhenPermission = true
                                    }
                                }
                                // Short tap: leave recording on (click-to-toggle).
                            }
                    )
            }
            // While recording: tap send also finishes (same as release). Otherwise
            // Grok keeps the composer usable while the bot works: text sends, an
            // empty field starts a voice call with the agent (phone icon next to mic).
            // Stop lives in the … menu.
            Button(action: {
                if recorder.recording {
                    memoHoldActive = false
                    memoIgnoreRelease = false
                    memoFinishWhenPermission = false
                    memoPressStartedAt = nil
                    finishMemoHold()
                } else if canSend {
                    send()
                } else {
                    startCall(.voice)
                }
            }) {
                Circle()
                    .fill(theme.primary)
                    .frame(width: Theme.voiceButton, height: Theme.voiceButton)
                    .overlay(
                        Image(systemName: (canSend || recorder.recording) ? "arrow.up" : idleButtonGlyph)
                            .font(.system(size: 16, weight: .bold))
                            .foregroundStyle(theme.onPrimary)
                    )
                    .contentShape(Circle().inset(by: -(44 - Theme.voiceButton) / 2))
            }
            .buttonStyle(.plain)
            .accessibilityLabel((canSend || recorder.recording) ? "Send" : "Call")
            .padding(.trailing, 3)
        }
        // Hard max so a TextField layout glitch cannot paint over the transcript.
        .frame(minHeight: Theme.fieldHeight, maxHeight: composerFieldMaxHeight, alignment: .bottom)
        .fixedSize(horizontal: false, vertical: true)
        .background(RoundedRectangle(cornerRadius: Theme.fieldHeight / 2, style: .continuous).fill(theme.field))
        .overlay(
            RoundedRectangle(cornerRadius: Theme.fieldHeight / 2, style: .continuous)
                .strokeBorder(
                    recorder.recording ? theme.danger.opacity(0.5)
                        : (needsAnswer ? theme.warn.opacity(0.7) : theme.fieldBorder),
                    lineWidth: 1
                )
        )
        .alert("Microphone access is off", isPresented: $recorder.permissionDenied) {
            Button("OK", role: .cancel) { }
        } message: {
            Text("Allow the microphone in Settings to record a voice memo.")
        }
        // Hold released before permission returned: finish once recording starts.
        // A short tap leaves recording on (click-to-toggle), so do not auto-finish then.
        .onChange(of: recorder.recording) { _, isOn in
            guard isOn, memoFinishWhenPermission else { return }
            memoFinishWhenPermission = false
            finishMemoHold()
        }
    }

    /// Stop the hold-to-talk memo and send its transcript (no-op if too short).
    private func finishMemoHold() {
        guard recorder.recording else { return }
        Haptics.send()
        if let memo = recorder.stop() {
            sendMemo(.init(name: memo.name, mime: "audio/m4a", data: memo.data))
        }
    }

    private var canSend: Bool {
        !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !pending.isEmpty
    }

    /// Idle round button: phone — starts a voice call (agent or people chat).
    private var idleButtonGlyph: String { "phone.fill" }

    /// Members whose handle/slug starts with the typed `@` prefix; agents first.
    private func mentionList(_ people: PeopleChat, query: String) -> some View {
        let agents = people.agents.filter { query.isEmpty || mentionSlug($0.project?.name ?? $0.handle).hasPrefix(query) }
        let humans = people.humans.filter { $0.id != model.user?.id && (query.isEmpty || $0.handle.lowercased().hasPrefix(query)) }
        let items = agents + humans
        return Group {
            if !items.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(items) { m in
                            Button { completeMention(m) } label: {
                                HStack(spacing: 6) {
                                    if m.isAgent, let p = m.project { AgentAvatar(project: p, theme: theme, size: 20, still: true) } else { PersonAvatar(url: m.avatarUrl, handle: m.handle, size: 20) }
                                    Text("@\(m.isAgent ? mentionSlug(m.project?.name ?? m.handle) : m.handle)")
                                        .font(.system(size: Theme.small, weight: .medium)).foregroundStyle(theme.text)
                                }
                                .padding(.leading, 6).padding(.trailing, 12).padding(.vertical, 7)
                                .background(Capsule().fill(theme.bubbleAssistant))
                            }
                            .buttonStyle(.plain)
                        }
                    }
                    .padding(.leading, Theme.plusButton + 12)
                }
            }
        }
    }

    private func completeMention(_ m: ChatMember) {
        let slug = m.isAgent ? mentionSlug(m.project?.name ?? m.handle) : m.handle
        var parts = draft.split(separator: " ", omittingEmptySubsequences: false).map(String.init)
        if let last = parts.last, last.hasPrefix("@") { parts[parts.count - 1] = "@\(slug)" } else { parts.append("@\(slug)") }
        draft = parts.joined(separator: " ") + " "
        talkTo = m.member
        mentionQuery = nil
    }

    /// Fixed-locale stamp for photo attachment names (relay blob keys must be ASCII).
    private static let photoStamp: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd HH.mm.ss"
        return f
    }()

    /// Photos leave the phone as JPEG or PNG no wider than 2048 px: the models behind the
    /// agent read those; a 12 MB HEIC straight from the camera they do not. Videos and
    /// anything that is not an image pass through unchanged.
    static func imageForUpload(_ data: Data, type: UTType?) -> (data: Data, mime: String, ext: String) {
        let original = (data: data, mime: type?.preferredMIMEType ?? "image/jpeg", ext: type?.preferredFilenameExtension ?? "jpg")
        if let type, type.conforms(to: .movie) || type.conforms(to: .video) { return original }
        guard let image = UIImage(data: data) else { return original }
        let longest = max(image.size.width, image.size.height) * image.scale
        let isWebFormat = type.map { $0.conforms(to: .jpeg) || $0.conforms(to: .png) } ?? false
        if isWebFormat && longest <= 2048 && data.count <= 4 * 1024 * 1024 { return original }
        let scale = min(1, 2048 / max(longest, 1))
        let size = CGSize(width: (image.size.width * scale).rounded(), height: (image.size.height * scale).rounded())
        let format = UIGraphicsImageRendererFormat.default()
        format.scale = 1
        let drawn = UIGraphicsImageRenderer(size: size, format: format).image { _ in image.draw(in: CGRect(origin: .zero, size: size)) }
        guard let jpeg = drawn.jpegData(compressionQuality: 0.85) else { return original }
        return (jpeg, "image/jpeg", "jpg")
    }

    /// Chip / inferred agent, or `"everyone"` when the draft says so — matches `composerPrompt`.
    /// A bare person name in the text drops a sticky agent so bob does not answer "Allan?".
    private func sendAddress(for text: String) -> String? {
        if AppModel.addressesEveryone(text) || talkTo == "everyone" { return "everyone" }
        let members = people?.members ?? []
        if AppModel.mentions(in: text, members: members).contains(where: { $0.hasPrefix("agent:") }) {
            return effectiveTalkTo
        }
        if !AppModel.namedPeople(in: text, members: members).isEmpty {
            return nil
        }
        return effectiveTalkTo
    }

    /// Finish dictation and send: transcript (plus any typed draft) goes out as text.
    /// The audio is only used for STT — it is not attached to the chat.
    private func sendMemo(_ memo: AppModel.PendingAttachment) {
        let typed = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        let extras = pending
        let reply = replyTo?.id
        let address = sendAddress(for: typed)
        pending.removeAll()
        draft = ""
        replyTo = nil
        mentionQuery = nil
        pinTalkTo(afterSending: address)
        sentOnce = true
        transcribing = true
        Task {
            let sttProject: Id? = project?.id ?? (isPeople ? "\(peopleSlot):\(chatId)" : nil)
            let transcript = await model.transcribe(projectId: sttProject, memo: memo)
            transcribing = false
            let text = [typed, transcript ?? ""].filter { !$0.isEmpty }.joined(separator: "\n\n")
            guard !text.isEmpty || !extras.isEmpty else { return }
            if isPeople {
                model.sendPeople(chatId: chatId, text: text, replyTo: reply, address: address, attachments: extras)
            } else if needsAnswer && extras.isEmpty {
                model.answer(chatId: chatId, text: text)
            } else {
                model.send(chatId: chatId, text: text, attachments: extras)
            }
        }
    }

    /// After a send in a DM, keep an explicit agent chip selected so the next line stays with
    /// them. Do not promote a reply-only / @-only address into a sticky chip when a human peer
    /// is in the chat — otherwise Allan+bob DMs leave Message @bob on unrelated follow-ups.
    /// In a group, clear the chip so the next line is unaddressed and the decision model picks
    /// who replies (an explicit chip tap still addresses that send). Always clear "everyone".
    private func pinTalkTo(afterSending address: String?) {
        if address == "everyone" || people?.kind == "group" {
            talkTo = nil
        } else if let address, address.hasPrefix("agent:") {
            let hasHumanPeer = people?.humans.contains { $0.id != model.user?.id } == true
            if hasHumanPeer, talkTo != address {
                // Reply-to or typed @agent addressed this send; leave the next line free.
                return
            }
            talkTo = address
        }
    }

    private func send() {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty || !pending.isEmpty else { return }
        Haptics.send()
        if isPeople {
            let address = sendAddress(for: text)
            model.sendPeople(chatId: chatId, text: text, replyTo: replyTo?.id, address: address, attachments: pending)
            pending.removeAll()
            draft = ""
            replyTo = nil
            mentionQuery = nil
            pinTalkTo(afterSending: address)
            sentOnce = true
            return
        }
        if needsAnswer && pending.isEmpty {
            model.answer(chatId: chatId, text: text)
        } else {
            model.send(chatId: chatId, text: text, attachments: pending)
        }
        pending.removeAll()
        draft = ""
        sentOnce = true
    }

    private func stop() {
        model.stop(chatId: chatId)
    }
}

/// Soft fade + streaming shimmer on "<name> is working" (Mac `working_line`).
private struct WorkingStatusText: View {
    let text: String
    let color: Color
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        if reduceMotion {
            label(color)
        } else {
            TimelineView(.animation(minimumInterval: 1.0 / 30.0)) { context in
                let t = context.date.timeIntervalSinceReferenceDate
                // Triangle wave 0…1 over 1.1 s — same period as Mac PulseClock.
                let cycle = t.truncatingRemainder(dividingBy: 1.1) / 1.1
                let pulse = cycle < 0.5 ? cycle * 2.0 : 2.0 - cycle * 2.0
                let fade = 0.55 + 0.45 * pulse
                // Bright band streams left → right across the label.
                let stream = t.truncatingRemainder(dividingBy: 1.8) / 1.8
                label(color.opacity(fade))
                    .overlay {
                        GeometryReader { geo in
                            let width = geo.size.width
                            let band = max(width * 0.5, 1)
                            LinearGradient(
                                colors: [.clear, color, .clear],
                                startPoint: .leading,
                                endPoint: .trailing
                            )
                            .frame(width: band)
                            .offset(x: stream * (width + band) - band)
                        }
                        .mask(label(color))
                        .allowsHitTesting(false)
                    }
            }
        }
    }

    private func label<S: ShapeStyle>(_ style: S) -> some View {
        Text(text)
            .font(.system(size: Theme.small))
            .foregroundStyle(style)
    }
}

/// `.navigationBarBackButtonHidden(true)` turns off the system edge-swipe
/// back gesture; this turns it back on for the enclosing UINavigationController.
private struct InteractivePopGestureEnabler: UIViewControllerRepresentable {
    func makeUIViewController(context: Context) -> Controller { Controller() }
    func updateUIViewController(_ uiViewController: Controller, context: Context) {}

    final class Controller: UIViewController, UIGestureRecognizerDelegate {
        override func viewDidAppear(_ animated: Bool) {
            super.viewDidAppear(animated)
            guard let nav = enclosingNavigationController(),
                  let pop = nav.interactivePopGestureRecognizer else { return }
            pop.isEnabled = true
            pop.delegate = self
        }

        func gestureRecognizerShouldBegin(_ gestureRecognizer: UIGestureRecognizer) -> Bool {
            (enclosingNavigationController()?.viewControllers.count ?? 0) > 1
        }

        /// Background representables sometimes have a nil `navigationController`;
        /// walk parents / the responder chain to find the stack that pushed chat.
        private func enclosingNavigationController() -> UINavigationController? {
            if let nav = navigationController { return nav }
            var parent = self.parent
            while let current = parent {
                if let nav = current as? UINavigationController { return nav }
                if let nav = current.navigationController { return nav }
                parent = current.parent
            }
            var responder: UIResponder? = view
            while let current = responder {
                if let nav = current as? UINavigationController { return nav }
                if let vc = current as? UIViewController, let nav = vc.navigationController { return nav }
                responder = current.next
            }
            return nil
        }
    }
}

/// "+" sheet: Photo or video, File, Voice chat — Grok's attach options as a
/// short bottom sheet.
struct AttachSheet: View {
    @Environment(\.theme) private var theme
    let name: String
    let onPhoto: () -> Void
    let onFile: () -> Void
    let onVoice: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            Capsule().fill(theme.muted.opacity(0.4)).frame(width: 36, height: 5).padding(.top, 8).padding(.bottom, 14)
            row(icon: "photo", title: "Photo or video", subtitle: "From your library", action: onPhoto)
            row(icon: "doc", title: "File", subtitle: "From Files or iCloud Drive", action: onFile)
            row(icon: "waveform", title: "Voice chat", subtitle: "Talk with \(name) live", action: onVoice)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 16)
        .background(theme.bg)
        .presentationDetents([.height(250)])
        .presentationDragIndicator(.hidden)
        .presentationBackground(theme.bg)
    }

    private func row(icon: String, title: String, subtitle: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 14) {
                GlassCircle(size: 44) {
                    Image(systemName: icon).font(.system(size: 18)).foregroundStyle(theme.text)
                }
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.system(size: Theme.body)).foregroundStyle(theme.text)
                    Text(subtitle).font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                }
                Spacer()
            }
            .padding(.vertical, 8)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

/// Talking-model picker: ids the relay proxy serves, plus the effort level.
struct ModelPickerView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    let project: Project
    @State private var query = ""
    @State private var effort: Effort = .low

    private var ids: [String] {
        var all = model.models
        if !all.contains(project.model) { all.insert(project.model, at: 0) }
        let q = query.trimmingCharacters(in: .whitespaces).lowercased()
        return q.isEmpty ? all : all.filter { $0.lowercased().contains(q) }
    }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    HStack(spacing: 8) {
                        Image(systemName: "magnifyingglass")
                            .font(.system(size: 15, weight: .medium))
                            .foregroundStyle(theme.muted)
                        TextField("", text: $query, prompt: Text("Filter models").foregroundStyle(theme.placeholder))
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .font(.system(size: Theme.body))
                            .foregroundStyle(theme.text)
                        if !query.isEmpty {
                            Button { query = "" } label: {
                                Image(systemName: "xmark.circle.fill").foregroundStyle(theme.muted)
                            }
                            .buttonStyle(.plain)
                        }
                    }
                    .listRowBackground(theme.card)
                }
                Section("Reasoning") {
                    Picker("Effort", selection: $effort) {
                        ForEach(Effort.allCases) { e in Text(e.label).tag(e) }
                    }
                    .pickerStyle(.segmented)
                    .onChange(of: effort) { _, new in
                        model.setModel(projectId: project.id, model: project.model, effort: new)
                    }
                }
                Section("Model") {
                    if model.models.isEmpty {
                        Text("Loading models from the relay…").foregroundStyle(theme.muted)
                    }
                    ForEach(ids, id: \.self) { id in
                        Button {
                            model.setModel(projectId: project.id, model: id, effort: effort)
                            dismiss()
                        } label: {
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(Project.shortModel(id)).foregroundStyle(theme.text)
                                    Text(id).font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                                }
                                Spacer()
                                if id == project.model {
                                    Image(systemName: "checkmark").foregroundStyle(theme.text)
                                }
                            }
                        }
                        .listRowBackground(theme.card)
                    }
                }
                if !project.modelNote.isEmpty {
                    Section {
                        Text(project.modelNote).font(.system(size: Theme.tiny)).foregroundStyle(theme.warn)
                    }
                }
            }
            .scrollContentBackground(.hidden)
            .background(theme.bg)
            .navigationTitle("Model")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } }
            }
            .onAppear { effort = project.effort }
        }
        .presentationDetents([.medium, .large])
    }
}

/// Reports when the scroll view sits more than 120 pt above its end (iOS 18+;
/// earlier systems never show the chevron).
private struct AwayFromBottomReporter: ViewModifier {
    @Binding var away: Bool

    func body(content: Content) -> some View {
        if #available(iOS 18, *) {
            content.onScrollGeometryChange(for: Bool.self) { g in
                g.contentOffset.y + g.containerSize.height < g.contentSize.height - 120
            } action: { _, isAway in
                away = isAway
            }
        } else {
            content
        }
    }
}
