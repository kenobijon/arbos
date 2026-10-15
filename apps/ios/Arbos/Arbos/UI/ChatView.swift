// One conversation, Grok Bot layout: floating glass header (back circle,
// avatar + name pill, "…" circle), messages scrolling underneath with a top
// fade, "<Project> is working" while a turn runs, and the composer ("+",
// "Ask <Project>" field with mic, white voice button) floating above the
// keyboard.

import SwiftUI

struct ChatView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    let chatId: Id
    let projectId: Id

    @State private var draft = ""
    @State private var rows: [Row] = []
    @State private var showModelPicker = false
    @State private var showThreads = false
    @State private var showVoice = false
    @State private var showSettings = false
    @State private var sentOnce = false
    @FocusState private var composerFocused: Bool

    private var project: Project? { model.project(projectId) }
    private var chat: Chat? { model.chat(chatId) }
    private var messages: [Message] { model.messages(of: chatId) }
    private var needsAnswer: Bool { model.chatNeedsAnswer(chatId) }
    private var streaming: Bool { model.isStreaming(chatId) }
    private var name: String { project?.name ?? "Arbos" }

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .top) {
                theme.bg.ignoresSafeArea()
                transcript(width: geo.size.width)
                headerBlur
                header
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            composer
                .padding(.horizontal, 18)
                .padding(.top, 8)
                .padding(.bottom, composerFocused ? 2 : 2)
                .background(alignment: .top) {
                    LinearGradient(colors: [theme.bg.opacity(0), theme.bg], startPoint: .top, endPoint: .bottom)
                        .frame(height: 24)
                        .offset(y: -24)
                }
                .background(theme.bg)
        }
        .toolbar(.hidden, for: .navigationBar)
        .navigationBarBackButtonHidden(true)
        .sheet(isPresented: $showModelPicker) {
            if let project {
                ModelPickerView(project: project).environmentObject(model).environment(\.theme, theme)
            }
        }
        .sheet(isPresented: $showThreads) {
            ChatListView(projectId: projectId).environmentObject(model).environment(\.theme, theme)
        }
        .sheet(isPresented: $showSettings) {
            SettingsView().environmentObject(model).environment(\.theme, theme)
        }
        .fullScreenCover(isPresented: $showVoice) {
            if let project {
                VoiceModeView(project: project).environmentObject(model).environment(\.theme, theme)
            }
        }
        .onAppear {
            model.markRead(chatId: chatId)
            if let show = DebugHooks.show {
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) {
                    switch show {
                    case "keyboard": composerFocused = true
                    case "settings": showSettings = true
                    case "voice": showVoice = true
                    case "threads": showThreads = true
                    case "model": showModelPicker = true
                    default: break
                    }
                }
            }
        }
        .onChange(of: messages, initial: true) { _, new in
            rows = Transcript.rows(new, projectId: projectId)
            model.markRead(chatId: chatId)
        }
        .onChange(of: model.caughtUp.contains(chatId), initial: true) { _, ready in
            if ready, !sentOnce, let text = DebugHooks.autoSend, DebugHooks.openChat == chatId {
                draft = text
                send()
            }
        }
    }

    // MARK: Header (floating glass)

    /// Frosted strip the messages scroll under (ref-02 shows blurred text
    /// behind the header); the mask fades it out so there is no hard edge.
    private var headerBlur: some View {
        ZStack {
            Rectangle().fill(.ultraThinMaterial)
            theme.bg.opacity(0.78)
        }
        .frame(height: 118)
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
                HStack(spacing: 8) {
                    if let project {
                        BlobAvatar(project: project, theme: theme, size: Theme.avatar, dot: machineDotColor(model.machine.state, theme: theme))
                    }
                    Text(name)
                        .font(.system(size: Theme.body, weight: .semibold))
                        .foregroundStyle(theme.text)
                        .lineLimit(1)
                }
                .padding(.leading, 10)
                .padding(.trailing, 16)
                .frame(height: Theme.control)
            }
            Spacer()
            Menu {
                Button { showThreads = true } label: { Label("Chats", systemImage: "bubble.left.and.bubble.right") }
                Button {
                    Task { if let c = await model.newChat(projectId: projectId) { model.open(chat: c) } }
                } label: { Label("New chat", systemImage: "square.and.pencil") }
                Button { showModelPicker = true } label: { Label("Model · \(project?.shortModel ?? "")", systemImage: "cpu") }
                if streaming {
                    Button(role: .destructive) { stop() } label: { Label("Stop \(name)", systemImage: "stop.circle") }
                }
                Divider()
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

    // MARK: Transcript

    private func transcript(width: CGFloat) -> some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    Color.clear.frame(height: 64)
                    if let banner = machineBannerText {
                        HStack(spacing: 6) {
                            if model.machine.state == .waking { ProgressView().controlSize(.mini).tint(theme.muted) }
                            Text(banner)
                        }
                        .font(.system(size: Theme.small))
                        .foregroundStyle(model.machine.state == .error ? theme.danger : theme.muted)
                        .frame(maxWidth: .infinity)
                        .padding(.bottom, 16)
                    }
                    if !model.caughtUp.contains(chatId) && messages.isEmpty && !model.isFixture {
                        HStack { Spacer(); ProgressView().tint(theme.muted); Spacer() }.padding(.top, 60)
                    } else if rows.isEmpty {
                        emptyHint
                    }
                    ForEach(Array(rows.enumerated()), id: \.element.id) { i, row in
                        RowView(row: row, chatId: chatId, width: width)
                            .padding(.top, gap(before: i))
                            .id(row.id)
                    }
                    if streaming {
                        workingLine.padding(.top, 20)
                    }
                    Color.clear.frame(height: 12).id("bottom")
                }
                .padding(.horizontal, 16)
            }
            .defaultScrollAnchor(.bottom)
            .scrollDismissesKeyboard(.interactively)
            .mask {
                VStack(spacing: 0) {
                    LinearGradient(colors: [.clear, .black], startPoint: .top, endPoint: .bottom).frame(height: 110)
                    Color.black
                }
                .ignoresSafeArea()
            }
            .onChange(of: rows.last?.id) { _, _ in
                if streaming || sentOnce {
                    withAnimation(.easeOut(duration: 0.2)) { proxy.scrollTo("bottom", anchor: .bottom) }
                }
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
        case .error: return model.machineDetail.map { "Machine: \($0)" } ?? "Could not wake your machine"
        case .awake, .hibernatedLight, .hibernatedDeep, .stoppedByUser: return model.link == .reconnecting ? "Reconnecting…" : nil
        }
    }

    private var emptyHint: some View {
        VStack(spacing: 10) {
            if let project {
                BlobAvatar(project: project, theme: theme, size: 64)
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

    private var workingLine: some View {
        HStack(spacing: 10) {
            if let project {
                BlobAvatar(project: project, theme: theme, size: 26)
            }
            Text("\(name) is working")
                .font(.system(size: Theme.small))
                .foregroundStyle(theme.muted)
            Spacer(minLength: 0)
        }
        .padding(.leading, 12)
        .accessibilityLabel("\(name) is working")
    }

    // MARK: Composer

    private var composer: some View {
        HStack(spacing: 12) {
            Menu {
                Button { } label: { Label("Photo or video", systemImage: "photo") }
                Button { } label: { Label("Take photo", systemImage: "camera") }
                Button { } label: { Label("File", systemImage: "doc") }
            } label: {
                ZStack {
                    Circle().fill(theme.field)
                    Circle().strokeBorder(theme.fieldBorder.opacity(0.6), lineWidth: 1)
                    Image(systemName: "plus")
                        .font(.system(size: 21, weight: .regular))
                        .foregroundStyle(theme.text)
                }
                .frame(width: Theme.plusButton, height: Theme.plusButton)
            }
            .buttonStyle(.plain)
            HStack(spacing: 8) {
                TextField("", text: $draft, prompt: Text(needsAnswer ? "Answer \(name)" : "Ask \(name)").foregroundStyle(theme.placeholder), axis: .vertical)
                    .lineLimit(1...8)
                    .font(.system(size: Theme.body))
                    .foregroundStyle(theme.text)
                    .tint(Color(hex: "#1084FE")!)
                    .focused($composerFocused)
                    .padding(.leading, 16)
                    .padding(.vertical, 10)
                if draft.isEmpty {
                    Button { composerFocused = true } label: {
                        Image(systemName: "mic")
                            .font(.system(size: 17))
                            .foregroundStyle(theme.icon)
                    }
                    .buttonStyle(.plain)
                    .padding(.trailing, 4)
                }
                // Grok keeps the composer usable while the bot works: text sends,
                // an empty field starts voice chat. Stop lives in the … menu.
                Button(action: canSend ? send : { showVoice = true }) {
                    Circle()
                        .fill(.white)
                        .frame(width: Theme.voiceButton, height: Theme.voiceButton)
                        .overlay(
                            Image(systemName: canSend ? "arrow.up" : "waveform")
                                .font(.system(size: 16, weight: .bold))
                                .foregroundStyle(.black)
                        )
                }
                .buttonStyle(.plain)
                .padding(.trailing, 3)
            }
            .frame(minHeight: Theme.fieldHeight)
            .background(
                RoundedRectangle(cornerRadius: Theme.fieldHeight / 2, style: .continuous).fill(theme.field)
            )
            .overlay(
                RoundedRectangle(cornerRadius: Theme.fieldHeight / 2, style: .continuous)
                    .strokeBorder(needsAnswer ? theme.warn.opacity(0.7) : theme.fieldBorder, lineWidth: 1)
            )
        }
    }

    private var canSend: Bool {
        !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func send() {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        if needsAnswer {
            model.answer(chatId: chatId, text: text)
        } else {
            model.send(chatId: chatId, text: text)
        }
        draft = ""
        sentOnce = true
    }

    private func stop() {
        model.stop(chatId: chatId)
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
            .searchable(text: $query, prompt: "Filter models")
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
