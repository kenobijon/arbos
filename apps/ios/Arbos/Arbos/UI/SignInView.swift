// First launch, Grok Bot welcome style: blobs, name, tagline and one
// button — "Sign in with GitHub" — against the compiled-in relay. "Advanced"
// reveals the relay address for self-hosters; a relay in paircode mode shows
// the one-time-code fields instead of the GitHub button.

import SwiftUI

struct SignInView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @State private var advanced = false
    @State private var relayUrl = ""

    var body: some View {
        ZStack {
            theme.bg.ignoresSafeArea()
            ScrollView {
                VStack(spacing: 16) {
                    Spacer(minLength: 120)
                    HStack(spacing: -10) {
                        let trees = TreeAvatars.shared.characters
                        if trees.count >= 3 {
                            // Coloured trees (D74); the approved trio when the bundle has them.
                            let picks = ["green", "blue", "red"]
                            ForEach(Array(trees.prefix(3).enumerated()), id: \.element.id) { i, c in
                                TreeCharacterView(slug: c.slug, size: 54,
                                                  color: TreeAvatars.shared.color(named: picks[i]) ?? TreeAvatars.shared.defaultColor(forProjectId: c.slug))
                            }
                        } else {
                            BlobAvatar(color: Theme.palette[1], shape: .blob, size: 54)
                            BlobAvatar(color: Theme.palette[0], shape: .drop, size: 54)
                            BlobAvatar(color: Theme.palette[2], shape: .cloud, size: 54)
                        }
                    }
                    .padding(.bottom, 10)
                    Text("Arbos")
                        .font(.system(size: 30, weight: .semibold))
                        .foregroundStyle(theme.text)
                    Text("Your team of always-on agents\nthat finish the work.")
                        .font(.system(size: Theme.body))
                        .foregroundStyle(theme.muted)
                        .multilineTextAlignment(.center)
                        .padding(.bottom, 26)

                    if model.authMode == .paircode {
                        PairingFields()
                    } else {
                        Button(action: model.signInWithGitHub) {
                            HStack(spacing: 10) {
                                if model.signingIn {
                                    ProgressView().tint(theme.onPrimary)
                                } else {
                                    GitHubMark().fill(theme.onPrimary).frame(width: 20, height: 20)
                                }
                                Text(model.signingIn ? "Signing in…" : "Sign in with GitHub")
                                    .font(.system(size: Theme.body, weight: .semibold))
                            }
                            .foregroundStyle(theme.onPrimary)
                            .frame(maxWidth: .infinity)
                            .frame(height: 50)
                            .background(Capsule().fill(theme.primary))
                        }
                        .buttonStyle(.plain)
                        .disabled(model.signingIn)
                        .padding(.horizontal, 24)

                        if let err = model.signInError {
                            Text(err)
                                .font(.system(size: Theme.small))
                                .foregroundStyle(theme.danger)
                                .multilineTextAlignment(.center)
                                .padding(.horizontal, 28)
                        }
                        Text("Any GitHub account works. Your agents run on your own cloud machine.")
                            .font(.system(size: Theme.tiny))
                            .foregroundStyle(theme.muted)
                            .multilineTextAlignment(.center)
                            .padding(.horizontal, 36)
                    }

                    Button {
                        withAnimation(.easeOut(duration: 0.2)) { advanced.toggle() }
                    } label: {
                        HStack(spacing: 4) {
                            Text("Advanced")
                            Image(systemName: advanced ? "chevron.up" : "chevron.down").font(.system(size: 11, weight: .semibold))
                        }
                        .font(.system(size: Theme.tiny))
                        .foregroundStyle(theme.muted)
                    }
                    .buttonStyle(.plain)
                    .padding(.top, 18)

                    if advanced {
                        VStack(spacing: 8) {
                            HStack(spacing: 10) {
                                Image(systemName: "network").font(.system(size: 15)).foregroundStyle(theme.icon)
                                TextField("", text: $relayUrl, prompt: Text("Relay address").foregroundStyle(theme.placeholder))
                                    .keyboardType(.URL)
                                    .textInputAutocapitalization(.never)
                                    .autocorrectionDisabled()
                                    .font(.system(size: Theme.body))
                                    .foregroundStyle(theme.text)
                                    .onSubmit(applyRelay)
                                if RelayEndpoint(relayUrl).httpUrl != model.relayUrl {
                                    Button("Use", action: applyRelay)
                                        .font(.system(size: Theme.small, weight: .semibold))
                                        .foregroundStyle(theme.text)
                                }
                            }
                            .padding(.horizontal, 16)
                            .frame(height: 50)
                            .background(RoundedRectangle(cornerRadius: 25, style: .continuous).fill(theme.field))
                            .overlay(RoundedRectangle(cornerRadius: 25, style: .continuous).strokeBorder(theme.fieldBorder, lineWidth: 1))
                            Text("Self-hosting? Point the app at your own relay. Relays in pairing-code mode show a code field here.")
                                .font(.system(size: Theme.tiny))
                                .foregroundStyle(theme.muted)
                                .multilineTextAlignment(.center)
                                .padding(.horizontal, 12)
                        }
                        .padding(.horizontal, 24)
                        .transition(.opacity)
                    }
                    Spacer(minLength: 40)
                }
            }
            .scrollDismissesKeyboard(.interactively)
        }
        .onAppear {
            relayUrl = model.relayUrl
            if DebugHooks.show == "signin" {
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) { model.signInWithGitHub() }
            }
        }
    }

    private func applyRelay() {
        let trimmed = relayUrl.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return }
        model.setRelayUrl(trimmed)
    }
}

/// GitHub's mark as a path (so no asset is needed), drawn in a 20×20 box.
struct GitHubMark: Shape {
    func path(in rect: CGRect) -> Path {
        let s = min(rect.width, rect.height) / 16
        var p = Path()
        // Simplified octocat silhouette: head circle + body.
        p.addEllipse(in: CGRect(x: rect.minX + 2 * s, y: rect.minY + 1 * s, width: 12 * s, height: 12 * s))
        var bottom = Path()
        bottom.move(to: CGPoint(x: rect.minX + 5 * s, y: rect.minY + 10 * s))
        bottom.addLine(to: CGPoint(x: rect.minX + 5 * s, y: rect.minY + 16 * s))
        bottom.addLine(to: CGPoint(x: rect.minX + 11 * s, y: rect.minY + 16 * s))
        bottom.addLine(to: CGPoint(x: rect.minX + 11 * s, y: rect.minY + 10 * s))
        bottom.closeSubpath()
        p.addPath(bottom)
        return p
    }
}

/// Pairing-code fallback for relays with `auth_mode = paircode`.
struct PairingFields: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @State private var code = ""

    var body: some View {
        VStack(spacing: 12) {
            HStack(spacing: 10) {
                Image(systemName: "key").font(.system(size: 15)).foregroundStyle(theme.icon)
                TextField("", text: $code, prompt: Text("One-time code").foregroundStyle(theme.placeholder))
                    .keyboardType(.numberPad)
                    .textContentType(.oneTimeCode)
                    .font(.system(size: Theme.body, design: .monospaced))
                    .foregroundStyle(theme.text)
                    .onChange(of: code) { _, new in
                        let digits = new.filter(\.isNumber)
                        if digits != new || digits.count > 6 { code = String(digits.prefix(6)) }
                    }
            }
            .padding(.horizontal, 16)
            .frame(height: 50)
            .background(RoundedRectangle(cornerRadius: 25, style: .continuous).fill(theme.field))
            .overlay(RoundedRectangle(cornerRadius: 25, style: .continuous).strokeBorder(theme.fieldBorder, lineWidth: 1))
            Button {
                model.pair(code: code)
            } label: {
                Text(model.pairingBusy ? "Pairing…" : "Pair this iPhone")
                    .font(.system(size: Theme.body, weight: .semibold))
                    .foregroundStyle(theme.onPrimary)
                    .frame(maxWidth: .infinity)
                    .frame(height: 50)
                    .background(Capsule().fill(theme.primary))
            }
            .buttonStyle(.plain)
            .disabled(code.count != 6 || model.pairingBusy)
            .opacity(code.count == 6 && !model.pairingBusy ? 1 : 0.45)
            if let err = model.pairingError {
                Text(err).font(.system(size: Theme.small)).foregroundStyle(theme.danger).multilineTextAlignment(.center)
            }
            Text("This relay uses one-time codes: run `arbos-relay pair-code` on it.")
                .font(.system(size: Theme.tiny)).foregroundStyle(theme.muted).multilineTextAlignment(.center)
        }
        .padding(.horizontal, 24)
    }
}
