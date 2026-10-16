// Simulator-only hooks so a headless `simctl launch` can land on a given
// screen for screenshots. Compiled out of Release builds.

import Foundation

enum DebugHooks {
    static var openProject: Id? { value("ARBOS_OPEN_PROJECT") }
    static var openChat: Id? { value("ARBOS_OPEN_CHAT") }
    static var autoSend: String? { value("ARBOS_AUTO_SEND") }
    static var pairCode: String? { value("ARBOS_PAIR_CODE") }
    static var fixture: Bool { value("ARBOS_FIXTURE") == "1" }
    /// Accept an incoming call automatically (two-simulator relay tests).
    static var autoAcceptCall: Bool { value("ARBOS_AUTO_ACCEPT") == "1" }
    /// Join a LiveKit room directly (dev server + locally minted token), bypassing the relay.
    static var callUrl: String? { value("ARBOS_CALL_URL") }
    static var callToken: String? { value("ARBOS_CALL_TOKEN") }
    static var callKind: String? { value("ARBOS_CALL_KIND") }
    /// Text to put in the feedback sheet and send as soon as it opens (endpoint check).
    static var autoFeedback: String? { value("ARBOS_AUTO_FEEDBACK") }
    /// Fixture machine reports `always_on` (D73 policy screens).
    static var alwaysOn: Bool { value("ARBOS_ALWAYS_ON") == "1" }
    /// keyboard | settings | voice | threads | model | profile — opened on the chat screen.
    static var show: String? { value("ARBOS_SHOW") }
    /// Scripted tour for the screen recording (fixture only).
    static var demo: Bool { value("ARBOS_DEMO") == "1" }
    /// Scripted tour against the live relay: sends ARBOS_AUTO_SEND, waits for the reply, tours the sheets.
    static var liveDemo: Bool { value("ARBOS_DEMO") == "live" }

    private static func value(_ key: String) -> String? {
        #if DEBUG
        let v = ProcessInfo.processInfo.environment[key] ?? ""
        return v.isEmpty ? nil : v
        #else
        return nil
        #endif
    }
}
