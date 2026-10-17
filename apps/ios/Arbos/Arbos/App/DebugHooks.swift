// Simulator-only hooks so a headless `simctl launch` can land on a given
// screen for screenshots. Compiled out of Release builds.

import Foundation

enum DebugHooks {
    static var openProject: Id? { value("ARBOS_OPEN_PROJECT") }
    static var openChat: Id? { value("ARBOS_OPEN_CHAT") }
    static var autoSend: String? { value("ARBOS_AUTO_SEND") }
    static var pairCode: String? { value("ARBOS_PAIR_CODE") }
    static var fixture: Bool { value("ARBOS_FIXTURE") == "1" }
    /// Join a call seen via presence automatically (two-simulator relay tests).
    static var autoJoinCall: Bool { value("ARBOS_AUTO_JOIN") == "1" }
    /// Accept an incoming call automatically (two-simulator relay tests).
    static var autoAcceptCall: Bool { value("ARBOS_AUTO_ACCEPT") == "1" }
    /// Headless simulators have no audio device: WAV files dropped into this directory are sent
    /// as the microphone (LiveKit manual-rendering mode), in file-name order, once each.
    static var micDir: String? { value("ARBOS_MIC_DIR") }
    /// With `ARBOS_MIC_DIR`: also switch the audio engine to manual rendering (no device at all).
    static var micManual: Bool { value("ARBOS_MIC_MANUAL") == "1" }
    /// Join a LiveKit room directly (dev server + locally minted token), bypassing the relay.
    static var callUrl: String? { value("ARBOS_CALL_URL") }
    static var callToken: String? { value("ARBOS_CALL_TOKEN") }
    static var callKind: String? { value("ARBOS_CALL_KIND") }
    /// Simulator has no camera: pretend ours is on and the agent reports seeing it (§11 badge screenshots).
    static var fakeVision: Bool { value("ARBOS_FAKE_VISION") == "1" }
    /// Text to put in the feedback sheet and send as soon as it opens (endpoint check).
    static var autoFeedback: String? { value("ARBOS_AUTO_FEEDBACK") }
    /// Fixture machine reports `always_on` (D73 policy screens).
    static var alwaysOn: Bool { value("ARBOS_ALWAYS_ON") == "1" }
    /// keyboard | settings | voice | threads | model | profile — opened on the chat screen.
    /// `onboarding` shows the D82 first-login screen frozen on `ARBOS_ONBOARDING_STAGE` (1|2|3|fail).
    static var show: String? { value("ARBOS_SHOW") }
    static var onboardingStage: String? { value("ARBOS_ONBOARDING_STAGE") }
    static var onboardingAutoplay: Bool { value("ARBOS_ONBOARDING_AUTOPLAY") == "1" }
    /// With `ARBOS_SHOW=onboarding`: also present the name/handle sheet over the terminal (rig).
    static var onboardingProfile: Bool { value("ARBOS_ONBOARDING_PROFILE") == "1" }
    /// With `ARBOS_SHOW=settings`: the Settings page to land on (`appearance`).
    static var settingsPage: String? { value("ARBOS_SETTINGS_PAGE") }
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
