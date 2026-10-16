// The Grok Bot question card, measured from internal/refs/grokbot-question-card-light.jpg
// (@3x): one rounded card in the assistant-bubble grey (24 pt radius, 14 pt padding),
// a semibold 17.5 pt question, a thin grey × top-right, and a white (dark: slightly lighter)
// inner list (14 pt radius) of 41 pt rows — a 24 pt grey lettered badge, 17 pt text,
// hairline separators. Nothing else: no icons, chips, coloured buttons or hints.
//
// Tapping a row answers the question (`answer { questionId, value }`); × hides the card
// for this session, the composer still answers. An answered card keeps the picked row
// and fades the others.

import SwiftUI

struct QuestionCard: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme

    let rowId: String
    let chatId: Id
    /// nil for a legacy `waiting_user` status line: title only, the composer answers.
    let questionId: String?
    let title: String
    let options: [ChoiceOption]
    let multi: Bool
    /// Picked option ids once answered.
    let picked: [String]
    let open: Bool

    @State private var dismissed = false
    @State private var selected: Set<String> = []

    static let radius: CGFloat = 24
    static let pad: CGFloat = 14
    static let listRadius: CGFloat = 14
    static let rowHeight: CGFloat = 41
    static let badge: CGFloat = 24

    var body: some View {
        if dismissed {
            EmptyView()
        } else {
            VStack(alignment: .leading, spacing: 10) {
                header
                if !options.isEmpty {
                    list
                }
            }
            .padding(.top, Self.pad)
            .padding(.horizontal, Self.pad)
            .padding(.bottom, Self.pad + 1)
            .background(RoundedRectangle(cornerRadius: Self.radius, style: .continuous).fill(palette.outer))
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 10) {
            Text(title)
                .font(.system(size: 17.5, weight: .semibold))
                .foregroundStyle(theme.text)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.leading, 1)
                .frame(maxWidth: .infinity, alignment: .leading)
            if open {
                Button {
                    withAnimation(.easeOut(duration: 0.15)) { dismissed = true }
                } label: {
                    CloseGlyph(color: palette.close)
                        .frame(width: 12, height: 12)
                        .frame(width: 28, height: 22)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .padding(.trailing, -4)
                .accessibilityLabel("Dismiss")
            }
        }
    }

    private var list: some View {
        VStack(spacing: 0) {
            ForEach(Array(options.enumerated()), id: \.element.id) { ix, option in
                if ix > 0 { hairline }
                optionRow(ix: ix, option: option)
            }
            if multi && open {
                hairline
                sendRow
            }
        }
        .background(RoundedRectangle(cornerRadius: Self.listRadius, style: .continuous).fill(palette.inner))
        .clipShape(RoundedRectangle(cornerRadius: Self.listRadius, style: .continuous))
    }

    private var hairline: some View {
        Rectangle().fill(palette.hairline).frame(height: 1 / UIScreen.main.scale)
    }

    private func optionRow(ix: Int, option: ChoiceOption) -> some View {
        let chosen = open ? selected.contains(option.id) : picked.contains(option.id)
        let faded = !open && !chosen
        return Button {
            guard open else { return }
            if multi {
                if selected.remove(option.id) == nil { selected.insert(option.id) }
            } else {
                answer(ids: [option.id], label: option.label)
            }
        } label: {
            HStack(spacing: 11) {
                Text(Self.letter(ix))
                    .font(.system(size: 14))
                    .foregroundStyle(chosen ? palette.inner : palette.badgeInk)
                    .frame(width: Self.badge, height: Self.badge)
                    .background(RoundedRectangle(cornerRadius: 6, style: .continuous).fill(chosen ? theme.text : palette.badge))
                Text(option.label)
                    .font(.system(size: Theme.body))
                    .foregroundStyle(faded ? theme.placeholder : theme.text)
                    .multilineTextAlignment(.leading)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .frame(minHeight: Self.rowHeight)
            .contentShape(Rectangle())
        }
        .buttonStyle(RowPress(enabled: open, fill: palette.press))
        .disabled(!open)
        .accessibilityLabel("\(Self.letter(ix)), \(option.label)")
    }

    private var sendRow: some View {
        Button {
            let labels = options.filter { selected.contains($0.id) }.map(\.label)
            guard !labels.isEmpty else { return }
            answer(ids: options.map(\.id).filter { selected.contains($0) }, label: labels.joined(separator: ", "))
        } label: {
            HStack {
                Text("Send")
                    .font(.system(size: Theme.body, weight: .semibold))
                    .foregroundStyle(selected.isEmpty ? theme.placeholder : theme.text)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 10 + Self.badge + 11)
            .frame(minHeight: Self.rowHeight)
            .contentShape(Rectangle())
        }
        .buttonStyle(RowPress(enabled: !selected.isEmpty, fill: palette.press))
        .disabled(selected.isEmpty)
    }

    private func answer(ids: [String], label: String) {
        guard let questionId else { return }
        let value: Any = multi ? ids : (ids.first ?? "")
        model.answerQuestion(chatId: chatId, questionId: questionId, value: value, label: label)
    }

    static func letter(_ ix: Int) -> String {
        ix < 26 ? String(UnicodeScalar(UInt8(65 + ix))) : String(ix + 1)
    }

    // MARK: Palette

    private struct Palette {
        var outer: Color
        var inner: Color
        var badge: Color
        var badgeInk: Color
        var close: Color
        var hairline: Color
        var press: Color
    }

    private var palette: Palette {
        if theme.isDark {
            return Palette(outer: theme.bubbleAssistant, inner: Color(hex: "#2C2C2C")!, badge: Color(hex: "#3A3A3C")!,
                           badgeInk: Color(hex: "#8E8E93")!, close: Color(hex: "#8E8E93")!,
                           hairline: Color.white.opacity(0.08), press: Color.white.opacity(0.06))
        }
        return Palette(outer: theme.bubbleAssistant, inner: .white, badge: Color(hex: "#F1F1F3")!,
                       badgeInk: Color(hex: "#B4B4B8")!, close: Color(hex: "#B4B4B8")!,
                       hairline: Color.black.opacity(0.06), press: Color.black.opacity(0.04))
    }
}

/// Thin × drawn as two strokes (SF's xmark is heavier than Grok's).
private struct CloseGlyph: View {
    let color: Color

    var body: some View {
        Canvas { ctx, size in
            var path = Path()
            path.move(to: CGPoint(x: 0, y: 0))
            path.addLine(to: CGPoint(x: size.width, y: size.height))
            path.move(to: CGPoint(x: size.width, y: 0))
            path.addLine(to: CGPoint(x: 0, y: size.height))
            ctx.stroke(path, with: .color(color), style: StrokeStyle(lineWidth: 1.4, lineCap: .round))
        }
    }
}

/// Row highlight while pressed; no other affordance.
private struct RowPress: ButtonStyle {
    let enabled: Bool
    let fill: Color

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .background(enabled && configuration.isPressed ? fill : .clear)
    }
}
