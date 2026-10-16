// Cold-launch timing (#929): milliseconds from process start to the first
// populated home list, logged once so `log show` / Console can read it:
//
//   log show --predicate 'subsystem == "com.unarbos.arbos.ios" AND category == "launch"'

import Foundation
import os

enum LaunchClock {
    private static let log = Logger(subsystem: "com.unarbos.arbos.ios", category: "launch")
    private static let signposter = OSSignposter(subsystem: "com.unarbos.arbos.ios", category: "launch")
    private static var marked = Set<String>()

    /// Kernel-recorded process start, so the number includes dyld and SwiftUI boot.
    static let processStart: Date = {
        var info = kinfo_proc()
        var size = MemoryLayout<kinfo_proc>.stride
        var mib: [Int32] = [CTL_KERN, KERN_PROC, KERN_PROC_PID, getpid()]
        if sysctl(&mib, UInt32(mib.count), &info, &size, nil, 0) == 0 {
            let tv = info.kp_proc.p_starttime
            return Date(timeIntervalSince1970: Double(tv.tv_sec) + Double(tv.tv_usec) / 1_000_000)
        }
        return Date()
    }()

    static func msSinceLaunch() -> Int {
        Int(Date().timeIntervalSince(processStart) * 1000)
    }

    /// Log a milestone once per process (`first-content`, `first-transcript`, …).
    static func mark(_ name: String, detail: String = "") {
        guard !marked.contains(name) else { return }
        marked.insert(name)
        let ms = msSinceLaunch()
        log.notice("\(name, privacy: .public) \(ms) ms \(detail, privacy: .public)")
        signposter.emitEvent("milestone", "\(name, privacy: .public) \(ms, privacy: .public) ms")
    }
}
