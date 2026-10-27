// Prints "x y width height" (screen points) of the largest on-screen window owned by the given pid.
import CoreGraphics
import Foundation

let pid = Int32(CommandLine.arguments[1]) ?? 0
let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
let bounds = windows
    .filter { ($0[kCGWindowOwnerPID as String] as? Int32) == pid && ($0[kCGWindowLayer as String] as? Int) == 0 }
    .compactMap { $0[kCGWindowBounds as String] as? [String: Double] }
    .max { ($0["Width"] ?? 0) * ($0["Height"] ?? 0) < ($1["Width"] ?? 0) * ($1["Height"] ?? 0) }
if let b = bounds {
    print(Int(b["X"] ?? 0), Int(b["Y"] ?? 0), Int(b["Width"] ?? 0), Int(b["Height"] ?? 0))
}
