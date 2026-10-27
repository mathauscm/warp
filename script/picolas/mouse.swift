// Synthesizes mouse input in screen points: `click x y`, `drag x1 y1 x2 y2`, or `key <char> [cmd]`.
import CoreGraphics
import Foundation

func post(_ type: CGEventType, _ x: Double, _ y: Double) {
    let event = CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: CGPoint(x: x, y: y), mouseButton: .left)
    event?.post(tap: .cghidEventTap)
    usleep(40_000)
}

let args = CommandLine.arguments.dropFirst().map { $0 }
switch args.first {
case "click":
    let x = Double(args[1])!, y = Double(args[2])!
    post(.mouseMoved, x, y)
    post(.leftMouseDown, x, y)
    post(.leftMouseUp, x, y)
case "drag":
    let (x1, y1, x2, y2) = (Double(args[1])!, Double(args[2])!, Double(args[3])!, Double(args[4])!)
    post(.mouseMoved, x1, y1)
    post(.leftMouseDown, x1, y1)
    for step in 1...12 {
        let t = Double(step) / 12
        post(.leftMouseDragged, x1 + (x2 - x1) * t, y1 + (y2 - y1) * t)
    }
    post(.leftMouseUp, x2, y2)
case "key":
    let keyCodes: [String: CGKeyCode] = ["l": 37, "c": 8, "j": 38]
    let code = keyCodes[args[1]] ?? 0
    let flags: CGEventFlags = args.count > 2 && args[2] == "cmd" ? .maskCommand : []
    for down in [true, false] {
        let event = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: down)
        event?.flags = flags
        event?.post(tap: .cghidEventTap)
        usleep(40_000)
    }
default:
    print("usage: mouse.swift click x y | drag x1 y1 x2 y2 | key <l|c|j> [cmd]")
}
