// Prints the CGWindowID of the given pid's main window: the largest normal-layer (0), opaque window,
// preferring on-screen ones. Windows smaller than 200×200 points are skipped, since the app also
// owns small transient windows (like a 500×500 placeholder) that capture blank.
import CoreGraphics
import Foundation

let pid = Int32(CommandLine.arguments[1]) ?? 0
let windows = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]] ?? []

func area(_ window: [String: Any]) -> Double {
    let bounds = window[kCGWindowBounds as String] as? [String: Double] ?? [:]
    return (bounds["Width"] ?? 0) * (bounds["Height"] ?? 0)
}

func isCandidate(_ window: [String: Any]) -> Bool {
    let bounds = window[kCGWindowBounds as String] as? [String: Double] ?? [:]
    return (window[kCGWindowOwnerPID as String] as? Int32) == pid
        && (window[kCGWindowLayer as String] as? Int) == 0
        && (window[kCGWindowAlpha as String] as? Double ?? 1) > 0
        && (bounds["Width"] ?? 0) >= 200
        && (bounds["Height"] ?? 0) >= 200
}

let owned = windows.filter(isCandidate)
let onScreen = owned.filter { ($0[kCGWindowIsOnscreen as String] as? Bool) == true }
let main = (onScreen.isEmpty ? owned : onScreen).max { area($0) < area($1) }
if let id = main?[kCGWindowNumber as String] as? Int { print(id) }
