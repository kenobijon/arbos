// Debug-only fixture: the Grok Bot reference conversation mapped onto Arbos
// concepts, so the simulator can be screenshotted without a relay. Enabled by
// `ARBOS_FIXTURE=1` in the launch environment. Not compiled into Release.

import Foundation

#if DEBUG
extension AppModel {
    func loadFixture() {
        isFixture = true
        let now = nowMs()
        let comms = Project(id: "fx-comms", name: "Comms", icon: "Co", color: "#1084FE", state: .awake, stateSince: now - 3_600_000,
                            stateDetail: nil, model: "anthropic/claude-haiku-5.5", effort: .low, spendTodayUsd: 0.4, spendCapUsd: 25)
        let affine = Project(id: "fx-affine", name: "Affine", icon: "Af", color: "#10B981", state: .awake, stateSince: now - 3_600_000,
                             stateDetail: nil, model: "anthropic/claude-sonnet-5.5", effort: .medium, spendTodayUsd: 1.2, spendCapUsd: 25)
        let hyper = Project(id: "fx-hyper", name: "Hyper", icon: "Hy", color: "#FF7A00", state: .awake, stateSince: now - 3_600_000,
                            stateDetail: nil, model: "anthropic/claude-haiku-5.5", effort: .low, spendTodayUsd: 0, spendCapUsd: 25)
        let scout = Project(id: "fx-scout", name: "Talent Scout", icon: "Ts", color: "#8E5A3C", state: .awake, stateSince: now,
                            stateDetail: nil, model: "anthropic/claude-haiku-5.5", effort: .low, spendTodayUsd: 0, spendCapUsd: 25)
        let expense = Project(id: "fx-expense", name: "Expense Manager", icon: "Em", color: "#FF2D9B", state: .awake, stateSince: now,
                              stateDetail: nil, model: "anthropic/claude-haiku-5.5", effort: .low, spendTodayUsd: 0, spendCapUsd: 25)
        let invoices = Project(id: "fx-invoices", name: "Invoice Collector", icon: "Ic", color: "#8E8E93", state: .awake, stateSince: now,
                               stateDetail: nil, model: "anthropic/claude-haiku-5.5", effort: .low, spendTodayUsd: 0, spendCapUsd: 25)
        var list = [comms, affine, hyper, scout, expense, invoices]
        list[0].description = "Keeps your Discord and email in your voice: short, casual, no sign-offs. Drafts before it sends."
        list[0].machineState = "awake"; list[0].machineClass = "daytona"; list[0].machineSnapshot = "arbos-agent-0.1.11"; list[0].machineAlwaysOn = true
        // Every agent is a coloured tree (D74); the colours differ so screenshots read well.
        let tints = ["fx-comms": "blue", "fx-affine": "green", "fx-hyper": "orange", "fx-expense": "pink", "fx-invoices": "teal", "fx-scout": "violet"]
        for i in list.indices { list[i].avatarColor = tints[list[i].id] }
        setFixture(projects: list, now: now)
    }

    private func setFixture(projects list: [Project], now: Int64) {
        projects = list
        var allChats: [Id: [Chat]] = [:]
        var allMessages: [Id: [Message]] = [:]
        let previews: [Id: (String, Int64)] = [
            "fx-affine": ("Shortlist of 6 candidates ready.", now - 19 * 60_000),
            "fx-hyper": ("A/B copy variants ready to review.", now - 36 * 60_000),
            "fx-scout": ("Outreach drafts queued for approval.", now - 26 * 3_600_000),
            "fx-expense": ("Receipts coded — one needs a look.", now - 27 * 3_600_000),
            "fx-invoices": ("Pulled 9 invoices from vendor portals.", now - 3 * 86_400_000),
        ]
        for p in list {
            let chat = Chat(id: "\(p.id)-main", projectId: p.id, title: p.name, isPinned: true, unread: p.id == "fx-affine",
                            lastMessageAt: p.id == "fx-comms" ? now - 60_000 : (previews[p.id]?.1 ?? now))
            allChats[p.id] = [chat]
            if let (text, at) = previews[p.id] {
                allMessages[chat.id] = [assistant("\(p.id)-m1", chat.id, text, at: at)]
            }
            caughtUp.insert(chat.id)
        }
        let c = "fx-comms-main"
        let t0 = now - 14 * 60_000
        allMessages[c] = [
            user("c1", c, "That was really great what we set up with Alan. However, I think we need to be a little more careful about the way in which we type in Discord, not to write massive, verbose responses. Try to write in my style do things that make it seem like it's not an agent that's writing the response. so maybe you can rewrite messages that go through our Discord.", at: t0),
            assistant("c2", c, "Makes sense. I'll pull a sample of your own Discord messages to get your style, then set it so everything going through Discord gets rewritten to sound like you first.", at: t0 + 20_000),
            handoff("c3", c, name: "Affine", at: t0 + 40_000),
            assistant("c4", c, "From now on, anything I send on your Discord gets rewritten to sound like you first. That means short, casual one-liners like \"Can be done in a day\" or \"nice work with 10\", sometimes split into two or three quick messages. It also means no lists, no long blocks, and no \"— Arbos\" sign-off. I've told Affine to keep its answers to the key points too, and I'll stick to this style for anything else I send on your Discord.", at: t0 + 90_000),
            user("c5", c, "Hey", at: t0 + 120_000),
            streaming("c6", c, at: t0 + 125_000),
        ]
        // Affine's chat shows the card types: file, voice memo, question, stop.
        let a = "fx-affine-main"
        let a0 = now - 19 * 60_000
        allMessages[a] = [
            user("a1", a, "Pull the board deck numbers from Salesforce, Mercury and QuickBooks and send me the deck.", at: a0 - 15 * 60_000),
            assistant("a2", a, "Updated the numbers based on today's pull from Salesforce, Mercury, QuickBooks, and Hex.", at: a0 - 14 * 60_000),
            attachment("a3", a, name: "Q3 board deck v4.pdf", mime: "application/pdf", at: a0 - 13 * 60_000),
            user("a4", a, "Can you add a slide for headcount and pull from Rippling", at: a0 - 10 * 60_000),
            assistant("a5", a, "Pulled and added.", at: a0 - 9 * 60_000),
            attachment("a6", a, name: "Q3 board deck v5.pdf", mime: "application/pdf", at: a0 - 9 * 60_000 + 5_000),
            attachment("a7", a, name: "Walkthrough.m4a", mime: "audio/m4a", at: a0 - 8 * 60_000),
            question("a8", a, "Should I send the deck to the whole team or just the board list?", at: a0 - 2 * 60_000),
            stopped("a9", a, at: a0),
        ]
        chats = allChats
        messages = allMessages
        machine = Machine(state: .awake, since: now - 3_600_000, spec: "Daytona · 4 vCPU · 8 GiB · 10 GiB disk · arbos-agent-0.2.0",
                          provider: "daytona", sandboxId: "fixture-box", spendTodayUsd: 1.6, spendCapUsd: 25,
                          workspaces: Dictionary(uniqueKeysWithValues: list.map { ($0.id, $0.id == "fx-comms" ? WorkspaceState.active : .idle) }),
                          alwaysOn: DebugHooks.alwaysOn, machineTodayUsd: 2.88, reported: true)
        // One DM with three unread messages, for the count pill and the notification screens.
        let meMember = ChatMember(id: "fx-me", kind: .user, member: "user:fx-me", role: "owner", handle: "you", avatarUrl: nil, name: "You", status: "member",
                                  joinedAt: now - 40 * 60_000, project: nil, ownerId: nil, ownerHandle: nil, replyPolicy: nil)
        let unconst = ChatMember(id: "gh_32490803", kind: .user, member: "user:gh_32490803", role: "member", handle: "unconst",
                                 avatarUrl: "https://avatars.githubusercontent.com/u/32490803?v=4", name: "Unconst", status: "member",
                                 joinedAt: now - 40 * 60_000, project: nil, ownerId: nil, ownerHandle: nil, replyPolicy: nil,
                                 lastSeenAt: now - 7 * 60_000, online: false)
        // A small group too, for the group page.
        let mira = ChatMember(id: "fx-u3", kind: .user, member: "user:fx-u3", role: "member", handle: "mira-k", avatarUrl: nil, name: "Mira Kaplan", status: "member",
                              joinedAt: now - 3 * 86_400_000, project: nil, ownerId: nil, ownerHandle: nil, replyPolicy: nil, lastSeenAt: now - 30_000, online: true)
        let sam = ChatMember(id: "fx-u4", kind: .user, member: "user:fx-u4", role: "member", handle: "sam", avatarUrl: nil, name: "Sam Okafor", status: "member",
                             joinedAt: now - 3 * 86_400_000, project: nil, ownerId: nil, ownerHandle: nil, replyPolicy: nil, lastSeenAt: now - 2 * 86_400_000, online: false)
        var meOwner = meMember; meOwner.role = "owner"
        let commsAgent = ChatMember(id: "fx-comms", kind: .agent, member: "agent:fx-comms", role: "agent", handle: "comms", avatarUrl: nil, name: "Comms", status: "member",
                                    joinedAt: now - 86_400_000, project: list.first { $0.id == "fx-comms" }, ownerId: "fx-me", ownerHandle: "you", replyPolicy: "mention")
        let group = PeopleChat(id: "fx-group-launch", kind: "group", createdBy: "fx-me", createdAt: now - 3 * 86_400_000, title: "Launch week",
                               pending: false, members: [meOwner, unconst, mira, sam, commsAgent], headSeq: 40, readSeq: 40, unread: false,
                               lockState: "visible_to_agents", lastActivityAt: now - 50 * 60_000, myRole: "owner", muted: false)
        peopleChats = [group, PeopleChat(id: "fx-dm-unconst", kind: "dm", createdBy: "gh_32490803", createdAt: now - 40 * 60_000, title: "@unconst",
                                  pending: false, members: [unconst, meMember], headSeq: 12, readSeq: 9, unread: true,
                                  lockState: "people_only", lastActivityAt: now - 7 * 60_000, myRole: "member", muted: false)]
        peopleUnread = ["fx-dm-unconst"]
        messages["fx-group-launch"] = [
            Message(id: "g1", chatId: "fx-group-launch", role: .user, sender: "user:fx-u3", status: .done, turn: nil, decision: nil, subagentId: nil,
                    subagentName: nil, inReplyTo: nil, usage: [:], model: nil, createdAt: now - 3 * 3_600_000, completedAt: nil, pending: false,
                    parts: [.text(id: "g1t", text: "Launch checklist is here: https://notion.so/arbos/launch-week")], hiddenCount: 0, senderHandle: "mira-k", senderName: "Mira Kaplan"),
            Message(id: "g2", chatId: "fx-group-launch", role: .user, sender: "user:fx-u4", status: .done, turn: nil, decision: nil, subagentId: nil,
                    subagentName: nil, inReplyTo: nil, usage: [:], model: nil, createdAt: now - 2 * 3_600_000, completedAt: nil, pending: false,
                    parts: [.attachment(id: "g2a", attachment: Attachment(name: "press-kit-v3.pdf", mime: "application/pdf", blobRef: nil))], hiddenCount: 0, senderHandle: "sam", senderName: "Sam Okafor"),
            Message(id: "g3", chatId: "fx-group-launch", role: .user, sender: "user:fx-u3", status: .done, turn: nil, decision: nil, subagentId: nil,
                    subagentName: nil, inReplyTo: nil, usage: [:], model: nil, createdAt: now - 50 * 60_000, completedAt: nil, pending: false,
                    parts: [.text(id: "g3t", text: "And the signup form: https://arbos.life/i/launch")], hiddenCount: 0, senderHandle: "mira-k", senderName: "Mira Kaplan"),
        ]
        caughtUp.insert("fx-group-launch")
        directory = [
            Person(id: "fx-u1", handle: "alex-rivera", avatarUrl: nil, name: "Alex Rivera", status: "member"),
            Person(id: "fx-u2", handle: "jacob", avatarUrl: nil, name: "Jacob Steeves", status: "member"),
            Person(id: "fx-u3", handle: "mira-k", avatarUrl: nil, name: "Mira Kaplan", status: "member"),
            Person(id: "fx-u4", handle: "sam", avatarUrl: nil, name: "Sam Okafor", status: "invited"),
            Person(id: "fx-u5", handle: "tobias", avatarUrl: nil, name: "Tobias Lund", status: "member"),
        ]
        link = .live
    }

    /// Two sample message notifications (an agent, a person) for screenshots.
    func postSampleNotifications() {
        let affine = projects.first { $0.id == "fx-affine" }
        let agentInfo = MessagePush.userInfo(
            chatId: "fx-affine-main", projectId: "fx-affine", title: "Affine",
            body: "Shortlist of 6 candidates is ready — two look very strong.",
            senderKind: "agent", senderId: "fx-affine", senderName: "Affine", senderHandle: "affine",
            avatar: affine.flatMap { avatarSlug(for: $0) } ?? "oak", avatarColor: affine?.avatarColor ?? "green", avatarUrl: nil)
        let personInfo = MessagePush.userInfo(
            chatId: "fx-dm-unconst", projectId: peopleSlot, title: "Unconst",
            body: "@main who is in the standup tomorrow?",
            senderKind: "user", senderId: "gh_32490803", senderName: "Unconst", senderHandle: "unconst",
            avatar: nil, avatarColor: nil, avatarUrl: "https://avatars.githubusercontent.com/u/32490803?v=4")
        Task {
            await PushManager.shared.postMessage(id: "sample-agent", info: agentInfo)
            try? await Task.sleep(nanoseconds: 2_500_000_000)
            await PushManager.shared.postMessage(id: "sample-person", info: personInfo)
            PushManager.shared.setBadge(3)
        }
    }

    /// Sends in fixture mode append locally so the composer can be exercised,
    /// then a scripted reply streams in word by word.
    func fixtureAppend(chatId: Id, text: String, attachments: [Attachment]) {
        var list = messages[chatId] ?? []
        // Finish the fixture's stuck "working" turn so the demo reads cleanly.
        if let i = list.lastIndex(where: { $0.isStreaming }) {
            list[i].status = .done
            list[i].parts = [.text(id: "\(list[i].id)-t", text: "Got it.")]
        }
        var m = user(newId(), chatId, text, at: nowMs())
        m.parts = attachments.map { .attachment(id: newId(), attachment: $0) } + (text.isEmpty ? [] : m.parts)
        list.append(m)
        messages[chatId] = list
        Task { await fixtureReply(chatId: chatId) }
    }

    /// Grok Bot reply flow: an ack bubble at once, a working line that names
    /// the current activity, a milestone bubble, then the result bubble.
    private func fixtureReply(chatId: Id) async {
        func append(_ m: Message) { messages[chatId, default: []].append(m) }
        func replace(_ m: Message) { if let i = messages[chatId]?.firstIndex(where: { $0.id == m.id }) { messages[chatId]?[i] = m } }
        func bubble(_ text: String, done: Bool = true) -> Message {
            let id = newId()
            return Message(id: id, chatId: chatId, role: .assistant, sender: "agent", status: done ? .done : .streaming, turn: .reply, decision: nil,
                           subagentId: nil, subagentName: nil, inReplyTo: nil, usage: [:], model: nil, createdAt: nowMs(),
                           completedAt: done ? nowMs() : nil, pending: false, parts: [.text(id: "\(id)-t", text: text)], hiddenCount: 0)
        }
        let request = messages[chatId]?.last(where: { $0.role == .user })?.plainText.lowercased() ?? ""
        let clone = request.contains("pull") || request.contains("clone") || request.contains("subtensor")
        try? await Task.sleep(nanoseconds: 900_000_000)
        // 1. Ack.
        append(bubble(clone ? "On it — pulling RaoFoundation/subtensor into your project folder." : "On it."))
        // 2. Working, with the activity the harness reports.
        let workId = newId()
        var work = Message(id: workId, chatId: chatId, role: .assistant, sender: "agent", status: .streaming, turn: .act, decision: nil,
                           subagentId: nil, subagentName: nil, inReplyTo: nil, usage: [:], model: nil, createdAt: nowMs(),
                           completedAt: nil, pending: false, parts: [.status(id: "\(workId)-s", code: .working, message: "Working")], hiddenCount: 0)
        append(work)
        let steps = clone
            ? ["Cloning RaoFoundation/subtensor", "Checking out main", "Reading the latest commit"]
            : ["Reading your Discord style", "Drafting the rewrite rule"]
        for step in steps {
            try? await Task.sleep(nanoseconds: 1_600_000_000)
            work.activity = step
            replace(work)
        }
        try? await Task.sleep(nanoseconds: 1_400_000_000)
        // 3. Milestone bubble: the working row becomes a short progress note.
        work.activity = nil
        work.status = .done
        work.completedAt = nowMs()
        work.parts = [.text(id: "\(workId)-t", text: clone ? "Cloned — shallow, `main`, 1 commit. Reading it now." : "Got your style from 40 recent messages.")]
        replace(work)
        // 4. Result, streamed.
        let resultText = clone
            ? "The RaoFoundation/subtensor repo is in your project folder at `subtensor/`, on `main`. Latest commit: \"etao: per-subnet emission weights (spec 480)\". Shallow clone to keep it quick — say the word if you need the full history."
            : "Done. Short replies from now on, and I'll keep anything that goes out on Discord to one or two casual lines in your voice."
        var result = bubble("", done: false)
        result.parts = [.status(id: "\(result.id)-s", code: .working, message: "Working")]
        append(result)
        try? await Task.sleep(nanoseconds: 700_000_000)
        var text = ""
        for w in resultText.split(separator: " ") {
            text += (text.isEmpty ? "" : " ") + w
            result.parts = [.text(id: "\(result.id)-t", text: text)]
            replace(result)
            try? await Task.sleep(nanoseconds: 60_000_000)
        }
        result.status = .done
        result.completedAt = nowMs()
        replace(result)
    }

    private func user(_ id: Id, _ chat: Id, _ text: String, at: Int64) -> Message {
        Message(id: id, chatId: chat, role: .user, sender: "user", status: .done, turn: nil, decision: nil, subagentId: nil,
                subagentName: nil, inReplyTo: nil, usage: [:], model: nil, createdAt: at, completedAt: at, pending: false,
                parts: [.text(id: "\(id)-t", text: text)], hiddenCount: 0)
    }

    private func assistant(_ id: Id, _ chat: Id, _ text: String, at: Int64) -> Message {
        Message(id: id, chatId: chat, role: .assistant, sender: "agent", status: .done, turn: .reply, decision: "reply", subagentId: nil,
                subagentName: nil, inReplyTo: nil, usage: [:], model: "anthropic/claude-haiku-5.5", createdAt: at, completedAt: at + 2_000,
                pending: false, parts: [.text(id: "\(id)-t", text: text)], hiddenCount: 0)
    }

    private func handoff(_ id: Id, _ chat: Id, name: String, at: Int64) -> Message {
        Message(id: id, chatId: chat, role: .assistant, sender: "agent", status: .done, turn: .spawn, decision: "new", subagentId: "sub-1",
                subagentName: name, inReplyTo: nil, usage: [:], model: nil, createdAt: at, completedAt: at + 5_000, pending: false,
                parts: [.status(id: "\(id)-s", code: .handoff, message: "Handed to subagent \(name)")], hiddenCount: 2)
    }

    private func attachment(_ id: Id, _ chat: Id, name: String, mime: String, at: Int64) -> Message {
        Message(id: id, chatId: chat, role: .assistant, sender: "agent", status: .done, turn: .reply, decision: "reply", subagentId: nil,
                subagentName: nil, inReplyTo: nil, usage: [:], model: nil, createdAt: at, completedAt: at, pending: false,
                parts: [.attachment(id: "\(id)-a", attachment: Attachment(name: name, mime: mime, blobRef: "fixture-\(id)"))], hiddenCount: 0)
    }

    private func question(_ id: Id, _ chat: Id, _ text: String, at: Int64) -> Message {
        Message(id: id, chatId: chat, role: .assistant, sender: "agent", status: .done, turn: .reply, decision: "reply", subagentId: nil,
                subagentName: nil, inReplyTo: nil, usage: [:], model: nil, createdAt: at, completedAt: at, pending: false,
                parts: [.status(id: "\(id)-q", code: .askUser, message: text)], hiddenCount: 0)
    }

    private func stopped(_ id: Id, _ chat: Id, at: Int64) -> Message {
        Message(id: id, chatId: chat, role: .assistant, sender: "agent", status: .cancelled, turn: .reply, decision: nil, subagentId: nil,
                subagentName: nil, inReplyTo: nil, usage: [:], model: nil, createdAt: at, completedAt: at, pending: false,
                parts: [.status(id: "\(id)-s", code: .stopped, message: "Stopped by the user")], hiddenCount: 0)
    }

    private func streaming(_ id: Id, _ chat: Id, at: Int64) -> Message {
        Message(id: id, chatId: chat, role: .assistant, sender: "agent", status: .streaming, turn: .reply, decision: nil, subagentId: nil,
                subagentName: nil, inReplyTo: nil, usage: [:], model: nil, createdAt: at, completedAt: nil, pending: false,
                parts: [.status(id: "\(id)-s", code: .working, message: "Working")], hiddenCount: 0)
    }
}
#endif
