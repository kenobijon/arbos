// Debug-only fixture: the Grok Bot reference conversation mapped onto Arbos
// concepts, so the simulator can be screenshotted without a relay. Enabled by
// `ARBOS_FIXTURE=1` in the launch environment. Not compiled into Release.

import Foundation

#if DEBUG
extension AppModel {
    func loadFixture() {
        isFixture = true
        // The signed-in user (D79 initials at home and in Settings; reactions need `myUserId`).
        setFixtureUser(AuthUser(id: "fx-me", login: "jacob", name: "Jacob Steeves"))
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
        // Distinct palette colours (D79) so screenshots read well.
        let tints = ["fx-comms": 5, "fx-affine": 3, "fx-hyper": 1, "fx-expense": 6, "fx-invoices": 4, "fx-scout": 2]
        for i in list.indices { list[i].avatarColorIndexEffective = tints[list[i].id] }
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
        // An agent-hosted group chat (F6): Comms hosts, Affine and Hyper were added.
        let g = "fx-comms-group-launch"
        allChats["fx-comms", default: []].append(Chat(id: g, projectId: "fx-comms", title: "Launch copy", isPinned: false, unread: true,
                                                      lastMessageAt: now - 9 * 60_000, kind: "group", participants: ["fx-affine", "fx-hyper"]))
        allMessages[g] = [
            user("g0", g, "Affine, Hyper — align the launch copy with the Discord tone Comms set.", at: now - 30 * 60_000),
            // Two hand-offs in a row merge into one Grok line: "4 messages with ◉ Affine and ◉ Hyper".
            handoff("g0a", g, name: "Affine", at: now - 29 * 60_000),
            handoff("g0b", g, name: "Hyper", at: now - 28 * 60_000),
            assistant("g1", g, "Got it. Three variants in the shared doc, all short and casual.", at: now - 9 * 60_000),
        ]
        caughtUp.insert(g)
        let c = "fx-comms-main"
        let t0 = now - 14 * 60_000
        allMessages[c] = [
            user("c1", c, "That was really great what we set up with Alan. However, I think we need to be a little more careful about the way in which we type in Discord, not to write massive, verbose responses. Try to write in my style do things that make it seem like it's not an agent that's writing the response. so maybe you can rewrite messages that go through our Discord.", at: t0),
            assistant("c2", c, "Makes sense. I'll pull a sample of your own Discord messages to get your style, then set it so everything going through Discord gets rewritten to sound like you first.", at: t0 + 20_000),
            handoff("c3", c, name: "Affine", at: t0 + 40_000, messages: 1),
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
            question("a8", a, "Send the deck to the whole team or just the board list?",
                     options: ["Whole team", "Board list only", "Hold off for now"], at: a0 - 2 * 60_000),
            stopped("a9", a, at: a0),
        ]
        chats = allChats
        messages = allMessages
        machine = Machine(state: .awake, since: now - 3_600_000, spec: "Daytona · 4 vCPU · 8 GiB · 10 GiB disk · arbos-agent-0.2.0",
                          provider: "daytona", sandboxId: "fixture-box", spendTodayUsd: 1.6, spendCapUsd: 25,
                          workspaces: Dictionary(uniqueKeysWithValues: list.map { ($0.id, $0.id == "fx-comms" ? WorkspaceState.active : .idle) }),
                          alwaysOn: DebugHooks.alwaysOn, machineTodayUsd: 2.88, reported: true)
        // One DM with three unread messages, for the count pill and the notification screens.
        let meMember = ChatMember(id: "fx-me", kind: .user, member: "user:fx-me", role: "owner", handle: "jacob", avatarUrl: nil, name: "Jacob Steeves", status: "member",
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
                                    joinedAt: now - 86_400_000, project: list.first { $0.id == "fx-comms" }, ownerId: "fx-me", ownerHandle: "jacob", replyPolicy: "mention")
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
            Person(id: "fx-u2", handle: "jen", avatarUrl: nil, name: "Jen Park", status: "member"),
            Person(id: "fx-u3", handle: "mira-k", avatarUrl: nil, name: "Mira Kaplan", status: "member"),
            Person(id: "fx-u4", handle: "sam", avatarUrl: nil, name: "Sam Okafor", status: "invited"),
            Person(id: "fx-u5", handle: "tobias", avatarUrl: nil, name: "Tobias Lund", status: "member"),
        ]
        link = .live
        seedFixtureSecrets(now: now)
    }

    /// Settings → Secrets demo (D83), the Mac fixture's twin: a Stripe key for Affine and
    /// Comms, a Mailgun key for Comms, a GitHub login for Hyper, one 1Password vault, recent
    /// activity. No value exists anywhere in the fixture.
    private func seedFixtureSecrets(now: Int64) {
        func agent(_ id: String, _ name: String, _ enabled: Bool, _ note: String) -> [String: Any] {
            var a: [String: Any] = ["project_id": id, "name": name, "access_id": "fx-acc-\(id)-\(note.count)", "enabled": enabled, "note": note, "hosts": []]
            if enabled { a["last_used_at"] = now - 120_000 } else { a["disabled_at"] = now - 6 * 86_400_000 }
            return a
        }
        func secret(_ id: String, _ name: String, _ env: String, _ kind: String, _ note: String, _ hosts: [String], _ agents: [[String: Any]], _ daysAgo: Int64) -> [String: Any] {
            var m: [String: Any] = ["id": id, "name": name, "env": env, "kind": kind, "note": note, "hosts": hosts, "ask_every_time": false,
                                    "version": 1, "created_at": now - daysAgo * 86_400_000, "vault_name": "Arbos", "agents": agents]
            if daysAgo > 3 { m["last_used_at"] = now - 120_000 }
            if kind == "login" { m["username"] = "unarbos" }
            return m
        }
        func act(_ id: Int, _ action: String, _ name: String, _ project: (String, String)?, _ host: String?, _ by: String, _ minsAgo: Int64) -> [String: Any] {
            var m: [String: Any] = ["id": id, "at": now - minsAgo * 60_000, "action": action, "name": name, "by": by, "detail": [:]]
            if let p = project { m["project_id"] = p.0; m["project_name"] = p.1 }
            if let host { m["host"] = host }
            return m
        }
        let stripeAgents = [agent("fx-affine", "Affine", true, "pull refund reports"),
                            agent("fx-comms", "Comms", true, "monthly revenue line in the newsletter"),
                            agent("fx-hyper", "Hyper", false, "")]
        var stripe = secret("fx-sec-stripe", "STRIPE_KEY", "STRIPE_KEY", "key", "Stripe live key — refunds and reports. Never for payouts.", ["api.stripe.com"], stripeAgents, 12)
        stripe["inject_env"] = true
        let mailgun = secret("fx-sec-mailgun", "MAILGUN_API_KEY", "MAILGUN_API_KEY", "key", "Newsletter sends.", ["api.mailgun.net"], [agent("fx-comms", "Comms", true, "send the newsletter")], 5)
        let github = secret("fx-sec-github", "GitHub login", "GITHUB_LOGIN", "login", "The unarbos account, for Actions and reviews.", ["github.com", "*.github.com"], [agent("fx-hyper", "Hyper", true, "review failing Actions runs")], 1)
        let vault: [String: Any] = ["id": "fx-vault-1p", "provider": "onepassword", "name": "Arbos — Research", "items_count": 12, "status": "ok",
                                    "env_items": 1, "limits": ["reads_per_hour": 1000, "reads_used_hour": 3],
                                    "agents": [agent("fx-affine", "Affine", true, "read the Stripe and Postgres items")]]
        let activity = [act(1, "used", "STRIPE_KEY", ("fx-affine", "Affine"), "api.stripe.com", "agent:affine", 2),
                        act(2, "access_on", "MAILGUN_API_KEY", ("fx-comms", "Comms"), nil, "user:me", 1_440),
                        act(3, "refused", "STRIPE_KEY", ("fx-hyper", "Hyper"), "stripe.evil.example", "agent:hyper", 4_320)]
        // One pending card: shown on the Secrets root and on Comms's page (same rule on the Mac).
        let request: [String: Any] = ["id": "fx-req-1", "project_id": "fx-comms", "project_name": "Comms", "name": "GITHUB_LOGIN",
                                      "purpose": "Sign in to post the release notes", "use": "browser_fill", "host": "github.com",
                                      "status": "pending", "created_at": now - 180_000]
        fixtureVaultItems = [
            ["ref": "op://Arbos — Research/Stripe", "title": "Stripe", "category": "API_CREDENTIAL", "fields": ["credential", "username"], "env": "STRIPE", "inject_env": true],
            ["ref": "op://Arbos — Research/Postgres", "title": "Postgres", "category": "DATABASE", "fields": ["password", "hostname", "port", "database"], "env": "POSTGRES", "inject_env": false],
        ]
        secrets = SecretsOverview.parse(["paused": false, "vaults": [vault], "secrets": [stripe, mailgun, github],
                                         "requests_pending": 1, "requests": [request], "activity": activity, "providers": ["onepassword"]])
        var stripeAccess = stripeAgents[1]
        stripeAccess["secret"] = stripe
        var vaultAccess = agent("fx-comms", "Comms", true, "read the Stripe item")
        vaultAccess["vault"] = vault
        vaultAccess["target_kind"] = "vault"
        projectSecrets["fx-comms"] = ProjectSecrets.parse(["paused": false, "owner": true, "items": [stripeAccess, vaultAccess],
                                                            "requests": [request], "activity": activity])
    }

    /// Two sample message notifications (an agent, a person) for screenshots.
    func postSampleNotifications() {
        let affine = projects.first { $0.id == "fx-affine" }
        let agentInfo = MessagePush.userInfo(
            chatId: "fx-affine-main", projectId: "fx-affine", title: "Affine",
            body: "Shortlist of 6 candidates is ready — two look very strong.",
            senderKind: "agent", senderId: "fx-affine", senderName: "Affine", senderHandle: "affine",
            avatar: nil, avatarColor: nil, avatarUrl: nil)
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

    /// Grok Bot reply flow for a cloning exercise: one short ack, a working
    /// line, then a conversational result — no thoughts, no milestone boilerplate.
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
        // 1. Ack — one short present-tense sentence, nothing else.
        append(bubble(clone ? "Cloning it now." : "On it."))
        // 2. Working line only (no milestone bubble for a quick clone).
        let workId = newId()
        var work = Message(id: workId, chatId: chatId, role: .assistant, sender: "agent", status: .streaming, turn: .act, decision: nil,
                           subagentId: nil, subagentName: nil, inReplyTo: nil, usage: [:], model: nil, createdAt: nowMs(),
                           completedAt: nil, pending: false, parts: [.status(id: "\(workId)-s", code: .working, message: "Working")], hiddenCount: 0)
        append(work)
        let steps = clone
            ? ["Cloning RaoFoundation/subtensor"]
            : ["Reading your Discord style", "Drafting the rewrite rule"]
        for step in steps {
            try? await Task.sleep(nanoseconds: clone ? 2_200_000_000 : 1_600_000_000)
            work.activity = step
            replace(work)
        }
        try? await Task.sleep(nanoseconds: clone ? 800_000_000 : 1_400_000_000)
        if clone {
            // Quick clone: drop the working row; result is the only follow-up.
            if let i = messages[chatId]?.firstIndex(where: { $0.id == workId }) {
                messages[chatId]?.remove(at: i)
            }
        } else {
            // Longer act: working row becomes a short progress note.
            work.activity = nil
            work.status = .done
            work.completedAt = nowMs()
            work.parts = [.text(id: "\(workId)-t", text: "Got your style from 40 recent messages.")]
            replace(work)
        }
        // 3. Result, streamed — clone stays short and conversational.
        let resultText = clone
            ? "Got `subtensor/` on main. What do you want to do with it?"
            : "Done. Short replies from now on, and I'll keep anything that goes out on Discord to one or two casual lines in your voice."
        var result = bubble("", done: false)
        result.parts = [.status(id: "\(result.id)-s", code: .working, message: "Working")]
        append(result)
        try? await Task.sleep(nanoseconds: clone ? 500_000_000 : 700_000_000)
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

    private func handoff(_ id: Id, _ chat: Id, name: String, at: Int64, messages: Int = 2) -> Message {
        Message(id: id, chatId: chat, role: .assistant, sender: "agent", status: .done, turn: .spawn, decision: "new", subagentId: "sub-1",
                subagentName: name, handoffMessages: messages, inReplyTo: nil, usage: [:], model: nil, createdAt: at, completedAt: at + 5_000, pending: false,
                parts: [.status(id: "\(id)-s", code: .handoff, message: "Handed to subagent \(name)")], hiddenCount: 2)
    }

    private func attachment(_ id: Id, _ chat: Id, name: String, mime: String, at: Int64) -> Message {
        Message(id: id, chatId: chat, role: .assistant, sender: "agent", status: .done, turn: .reply, decision: "reply", subagentId: nil,
                subagentName: nil, inReplyTo: nil, usage: [:], model: nil, createdAt: at, completedAt: at, pending: false,
                parts: [.attachment(id: "\(id)-a", attachment: Attachment(name: name, mime: mime, blobRef: "fixture-\(id)"))], hiddenCount: 0)
    }

    /// As agentd writes it: a `question` part with a choice widget plus the legacy
    /// `waiting_user` status line (the card wins, the line is hidden).
    private func question(_ id: Id, _ chat: Id, _ text: String, options: [String], at: Int64) -> Message {
        let opts = options.enumerated().map { ChoiceOption(id: "opt-\($0.offset)", label: $0.element) }
        let q = Question(questionId: "\(id)-question", question: text, options: opts, multi: false, widgetType: "choice", answer: [], open: true)
        return Message(id: id, chatId: chat, role: .assistant, sender: "agent", status: .done, turn: .reply, decision: "reply", subagentId: nil,
                       subagentName: nil, inReplyTo: nil, usage: [:], model: nil, createdAt: at, completedAt: at, pending: false,
                       parts: [.question(id: "\(id)-w", question: q), .status(id: "\(id)-q", code: .askUser, message: text)], hiddenCount: 0)
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
