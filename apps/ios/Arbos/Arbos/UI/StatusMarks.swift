// Status marks that are not coloured dots (Jacob, 2026-10-27, [feedback
// neon-dots-ios-2026-10-27]: "replace all neon glimmer icons like this in the
// app with braille spinners always, on a matter of principle never use colored
// dots"). Activity → the braille spinner (`WorkingSpinner`); unread → the count
// pill (`UnreadBadge`); a fault → the red "!" glyph; a resting state (awake,
// asleep, read) → nothing. `StatusDotLintTests` refuses a new small
// `Circle().fill(<status colour>)` anywhere in the app. Mirrors the Mac's
// `ui/marks.rs`.

import SwiftUI

/// What a state shows instead of its old coloured dot.
enum StatusMark: Equatable {
    /// Something is happening: the braille spinner.
    case working
    /// Something is wrong: the red exclamation glyph.
    case fault
}

/// The machine's mark: the spinner while it wakes, the fault glyph on error,
/// nothing at rest (awake, hibernated, stopped).
func machineMark(_ state: BoxState) -> StatusMark? {
    switch state {
    case .waking: return .working
    case .error: return .fault
    case .awake, .hibernatedLight, .hibernatedDeep, .stoppedByUser: return nil
    }
}

/// The red exclamation glyph at `size` points — the error note's mark, on its own.
struct FaultGlyph: View {
    @Environment(\.theme) private var theme
    var size: CGFloat = 11

    var body: some View {
        Image(systemName: "exclamationmark")
            .font(.system(size: size, weight: .bold))
            .foregroundStyle(theme.danger)
            .frame(width: size, height: size)
            .accessibilityLabel("Problem")
    }
}

/// A mark drawn in a slot of `size` points: the spinner or the fault glyph.
struct StatusMarkView: View {
    let mark: StatusMark
    var size: CGFloat = WorkingSpinnerFrames.cell

    var body: some View {
        switch mark {
        case .working:
            WorkingSpinner(size: size, glyphSize: min(size * 1.3, WorkingSpinnerFrames.glyphSize))
        case .fault:
            FaultGlyph(size: size)
        }
    }
}

/// The badge at an avatar's bottom-right corner (where the machine dot sat):
/// the mark on a disc of the page colour, or nothing when `mark` is nil.
struct AvatarCornerMark: View {
    @Environment(\.theme) private var theme
    let mark: StatusMark?
    /// The avatar's size; the badge scales like Grok's 12-on-48 status badge.
    let avatarSize: CGFloat

    var body: some View {
        if let mark {
            let d = min(avatarSize * 0.36, 14)
            ZStack {
                Circle().fill(theme.bg)
                StatusMarkView(mark: mark, size: d * 0.7)
            }
            .frame(width: d, height: d)
            .offset(x: avatarSize * 0.08, y: avatarSize * 0.08)
        }
    }
}

/// The unread mark for a row: the count pill, "1" when only the flag is lit.
struct UnreadMark: View {
    let count: Int

    var body: some View {
        UnreadBadge(count: max(1, count))
    }
}
