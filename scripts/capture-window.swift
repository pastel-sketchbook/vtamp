// Small macOS bridge for capture-site.py. Compiled into its temporary directory.
import AppKit
import CoreGraphics
import Foundation

func output(_ value: Any) {
    let data = try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
    print(String(data: data, encoding: .utf8)!)
}

let args = Array(CommandLine.arguments.dropFirst())
switch args.first {
case "front":
    output(["pid": NSWorkspace.shared.frontmostApplication?.processIdentifier ?? 0])
case "launch":
    let configuration = NSWorkspace.OpenConfiguration()
    configuration.createsNewApplicationInstance = true
    configuration.activates = true
    configuration.arguments = Array(args.dropFirst(2))
    var done = false
    NSWorkspace.shared.openApplication(at: URL(fileURLWithPath: args[1]), configuration: configuration) { app, error in
        if let app = app {
            output(["pid": app.processIdentifier])
        } else {
            output(["error": error?.localizedDescription ?? "Cannot launch Ghostty"])
        }
        done = true
    }
    while !done { RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.05)) }
case "window":
    let pid = Int(args[1])!
    let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
    let matches = windows.filter {
        ($0[kCGWindowOwnerPID as String] as? Int) == pid &&
        ($0[kCGWindowLayer as String] as? Int) == 0
    }
    output(matches.map { ["id": $0[kCGWindowNumber as String]!, "bounds": $0[kCGWindowBounds as String]!] })
case "activate":
    let app = NSRunningApplication(processIdentifier: pid_t(args[1])!)
    output(["ok": app?.activate(options: [.activateIgnoringOtherApps]) ?? false])
default:
    fputs("Expected front, launch, window, or activate\n", stderr)
    exit(2)
}
