// In-app feedback (the TestFlight "Share Beta Feedback" fallback): a
// screenshot of the current screen plus a note, sent to the relay's feedback
// endpoint, which feeds the same automatic fix loop as the Mac sheet.
// Opened by a shake, a system screenshot, or Settings → Send feedback.
//
// Endpoint: POST /feedback (device token) JSON {note, image: base64 PNG/JPEG
// ≤15 MB, mime, app_version, platform: "ios", os, project_id?, width,
// height} → 201 {id, created_at}; 400 bad image, 413 too big, 403 not a
// device token. Identity comes from the token.

import SwiftUI
import UIKit

@MainActor
final class FeedbackCenter: ObservableObject {
    static let shared = FeedbackCenter()
    static let shake = Notification.Name("arbos.deviceShake")

    @Published var isPresented = false
    @Published var screenshot: UIImage?
    /// Where the user was: a chat (agent/person) or a screen name.
    @Published var context = Context()
    private var lastOpened: Date = .distantPast

    struct Context: Equatable {
        var screen: String = "home"
        var chatId: String? = nil
        var projectId: String? = nil
    }

    /// Capture first, then present (so the sheet is not in its own picture).
    func open(screen: String? = nil, chatId: String? = nil, projectId: String? = nil) {
        guard !isPresented, Date().timeIntervalSince(lastOpened) > 1 else { return }
        lastOpened = Date()
        if let screen { context.screen = screen }
        context.chatId = chatId
        context.projectId = projectId
        screenshot = FeedbackCenter.captureScreen()
        isPresented = true
    }

    /// The key window drawn into an image at screen scale.
    static func captureScreen() -> UIImage? {
        guard let scene = UIApplication.shared.connectedScenes.compactMap({ $0 as? UIWindowScene }).first(where: { $0.activationState == .foregroundActive }),
              let window = scene.windows.first(where: \.isKeyWindow) ?? scene.windows.first else { return nil }
        let format = UIGraphicsImageRendererFormat()
        format.scale = window.screen.scale
        format.opaque = true
        return UIGraphicsImageRenderer(bounds: window.bounds, format: format).image { _ in
            window.drawHierarchy(in: window.bounds, afterScreenUpdates: false)
        }
    }

    static var appVersion: String {
        let v = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "?"
        let b = Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "?"
        return "\(v) (\(b))"
    }

    static var deviceLine: String {
        var systemInfo = utsname()
        uname(&systemInfo)
        let machine = withUnsafePointer(to: &systemInfo.machine) { ptr in
            ptr.withMemoryRebound(to: CChar.self, capacity: 1) { String(validatingUTF8: $0) ?? "" }
        }
        let model = machine.isEmpty || machine.hasPrefix("arm64") || machine.hasPrefix("x86") ? UIDevice.current.model + " (Simulator)" : machine
        return "\(model) · iOS \(UIDevice.current.systemVersion)"
    }
}

/// Shake → feedback. Overriding in an extension catches every window.
extension UIWindow {
    open override func motionEnded(_ motion: UIEvent.EventSubtype, with event: UIEvent?) {
        super.motionEnded(motion, with: event)
        if motion == .motionShake {
            NotificationCenter.default.post(name: FeedbackCenter.shake, object: nil)
        }
    }
}
