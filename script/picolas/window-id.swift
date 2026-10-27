// Prints the CGWindowID of the largest on-screen window owned by the given pid.
import CoreGraphics
import Foundation

let pid = Int32(CommandLine.arguments[1]) ?? 0
let windows = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]] ?? []
let owned = windows.filter { ($0[kCGWindowOwnerPID as String] as? Int32) == pid && ($0[kCGWindowLayer as String] as? Int) == 0 }
let largest = owned.max { a, b in
    let area = { (w: [String: Any]) -> Double in
        let bounds = w[kCGWindowBounds as String] as? [String: Double] ?? [:]
        return (bounds["Width"] ?? 0) * (bounds["Height"] ?? 0)
    }
    return area(a) < area(b)
}
if let id = largest?[kCGWindowNumber as String] as? Int { print(id) }
