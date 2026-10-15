// Voice / video call screen, Grok Bot layout: avatar, name, "Calling…" status,
// mute / camera / hang-up controls. This relay has no media endpoint yet, so the
// call never connects: after a moment the status says so and the only way
// out is Hang up. The visual is in place for when the backend lands.

import SwiftUI

struct VoiceModeView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.dismiss) private var dismiss
    /// Agent project to call; nil for a people-chat peer.
    var project: Project? = nil
    /// People-chat peer when `project` is nil.
    var peerName: String = ""
    var peerHandle: String = ""
    var peerAvatarUrl: String? = nil
    /// Video call chrome (camera controls) vs voice-only.
    var video: Bool = false

    @State private var status = "Calling…"
    @State private var muted = false
    @State private var cameraOn = true
    @State private var pulse = false

    private var title: String { video ? "Video call" : "Voice chat" }
    private var displayName: String {
        if let project { return project.name }
        return peerName.isEmpty ? (peerHandle.isEmpty ? "Chat" : "@\(peerHandle)") : peerName
    }

    var body: some View {
        ZStack {
            theme.bg.ignoresSafeArea()
            VStack(spacing: 0) {
                HStack {
                    Spacer()
                    Text(title)
                        .font(.system(size: Theme.small, weight: .semibold))
                        .foregroundStyle(theme.muted)
                    Spacer()
                }
                .padding(.top, 14)
                Spacer()
                ZStack {
                    if video {
                        RoundedRectangle(cornerRadius: 28, style: .continuous)
                            .fill(theme.card)
                            .frame(width: 240, height: 320)
                            .overlay(
                                RoundedRectangle(cornerRadius: 28, style: .continuous)
                                    .strokeBorder(theme.fieldBorder, lineWidth: 1)
                            )
                            .overlay {
                                VStack(spacing: 16) {
                                    peerOrAgentAvatar(size: 96)
                                    Image(systemName: cameraOn ? "video.fill" : "video.slash.fill")
                                        .font(.system(size: 22, weight: .semibold))
                                        .foregroundStyle(theme.muted)
                                }
                            }
                            .scaleEffect(pulse ? 1.02 : 0.98)
                    } else {
                        Circle()
                            .fill(accent.opacity(0.18))
                            .frame(width: 220, height: 220)
                            .scaleEffect(pulse ? 1.08 : 0.94)
                        peerOrAgentAvatar(size: 128)
                    }
                }
                .onAppear {
                    withAnimation(.easeInOut(duration: 1.6).repeatForever(autoreverses: true)) { pulse = true }
                }
                Text(displayName)
                    .font(.system(size: 28, weight: .semibold))
                    .foregroundStyle(theme.text)
                    .padding(.top, 28)
                Text(status)
                    .font(.system(size: Theme.body))
                    .foregroundStyle(theme.muted)
                    .padding(.top, 6)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 32)
                Spacer()
                HStack(spacing: 28) {
                    control(icon: muted ? "mic.slash.fill" : "mic.fill", label: muted ? "Unmute" : "Mute") { muted.toggle() }
                    if video {
                        control(icon: cameraOn ? "video.fill" : "video.slash.fill", label: cameraOn ? "Camera off" : "Camera on") {
                            cameraOn.toggle()
                        }
                    } else {
                        control(icon: "text.bubble", label: "Transcript") { }
                    }
                    Button { dismiss() } label: {
                        VStack(spacing: 8) {
                            Circle().fill(theme.danger).frame(width: 64, height: 64)
                                .overlay(Image(systemName: "phone.down.fill").font(.system(size: 24, weight: .semibold)).foregroundStyle(.white))
                            Text("Hang up").font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
                        }
                    }
                    .buttonStyle(.plain)
                }
                .padding(.bottom, 48)
            }
        }
        .task {
            try? await Task.sleep(nanoseconds: 2_200_000_000)
            if video {
                status = "Video calls are not available on this relay yet.\nText replies keep working."
            } else {
                status = "Voice chat is not available on this relay yet.\nText replies keep working."
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(title)
    }

    private var accent: Color {
        if let project { return theme.projectColor(project) }
        return theme.bubbleUser
    }

    @ViewBuilder
    private func peerOrAgentAvatar(size: CGFloat) -> some View {
        if let project {
            AgentAvatar(project: project, theme: theme, size: size, working: model.isStreaming(project: project.id))
        } else {
            PersonAvatar(url: peerAvatarUrl, handle: peerHandle.isEmpty ? displayName : peerHandle, size: size)
        }
    }

    private func control(icon: String, label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            VStack(spacing: 8) {
                GlassCircle(size: 64) {
                    Image(systemName: icon).font(.system(size: 22, weight: .medium)).foregroundStyle(theme.text)
                }
                Text(label).font(.system(size: Theme.tiny)).foregroundStyle(theme.muted)
            }
        }
        .buttonStyle(.plain)
    }
}
