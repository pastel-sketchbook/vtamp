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
case "verify":
    guard let data = try? Data(contentsOf: URL(fileURLWithPath: args[1])),
          let bitmap = NSBitmapImageRep(data: data) else {
        output(["coverVisible": false]); exit(0)
    }
    // Sample inside the cover, away from text and borders. An upload alone is
    // insufficient: an occluded Ghostty window may not have painted its texture.
    let wide = args[2] == "wide"
    let x0 = wide ? 0.03 : 0.10
    let x1 = wide ? 0.12 : 0.27
    let y0 = wide ? 0.18 : 0.20
    let y1 = wide ? 0.36 : 0.50
    var painted = 0
    for y in 0..<30 {
        for x in 0..<30 {
            let px = Int((x0 + (x1 - x0) * Double(x) / 30) * Double(bitmap.pixelsWide))
            let py = Int((y0 + (y1 - y0) * Double(y) / 30) * Double(bitmap.pixelsHigh))
            if let color = bitmap.colorAt(x: px, y: py)?.usingColorSpace(.deviceRGB) {
                let distance = abs(color.redComponent * 255 - 30) +
                    abs(color.greenComponent * 255 - 30) + abs(color.blueComponent * 255 - 46)
                if distance > 15 { painted += 1 }
            }
        }
    }
    output(["coverVisible": painted > 90])
default:
    fputs("Expected front, launch, window, activate, or verify\n", stderr)
    exit(2)
}
