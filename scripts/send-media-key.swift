// Test helper only. The player itself never monitors or synthesizes keyboard input.
import AppKit
let codes = ["toggle": 16, "next": 17, "previous": 18]
guard CommandLine.arguments.count == 2,
      let code = codes[CommandLine.arguments[1]] else {
    fputs("Usage: send-media-key toggle|next|previous\n", stderr)
    exit(2)
}
guard CGPreflightPostEventAccess() else {
    fputs("System key test needs Accessibility permission for the invoking terminal. vtamp itself does not.\n", stderr)
    exit(1)
}
for flag in [0xA00, 0xB00] { // key down, key up; NX_SUBTYPE_AUX_CONTROL_BUTTONS
    guard let event = NSEvent.otherEvent(with: .systemDefined, location: .zero,
        modifierFlags: [], timestamp: 0, windowNumber: 0, context: nil,
        subtype: 8, data1: (code << 16) | flag, data2: -1)?.cgEvent else { exit(1) }
    event.post(tap: .cghidEventTap)
}
// Posting crosses into the window server; allow delivery before this helper exits.
RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.25))
