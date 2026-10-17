// People-chat sheets (D72): New Message (Telegram-style: search, New agent,
// New group, your agents, every Arbos user), members (people, add-person, agents, add-agent picker, block, leave/delete) and
// project members (invite by handle).

import SwiftUI

/// "+" on home — Telegram's "New Message" sheet in Grok's dark skin: rises
/// from the bottom, search on top, then New agent / New group, then your
/// agents, then every other Arbos user. Typing filters agents and the
/// directory and also asks `GET /users/search`, so a GitHub handle not on
/// Arbos yet can be invited.
struct NewChatSheet: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var remote: [Person] = []
    @State private var searching = false
    @State private var opening = false
    @State private var loadingDirectory = false
    @FocusState private var focused: Bool

    private var trimmed: String {
        query.trimmingCharacters(in: .whitespaces).trimmingCharacters(in: CharacterSet(charactersIn: "@"))
    }

    /// Own + shared agents (home list order by name). Empty query hides archived;
    /// a search finds them again, same as the home list.
    private var agents: [Project] {
        let q = trimmed.lowercased()
        let base = model.projects.filter { q.isEmpty ? !model.isArchived($0.id) : true }
        let filtered = q.isEmpty ? base : base.filter {
            $0.name.lowercased().contains(q) || mentionSlug($0.name).contains(q)
        }
        return filtered.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    /// Directory hits first (alphabetical), then remote hits not already listed.
    private var people: [Person] {
        let q = trimmed.lowercased()
        let local = q.isEmpty ? model.directory : model.directory.filter {
            $0.handle.lowercased().contains(q) || ($0.name ?? "").lowercased().contains(q)
        }
        var seen = Set(local.map(\.id))
        var out = local
        for p in remote where !seen.contains(p.id) && p.id != model.user?.id {
            seen.insert(p.id)
            out.append(p)
        }
        return out
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    searchField
                        .padding(.horizontal, 16)
                        .padding(.top, 6)
                        .padding(.bottom, 14)

                    if trimmed.isEmpty {
                        card {
                            NavigationLink { NewAgentView(onDone: { dismiss() }) } label: {
                                actionRow(icon: "sparkles", title: "New agent", subtitle: "Its own workspace on your machine")
                            }
                            .buttonStyle(.plain)
                            rowDivider
                            NavigationLink { NewGroupView(onDone: { dismiss() }) } label: {
                                actionRow(icon: "person.2.fill", title: "New group", subtitle: "People and agents in one chat")
                            }
                            .buttonStyle(.plain)
                        }
                        .padding(.horizontal, 16)
                        .padding(.bottom, 22)
                    }

                    if !agents.isEmpty {
                        sectionHeader(trimmed.isEmpty ? "Your agents" : "Agents")
                        card {
                            ForEach(Array(agents.enumerated()), id: \.element.id) { i, p in
                                Button { openAgent(p) } label: { AgentPickRow(project: p) }
                                    .buttonStyle(.plain)
                                if i < agents.count - 1 { rowDivider }
                            }
                        }
                        .padding(.horizontal, 16)
                        .padding(.bottom, 22)
                    }

                    // Keep the people block when the directory is empty (invite copy),
                    // but skip a redundant "no match" under Agents when a search hit only agents.
                    if !people.isEmpty || agents.isEmpty || trimmed.isEmpty {
                        sectionHeader(trimmed.isEmpty ? "Arbos users" : "People")
                        if people.isEmpty {
                            peopleEmpty
                        } else {
                            card {
                                ForEach(Array(people.enumerated()), id: \.element.id) { i, p in
                                    Button { open(p) } label: { PersonRow(person: p) }
                                        .buttonStyle(.plain)
                                    if i < people.count - 1 { rowDivider }
                                }
                            }
                            .padding(.horizontal, 16)
                        }
                    }
                    Color.clear.frame(height: 40)
                }
            }
            .scrollDismissesKeyboard(.interactively)
            .background(theme.bg)
            .navigationTitle("New Message")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) { Button("Cancel") { dismiss() }.foregroundStyle(theme.text) }
            }
            .toolbarBackground(theme.bg, for: .navigationBar)
            .toolbarBackground(.visible, for: .navigationBar)
            .task {
                loadingDirectory = model.directory.isEmpty
                await model.loadDirectory()
                loadingDirectory = false
            }
            .onChange(of: query) { _, _ in search() }
        }
        .tint(theme.text)
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
        .presentationBackground(theme.bg)
        .presentationCornerRadius(28)
        .preferredColorScheme(theme.isDark ? .dark : .light)
    }

    // MARK: Pieces

    private var searchField: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass").font(.system(size: 15, weight: .medium)).foregroundStyle(theme.muted)
            TextField("", text: $query, prompt: Text("Search people or agents").foregroundStyle(theme.placeholder))
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .font(.system(size: Theme.body))
                .foregroundStyle(theme.text)
                .focused($focused)
                .submitLabel(.go)
                .onSubmit {
                    if let first = agents.first { openAgent(first) }
                    else if let first = people.first { open(first) }
                    else if !trimmed.isEmpty { openHandle(trimmed) }
                }
            if searching {
                ProgressView().controlSize(.small).tint(theme.muted)
            } else if !query.isEmpty {
                Button { query = ""; remote = [] } label: {
                    Image(systemName: "xmark.circle.fill").foregroundStyle(theme.muted)
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.horizontal, 12)
        .frame(height: 38)
        .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(theme.field))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(theme.fieldBorder, lineWidth: 1))
    }

    private func sectionHeader(_ text: String) -> some View {
        Text(text.uppercased())
            .font(.system(size: 12, weight: .medium))
            .kerning(0.4)
            .foregroundStyle(theme.muted)
            .padding(.horizontal, 32)
            .padding(.bottom, 8)
    }

    private func card<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        VStack(spacing: 0) { content() }
            .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(theme.card))
    }

    private var rowDivider: some View {
        Rectangle().fill(theme.separator).frame(height: 0.5).padding(.leading, 72)
    }

    private func actionRow(icon: String, title: String, subtitle: String) -> some View {
        HStack(spacing: 14) {
            GlassCircle(size: 44) {
                Image(systemName: icon).font(.system(size: 17, weight: .semibold)).foregroundStyle(theme.text)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.system(size: Theme.body, weight: .medium)).foregroundStyle(theme.text)
                Text(subtitle).font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
            }
            Spacer()
            Image(systemName: "chevron.right").font(.system(size: 13, weight: .semibold)).foregroundStyle(theme.muted.opacity(0.7))
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .contentShape(Rectangle())
    }

    private var peopleEmpty: some View {
        Group {
            if loadingDirectory || searching {
                HStack { Spacer(); ProgressView().tint(theme.muted); Spacer() }.padding(.top, 28)
            } else if trimmed.isEmpty {
                Text(model.credentials == nil
                     ? "Sign in to see who else is on Arbos."
                     : "No one else is on Arbos yet. Search a GitHub username to invite them; the chat opens for both of you when they sign in.")
                    .font(.system(size: Theme.small)).foregroundStyle(theme.muted)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: .infinity)
                    .padding(.horizontal, 36).padding(.top, 20)
            } else {
                VStack(spacing: 10) {
                    Text("No one on Arbos or GitHub matches @\(trimmed).")
                        .font(.system(size: Theme.small)).foregroundStyle(theme.muted)
                    if trimmed.count >= 2 {
                        Button { openHandle(trimmed) } label: {
                            Text("Invite @\(trimmed)")
                                .font(.system(size: Theme.small, weight: .semibold)).foregroundStyle(theme.onPrimary)
                                .padding(.horizontal, 18).frame(height: 38)
                                .background(Capsule().fill(theme.primary))
                        }
                        .buttonStyle(.plain)
                    }
                }
                .frame(maxWidth: .infinity)
                .padding(.top, 20)
            }
        }
    }

    // MARK: Actions

    private func search() {
        let q = trimmed
        guard q.count >= 2 else { remote = []; searching = false; return }
        searching = true
        Task {
            try? await Task.sleep(nanoseconds: 250_000_000)
            guard q == trimmed else { return }
            remote = await model.searchPeople(q)
            if q == trimmed { searching = false }
        }
    }

    private func open(_ p: Person) { openHandle(p.handle) }

    private func openAgent(_ p: Project) {
        guard !opening else { return }
        opening = true
        model.open(project: p)
        dismiss()
        opening = false
    }

    private func openHandle(_ handle: String) {
        guard !opening else { return }
        opening = true
        Task {
            if await model.openDM(handle: handle) != nil { dismiss() }
            opening = false
        }
    }
}

/// One of my agents in the New Message / New group lists: tree avatar, name, @slug.
struct AgentPickRow: View {
    @Environment(\.theme) private var theme
    let project: Project
    var trailing: AnyView? = nil

    var body: some View {
        HStack(spacing: 14) {
            AgentAvatar(project: project, theme: theme, size: 44, still: true)
            VStack(alignment: .leading, spacing: 2) {
                Text(project.name).font(.system(size: Theme.body, weight: .medium)).foregroundStyle(theme.text).lineLimit(1)
                Text("@\(mentionSlug(project.name))").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted).lineLimit(1)
            }
            Spacer()
            if let trailing { trailing }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 9)
        .contentShape(Rectangle())
    }
}

/// One person: avatar, name, @handle and their Arbos status.
struct PersonRow: View {
    @Environment(\.theme) private var theme
    let person: Person
    var trailing: AnyView? = nil

    var body: some View {
        HStack(spacing: 14) {
            PersonAvatar(url: person.avatarUrl, handle: person.handle, size: 44)
            VStack(alignment: .leading, spacing: 2) {
                Text(person.displayName).font(.system(size: Theme.body, weight: .medium)).foregroundStyle(theme.text).lineLimit(1)
                Text("@\(person.handle)").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted).lineLimit(1)
            }
            Spacer()
            if let trailing {
                trailing
            } else {
                Text(PersonRow.statusLabel(person.status))
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(person.status == "member" ? theme.online : theme.muted)
                    .padding(.horizontal, 9).padding(.vertical, 4)
                    .background(Capsule().fill(theme.field))
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 9)
        .contentShape(Rectangle())
    }

    static func statusLabel(_ s: String) -> String {
        switch s {
        case "member": return "On Arbos"
        case "invited": return "Invited"
        default: return "Invite"
        }
    }
}

/// New agent: a name (and optional look), then `POST /projects`. The relay
/// sets up its workspace. Tap the preview to pick the tree and colour.
struct NewAgentView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    /// Closes the whole sheet (a pushed view's own dismiss only pops).
    let onDone: () -> Void
    @State private var name = ""
    /// Explicit picks from the look sheet; nil keeps the name-hash preview
    /// and lets the relay choose the final pair on create.
    @State private var pickedSlug: String? = nil
    @State private var pickedColorName: String? = nil
    @State private var showLook = false
    @FocusState private var focused: Bool

    private var clean: String { name.trimmingCharacters(in: .whitespaces) }

    private var previewSlug: String? {
        let trees = TreeAvatars.shared.characters
        guard !trees.isEmpty else { return nil }
        if let pickedSlug { return pickedSlug }
        return trees[NewAgentView.stableIndex(clean, trees.count)].slug
    }

    /// Tint for the preview. Explicit "mono" → nil (white tree); no pick →
    /// the coloured hash default (D74).
    private var previewColor: TreeColor? {
        if let name = pickedColorName {
            if name == "mono" { return nil }
            return TreeAvatars.shared.color(named: name)
        }
        let palette = TreeAvatars.shared.coloredPalette
        guard !palette.isEmpty else { return nil }
        return palette[NewAgentView.stableIndex(clean + ":color", palette.count)]
    }

    var body: some View {
        VStack(spacing: 18) {
            Button {
                focused = false
                showLook = true
            } label: {
                Group {
                    if let slug = previewSlug {
                        TreeCharacterView(slug: slug, size: 92, color: previewColor)
                            .id("\(slug)|\(previewColor?.name ?? "mono")")
                    } else {
                        let blob = pickedColorName.flatMap { Color(hex: $0) }
                            ?? Theme.palette[NewAgentView.stableIndex(clean, Theme.palette.count)]
                        BlobAvatar(color: blob, shape: BlobShape.pick(for: clean), size: 92)
                    }
                }
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Choose look")
            .padding(.top, 28)
            .animation(.snappy, value: "\(clean)|\(pickedSlug ?? "")|\(pickedColorName ?? "")")
            Text(clean.isEmpty ? "Name your agent" : "@\(mentionSlug(clean))")
                .font(.system(size: Theme.small)).foregroundStyle(theme.muted)
            TextField("", text: $name, prompt: Text("Agent name").foregroundStyle(theme.placeholder))
                .font(.system(size: Theme.body)).foregroundStyle(theme.text)
                .multilineTextAlignment(.center)
                .focused($focused)
                .submitLabel(.done)
                .onSubmit(create)
                .padding(.horizontal, 16).frame(height: 50)
                .background(RoundedRectangle(cornerRadius: 25, style: .continuous).fill(theme.field))
                .overlay(RoundedRectangle(cornerRadius: 25, style: .continuous).strokeBorder(theme.fieldBorder, lineWidth: 1))
                .padding(.horizontal, 24)
            Button(action: create) {
                Text("Create agent")
                    .font(.system(size: Theme.body, weight: .semibold)).foregroundStyle(theme.onPrimary)
                    .frame(maxWidth: .infinity).frame(height: 50)
                    .background(Capsule().fill(theme.primary))
            }
            .buttonStyle(.plain)
            .disabled(clean.isEmpty)
            .opacity(clean.isEmpty ? 0.45 : 1)
            .padding(.horizontal, 24)
            Text("A new agent with its own workspace on your machine. Tap the picture to pick its look. You can rename it and pick its model from its profile.")
                .font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                .multilineTextAlignment(.center).padding(.horizontal, 36)
            Spacer()
        }
        .background(theme.bg)
        .navigationTitle("New agent")
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(theme.bg, for: .navigationBar)
        .toolbarBackground(.visible, for: .navigationBar)
        // Push (not a nested sheet): New agent already lives in the New Message sheet.
        .navigationDestination(isPresented: $showLook) {
            NewAgentLookView(
                slug: Binding(
                    get: { previewSlug },
                    set: { pickedSlug = $0 }
                ),
                colorName: Binding(
                    get: { pickedColorName ?? previewColor?.name ?? "mono" },
                    set: { pickedColorName = $0 }
                )
            )
        }
        .onAppear { focused = true }
    }

    /// FNV-1a so the preview colour is the same on every launch (Swift's
    /// `hashValue` is seeded per process).
    static func stableIndex(_ text: String, _ count: Int) -> Int {
        var h: UInt32 = 2166136261
        for b in text.utf8 { h = (h ^ UInt32(b)) &* 16777619 }
        return Int(h % UInt32(max(count, 1)))
    }

    private func create() {
        guard !clean.isEmpty else { return }
        // Hex picks are the blob palette (`color`); slug names are tree tints.
        let blob = pickedColorName.flatMap { $0.hasPrefix("#") ? $0 : nil }
        let tint = pickedColorName.flatMap { $0.hasPrefix("#") ? nil : $0 }
        model.createProject(name: clean, avatar: pickedSlug, avatarColor: tint, color: blob)
        onDone()
    }
}

/// Character + colour grids for a new agent (same pieces as the agent profile).
/// Pushed from New agent so it stays inside the New Message navigation stack.
struct NewAgentLookView: View {
    @Environment(\.theme) private var theme
    @Binding var slug: String?
    @Binding var colorName: String

    /// Tint for the tree preview in the grids; mono → nil (white tree).
    private var tint: TreeColor? {
        colorName == "mono" ? nil : TreeAvatars.shared.color(named: colorName)
    }

    /// Selection ring in the colour grid (includes mono).
    private var gridColor: TreeColor {
        if colorName == "mono" { return .mono }
        return TreeAvatars.shared.color(named: colorName) ?? .mono
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                if TreeAvatars.shared.isEmpty {
                    Text("Colour").font(.system(size: Theme.body, weight: .medium)).foregroundStyle(theme.text)
                    HStack(spacing: 0) {
                        ForEach(AgentEditSheet.colorHexes, id: \.self) { hex in
                            let selected = hex.lowercased() == colorName.lowercased()
                            Button { colorName = hex } label: {
                                ZStack {
                                    Circle().fill(Color(hex: hex) ?? .gray).frame(width: 26, height: 26)
                                    if selected {
                                        Circle().strokeBorder(theme.text, lineWidth: 2).frame(width: 34, height: 34)
                                    }
                                }
                                .frame(maxWidth: .infinity)
                                .frame(height: 36)
                            }
                            .buttonStyle(.plain)
                        }
                    }
                } else {
                    if TreeAvatars.shared.colorsTintable, let slug {
                        Text("Colour").font(.system(size: Theme.body, weight: .medium)).foregroundStyle(theme.text)
                        TreeColorGrid(slug: slug, current: gridColor) { c in
                            colorName = c.name
                        }
                    }
                    Text("Character").font(.system(size: Theme.body, weight: .medium)).foregroundStyle(theme.text)
                    TreePickerGrid(current: slug, color: tint) { s in
                        slug = s
                    }
                }
            }
            .padding(.horizontal, 16)
            .padding(.top, 12)
            .padding(.bottom, 28)
        }
        .background(theme.bg)
        .navigationTitle("Look")
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(theme.bg, for: .navigationBar)
        .toolbarBackground(.visible, for: .navigationBar)
    }
}

/// New group: pick people (and your own agents), then create.
struct NewGroupView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    let onDone: () -> Void
    @State private var query = ""
    @State private var remote: [Person] = []
    @State private var picked: [Person] = []
    @State private var pickedAgents: [Project] = []
    @State private var creating = false
    /// Shown on this screen — the root notice alert sits under the sheet and looks like a silent fail.
    @State private var errorText: String?

    private var trimmed: String { query.trimmingCharacters(in: .whitespaces).trimmingCharacters(in: CharacterSet(charactersIn: "@")) }

    /// Own agents only (same as Members → Add agent); shared ones cannot be re-shared here.
    private var agentCandidates: [Project] {
        let q = trimmed.lowercased()
        let base = model.projects.filter { !$0.shared && !model.isArchived($0.id) }
            .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
        if q.isEmpty { return base }
        return base.filter { $0.name.lowercased().contains(q) || mentionSlug($0.name).contains(q) }
    }

    private var candidates: [Person] {
        let q = trimmed.lowercased()
        var out = q.isEmpty ? model.directory : model.directory.filter { $0.handle.lowercased().contains(q) || ($0.name ?? "").lowercased().contains(q) }
        var seen = Set(out.map(\.id))
        for p in remote where !seen.contains(p.id) && p.id != model.user?.id { seen.insert(p.id); out.append(p) }
        return out
    }

    var body: some View {
        VStack(spacing: 0) {
            if !picked.isEmpty || !pickedAgents.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(pickedAgents) { p in
                            HStack(spacing: 6) {
                                AgentAvatar(project: p, theme: theme, size: 22, still: true)
                                Text("@\(mentionSlug(p.name))").font(.system(size: Theme.tiny, weight: .medium)).foregroundStyle(theme.text)
                                Button { toggleAgent(p) } label: { Image(systemName: "xmark").font(.system(size: 10, weight: .bold)).foregroundStyle(theme.muted) }
                                    .buttonStyle(.plain)
                            }
                            .padding(.leading, 4).padding(.trailing, 10).frame(height: 32)
                            .background(Capsule().fill(theme.field))
                        }
                        ForEach(picked) { p in
                            HStack(spacing: 6) {
                                PersonAvatar(url: p.avatarUrl, handle: p.handle, size: 22)
                                Text("@\(p.handle)").font(.system(size: Theme.tiny, weight: .medium)).foregroundStyle(theme.text)
                                Button { toggle(p) } label: { Image(systemName: "xmark").font(.system(size: 10, weight: .bold)).foregroundStyle(theme.muted) }
                                    .buttonStyle(.plain)
                            }
                            .padding(.leading, 4).padding(.trailing, 10).frame(height: 32)
                            .background(Capsule().fill(theme.field))
                        }
                    }
                    .padding(.horizontal, 16)
                }
                .frame(height: 44)
            }
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass").font(.system(size: 15, weight: .medium)).foregroundStyle(theme.muted)
                TextField("", text: $query, prompt: Text("Add people or agents").foregroundStyle(theme.placeholder))
                    .textInputAutocapitalization(.never).autocorrectionDisabled()
                    .font(.system(size: Theme.body)).foregroundStyle(theme.text)
            }
            .padding(.horizontal, 12).frame(height: 38)
            .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(theme.field))
            .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(theme.fieldBorder, lineWidth: 1))
            .padding(.horizontal, 16).padding(.vertical, 10)
            .onChange(of: query) { _, _ in search() }

            if let errorText {
                Text(errorText)
                    .font(.system(size: Theme.small))
                    .foregroundStyle(theme.danger)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 20)
                    .padding(.bottom, 8)
            }

            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    if !agentCandidates.isEmpty {
                        Text("YOUR AGENTS")
                            .font(.system(size: 12, weight: .medium))
                            .kerning(0.4)
                            .foregroundStyle(theme.muted)
                            .padding(.horizontal, 16)
                            .padding(.bottom, 8)
                        VStack(spacing: 0) {
                            ForEach(Array(agentCandidates.enumerated()), id: \.element.id) { i, p in
                                Button { toggleAgent(p) } label: {
                                    AgentPickRow(project: p, trailing: AnyView(
                                        Image(systemName: pickedAgents.contains(where: { $0.id == p.id }) ? "checkmark.circle.fill" : "circle")
                                            .font(.system(size: 22))
                                            .foregroundStyle(pickedAgents.contains(where: { $0.id == p.id }) ? theme.text : theme.muted.opacity(0.5))
                                    ))
                                }
                                .buttonStyle(.plain)
                                if i < agentCandidates.count - 1 { Rectangle().fill(theme.separator).frame(height: 0.5).padding(.leading, 72) }
                            }
                        }
                        .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(theme.card))
                        .padding(.horizontal, 16)
                        .padding(.bottom, 18)
                    }

                    if !candidates.isEmpty || agentCandidates.isEmpty {
                        if !agentCandidates.isEmpty {
                            Text("PEOPLE")
                                .font(.system(size: 12, weight: .medium))
                                .kerning(0.4)
                                .foregroundStyle(theme.muted)
                                .padding(.horizontal, 16)
                                .padding(.bottom, 8)
                        }
                        VStack(spacing: 0) {
                            ForEach(Array(candidates.enumerated()), id: \.element.id) { i, p in
                                Button { toggle(p) } label: {
                                    PersonRow(person: p, trailing: AnyView(
                                        Image(systemName: picked.contains(p) ? "checkmark.circle.fill" : "circle")
                                            .font(.system(size: 22))
                                            .foregroundStyle(picked.contains(p) ? theme.text : theme.muted.opacity(0.5))
                                    ))
                                }
                                .buttonStyle(.plain)
                                if i < candidates.count - 1 { Rectangle().fill(theme.separator).frame(height: 0.5).padding(.leading, 72) }
                            }
                            if candidates.isEmpty {
                                Text(trimmed.isEmpty ? "No one else on Arbos yet. Search a GitHub username." : "No one matches @\(trimmed).")
                                    .font(.system(size: Theme.small)).foregroundStyle(theme.muted)
                                    .multilineTextAlignment(.center).padding(.horizontal, 36).padding(.vertical, 24)
                            }
                        }
                        .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(theme.card))
                        .padding(.horizontal, 16)
                    }
                    Color.clear.frame(height: 40)
                }
            }
        }
        .background(theme.bg)
        .navigationTitle("New group")
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(theme.bg, for: .navigationBar)
        .toolbarBackground(.visible, for: .navigationBar)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button { create() } label: {
                    Text(creating ? "Creating…" : "Create")
                        .font(.system(size: Theme.small, weight: .semibold))
                        .foregroundStyle(theme.onPrimary)
                        .padding(.horizontal, 12)
                        .frame(height: 32)
                        .background(Capsule().fill(theme.primary))
                }
                .buttonStyle(.plain)
                .disabled(picked.isEmpty || creating)
                .opacity(picked.isEmpty || creating ? 0.45 : 1)
            }
        }
        .task { await model.loadDirectory() }
    }

    private func toggle(_ p: Person) {
        if let i = picked.firstIndex(of: p) { picked.remove(at: i) } else { picked.append(p) }
        errorText = nil
    }

    private func toggleAgent(_ p: Project) {
        if let i = pickedAgents.firstIndex(where: { $0.id == p.id }) { pickedAgents.remove(at: i) } else { pickedAgents.append(p) }
        errorText = nil
    }

    private func search() {
        let q = trimmed
        guard q.count >= 2 else { remote = []; return }
        Task {
            try? await Task.sleep(nanoseconds: 250_000_000)
            guard q == trimmed else { return }
            remote = await model.searchPeople(q)
        }
    }

    private func create() {
        guard !picked.isEmpty, !creating else { return }
        creating = true
        errorText = nil
        let agentIds = pickedAgents.map(\.id)
        Task {
            if await model.createGroup(handles: picked.map(\.handle), agentIds: agentIds) {
                onDone()
            } else {
                // Copy onto this screen: RootView's alert is behind the sheet.
                errorText = model.notice?.text ?? "Could not create the group."
                model.notice = nil
            }
            creating = false
        }
    }
}

/// Members of a people chat: people, shared agents (with owner), lock state,
/// Add person / Add agent (own projects), reply policy, remove, block, leave, delete.
struct MembersSheet: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    let chatId: String
    @State private var showAddPerson = false
    @State private var showAddAgent = false
    @State private var confirmDelete = false
    @State private var confirmLeave = false
    @State private var detent: PresentationDetent = .medium

    private var chat: PeopleChat? { model.peopleChat(chatId) }
    private var myProjects: [Project] { model.projects.filter { !$0.shared } }
    private var memberIds: Set<String> { Set((chat?.humans ?? []).map(\.id)) }

    var body: some View {
        NavigationStack {
            List {
                if let chat {
                    Section {
                        ForEach(chat.humans) { m in
                            HStack(spacing: 12) {
                                Button { model.showProfile(.user(ProfilePerson(m))) } label: { PersonAvatar(url: m.avatarUrl, handle: m.handle, size: 40) }.buttonStyle(.plain)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(m.displayName).foregroundStyle(theme.text)
                                    Text("@\(m.handle)\(m.status == "invited" ? " · invited" : "")").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                                }
                                Spacer()
                                if m.id == model.user?.id { Text("You").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted) }
                            }
                            .swipeActions {
                                if m.id != model.user?.id {
                                    Button("Block") { model.block(m) }.tint(theme.danger)
                                }
                            }
                        }
                        // Push via navigationDestination (not NavigationLink / nested sheet):
                        // List NavigationLinks are flaky in a medium detent sheet, and dismiss()
                        // from a pushed picker can close Members instead of popping.
                        Button {
                            detent = .large
                            showAddPerson = true
                        } label: {
                            Label("Add person", systemImage: "plus.circle").foregroundStyle(theme.text)
                        }
                    } header: { Text("People") } footer: { Text(chat.lockLabel) }
                    .listRowBackground(theme.card)

                    Section {
                        ForEach(chat.agents) { a in
                            HStack(spacing: 12) {
                                if let p = a.project {
                                    Button { model.showProfile(.agent(projectId: p.id)) } label: { AgentAvatar(project: p, theme: theme, size: 40) }.buttonStyle(.plain)
                                }
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(a.displayName).foregroundStyle(theme.text)
                                    Text("@\(mentionSlug(a.project?.name ?? a.handle)) · owned by @\(a.ownerHandle ?? "?")\(a.replyPolicy == "every_message" ? " · replies to everything" : "")")
                                        .font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                                }
                                Spacer()
                                if a.ownerId == model.user?.id {
                                    Menu {
                                        Button("Reply when mentioned") { model.setReplyPolicy(chatId: chatId, projectId: a.id, policy: "mention") }
                                        Button("Reply to every message") { model.setReplyPolicy(chatId: chatId, projectId: a.id, policy: "every_message") }
                                        Divider()
                                        Button("Remove agent", role: .destructive) { model.removeAgent(chatId: chatId, projectId: a.id) }
                                    } label: {
                                        Image(systemName: "ellipsis.circle").foregroundStyle(theme.muted)
                                    }
                                }
                            }
                        }
                        Button { showAddAgent = true } label: {
                            Label("Add agent", systemImage: "plus.circle").foregroundStyle(theme.text)
                        }
                        .disabled(myProjects.isEmpty)
                    } header: { Text("Agents") } footer: {
                        Text(chat.agents.isEmpty ? "Add one of your agents so both of you can talk to it. Mention it with @ to get a reply." : "Guests can ask a shared agent to do anything its owner can. The owner pays and can remove it any time.")
                    }
                    .listRowBackground(theme.card)

                    Section {
                        Button("Leave chat", role: .destructive) { confirmLeave = true }
                        if chat.myRole == "owner" {
                            Button("Delete chat for everyone", role: .destructive) { confirmDelete = true }
                        }
                    }
                    .listRowBackground(theme.card)
                }
            }
            .scrollContentBackground(.hidden)
            .background(theme.bg)
            .navigationTitle("Members")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } } }
            .navigationDestination(isPresented: $showAddPerson) {
                PersonPicker(excludeIds: memberIds) { person in
                    model.addPerson(chatId: chatId, handle: person.handle)
                    showAddPerson = false
                }
            }
            .sheet(isPresented: $showAddAgent) {
                AgentPicker(projects: myProjects) { p in
                    model.addAgent(chatId: chatId, projectId: p.id)
                }
                .environment(\.theme, theme)
            }
            .confirmationDialog("Leave this chat?", isPresented: $confirmLeave, titleVisibility: .visible) {
                Button("Leave", role: .destructive) { model.leavePeopleChat(chatId); dismiss() }
            }
            .confirmationDialog("Delete for everyone?", isPresented: $confirmDelete, titleVisibility: .visible) {
                Button("Delete chat", role: .destructive) { model.deletePeopleChat(chatId); dismiss() }
            } message: { Text("Messages, files and the sync log are removed for every member.") }
        }
        .presentationDetents([.medium, .large], selection: $detent)
    }
}

/// Pick a person (directory + GitHub search) who is not already in the chat.
/// Pushed onto Members via navigationDestination (same idea as New agent → look).
struct PersonPicker: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    let excludeIds: Set<String>
    let onPick: (Person) -> Void
    @State private var query = ""
    @State private var remote: [Person] = []
    @State private var searching = false

    private var trimmed: String {
        query.trimmingCharacters(in: .whitespaces).trimmingCharacters(in: CharacterSet(charactersIn: "@"))
    }

    private var people: [Person] {
        let q = trimmed.lowercased()
        let local = (q.isEmpty ? model.directory : model.directory.filter {
            $0.handle.lowercased().contains(q) || ($0.name ?? "").lowercased().contains(q)
        }).filter { !excludeIds.contains($0.id) && $0.id != model.user?.id }
        var seen = Set(local.map(\.id))
        var out = local
        for p in remote where !seen.contains(p.id) && !excludeIds.contains(p.id) && p.id != model.user?.id {
            seen.insert(p.id)
            out.append(p)
        }
        return out
    }

    var body: some View {
        List {
            Section {
                HStack(spacing: 8) {
                    Image(systemName: "magnifyingglass").foregroundStyle(theme.muted)
                    TextField("", text: $query, prompt: Text("Search people").foregroundStyle(theme.placeholder))
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                        .foregroundStyle(theme.text)
                    if searching { ProgressView().controlSize(.small).tint(theme.muted) }
                }
            }
            .listRowBackground(theme.card)

            Section {
                ForEach(people) { p in
                    Button {
                        onPick(p)
                    } label: {
                        PersonRow(person: p)
                    }
                    .buttonStyle(.plain)
                }
                if people.isEmpty {
                    Text(trimmed.isEmpty
                         ? "No one else on Arbos yet. Search a GitHub username."
                         : "No one matches @\(trimmed).")
                        .font(.system(size: Theme.small)).foregroundStyle(theme.muted)
                    if trimmed.count >= 2, !searching {
                        Button {
                            onPick(Person(id: "github:\(trimmed.lowercased())", handle: trimmed, avatarUrl: nil, name: nil, status: "github"))
                        } label: {
                            Text("Invite @\(trimmed)")
                                .font(.system(size: Theme.small, weight: .semibold))
                                .foregroundStyle(theme.text)
                        }
                    }
                }
            }
            .listRowBackground(theme.card)
        }
        .scrollContentBackground(.hidden)
        .background(theme.bg)
        .navigationTitle("Add person")
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(theme.bg, for: .navigationBar)
        .toolbarBackground(.visible, for: .navigationBar)
        .task { await model.loadDirectory() }
        .onChange(of: query) { _, _ in search() }
    }

    private func search() {
        let q = trimmed
        guard q.count >= 2 else { remote = []; searching = false; return }
        searching = true
        Task {
            try? await Task.sleep(nanoseconds: 250_000_000)
            guard q == trimmed else { return }
            remote = await model.searchPeople(q)
            if q == trimmed { searching = false }
        }
    }
}

/// Pick one of my projects to share into a chat.
struct AgentPicker: View {
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    let projects: [Project]
    let onPick: (Project) -> Void

    var body: some View {
        NavigationStack {
            List(projects) { p in
                Button { onPick(p); dismiss() } label: {
                    HStack(spacing: 12) {
                        AgentAvatar(project: p, theme: theme, size: 40)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(p.name).foregroundStyle(theme.text)
                            Text("@\(mentionSlug(p.name))").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                        }
                    }
                }
                .listRowBackground(theme.card)
            }
            .scrollContentBackground(.hidden)
            .background(theme.bg)
            .navigationTitle("Add agent")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarLeading) { Button("Cancel") { dismiss() } } }
        }
        .presentationDetents([.medium])
    }
}

/// Project members (D72 step 3): invite by @handle, list, remove (owner).
struct ProjectMembersSheet: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    let project: Project
    @State private var members: [ProjectMember] = []
    @State private var handle = ""
    @State private var busy = false

    var body: some View {
        NavigationStack {
            List {
                if !project.shared {
                    Section {
                        HStack(spacing: 8) {
                            Text("@").foregroundStyle(theme.muted)
                            TextField("", text: $handle, prompt: Text("GitHub username").foregroundStyle(theme.placeholder))
                                .textInputAutocapitalization(.never).autocorrectionDisabled()
                                .onSubmit(invite)
                            Button(busy ? "…" : "Invite", action: invite)
                                .font(.system(size: Theme.small, weight: .semibold))
                                .disabled(busy || handle.trimmingCharacters(in: .whitespaces).isEmpty)
                        }
                    } footer: {
                        Text("A member drives this agent with you, sees its chats and files, and spends your credits. You can remove them any time.")
                    }
                    .listRowBackground(theme.card)
                }
                Section("Members") {
                    ForEach(members) { m in
                        HStack(spacing: 12) {
                            Button { model.showProfile(.user(ProfilePerson(m))) } label: { PersonAvatar(url: m.avatarUrl, handle: m.handle, size: 40) }.buttonStyle(.plain)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(m.name?.isEmpty == false ? m.name! : m.handle).foregroundStyle(theme.text)
                                Text("@\(m.handle) · \(m.role)\(m.status == "invited" ? " · invited" : "")").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                            }
                        }
                        .swipeActions {
                            if !project.shared && m.role != "owner" {
                                Button("Remove", role: .destructive) {
                                    Task { await model.removeProjectMember(project.id, userId: m.id); await load() }
                                }
                            }
                        }
                    }
                    if members.isEmpty { Text("Just you so far.").foregroundStyle(theme.muted) }
                }
                .listRowBackground(theme.card)
            }
            .scrollContentBackground(.hidden)
            .background(theme.bg)
            .navigationTitle("Share \(project.name)")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } } }
            .task { await load() }
        }
        .presentationDetents([.medium, .large])
    }

    private func load() async { members = await model.projectMembers(project.id) }

    private func invite() {
        let h = handle.trimmingCharacters(in: .whitespaces)
        guard !h.isEmpty, !busy else { return }
        busy = true
        Task {
            if let list = await model.inviteToProject(project.id, handle: h) { members = list; handle = "" }
            busy = false
        }
    }
}
