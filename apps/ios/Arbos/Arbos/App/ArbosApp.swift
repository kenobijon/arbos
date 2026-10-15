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
            if phase == .active { Task { await push.refreshStatus(); await model.refreshUnread() } }
        }
    }

    /// Ask once, a beat after the home screen appears (never in the fixture or a live demo).
    private func schedulePrimer(_ paired: Bool) {
        guard paired, !model.isFixture, !DebugHooks.liveDemo, push.shouldShowPrimer, !showPrimer else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) {
            if push.shouldShowPrimer, model.isPaired { showPrimer = true }
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
