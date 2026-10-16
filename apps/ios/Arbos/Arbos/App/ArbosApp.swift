import SwiftUI

@main
struct ArbosApp: App {
    @UIApplicationDelegateAdaptor(PushAppDelegate.self) private var pushDelegate
    @StateObject private var model = AppModel()
    @Environment(\.colorScheme) private var systemScheme

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(model)
        }
    }
}

struct RootView: View {
    @EnvironmentObject private var model: AppModel
    @ObservedObject private var push = PushManager.shared
    @ObservedObject private var feedback = FeedbackCenter.shared
    @ObservedObject private var calls = CallManager.shared
    @Environment(\.colorScheme) private var systemScheme
    @Environment(\.scenePhase) private var scenePhase
    @State private var showPrimer = false

    private var theme: Theme {
        Theme.resolve(appearance: model.appearance, surface: model.surface, systemDark: systemScheme == .dark)
    }

    var body: some View {
        Group {
            if model.isPaired {
                HomeView()
            } else {
                SignInView()
            }
        }
        .environment(\.theme, theme)
        .preferredColorScheme(preferredScheme)
        .tint(theme.text)
        .alert(item: $model.notice) { n in
            Alert(title: Text("Arbos"), message: Text(n.text), dismissButton: .default(Text("OK")))
        }
        .sheet(isPresented: $showPrimer) {
            NotificationsPrimerView().environment(\.theme, theme)
        }
        .fullScreenCover(isPresented: $calls.isPresented) {
            CallScreen(expected: expectedPeer)
                .environmentObject(model)
                .environment(\.theme, theme)
                .preferredColorScheme(theme.isDark ? .dark : .light)
        }
        .fullScreenCover(isPresented: Binding(get: { calls.incoming != nil && !calls.isPresented }, set: { if !$0 { calls.decline() } })) {
            IncomingCallView()
                .environmentObject(model)
                .environment(\.theme, theme)
                .preferredColorScheme(theme.isDark ? .dark : .light)
        }
        .sheet(isPresented: $feedback.isPresented) {
            FeedbackSheet().environmentObject(model).environment(\.theme, theme)
        }
        .onReceive(NotificationCenter.default.publisher(for: FeedbackCenter.shake)) { _ in
            guard model.isPaired else { return }
            feedback.open(screen: model.path.isEmpty ? "home" : "chat",
                          chatId: model.path.last.map { if case .chat(let c, _) = $0 { return c } else { return "" } },
                          projectId: model.path.last.map { if case .chat(_, let p) = $0 { return p } else { return "" } })
        }
        .onChange(of: model.isPaired, initial: true) { _, paired in schedulePrimer(paired) }
        .onChange(of: push.status) { _, _ in schedulePrimer(model.isPaired) }
        .onChange(of: scenePhase) { _, phase in
            // Match Mac `window_active`: only suppress push while the scene is
            // active. Background / lock clears focused_chat so APNs banners fire.
            model.setAppActive(phase == .active)
            if phase == .active { Task { await push.refreshStatus(); await model.refreshUnread() } }
        }
    }

    /// The other person of the DM the call belongs to, for the card before they join.
    private var expectedPeer: CallPerson? {
        guard let c = calls.call, let chat = model.peopleChat(c.chatId), let other = chat.counterpart(me: model.user?.id) else { return nil }
        return CallPerson(id: other.id, kind: "user", name: other.name, handle: other.handle, avatar: nil, avatarColor: nil, avatarUrl: other.avatarUrl)
    }

    /// Ask once, a beat after the home screen appears (never in the fixture or a live demo).
    private func schedulePrimer(_ paired: Bool) {
        guard paired, !model.isFixture, !DebugHooks.liveDemo, DebugHooks.show == nil, push.shouldShowPrimer, !showPrimer else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) {
            // Never on top of another sheet (feedback, settings…).
            if push.shouldShowPrimer, model.isPaired, !feedback.isPresented { showPrimer = true }
        }
    }

    private var preferredScheme: ColorScheme? {
        switch model.appearance {
        case .system: return nil
        case .dark: return .dark
        case .light: return .light
        }
    }
}
