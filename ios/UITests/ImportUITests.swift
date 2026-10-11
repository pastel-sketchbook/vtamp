import UIKit
import XCTest

/// Requires an isolated server with 65 synthetic completed jobs named
/// "Import history 001" ... "Import history 065" and fake download tools.
/// Never point this at a personal listening server.
final class ImportUITests: XCTestCase {
    private var server = ""

    override func setUpWithError() throws {
        continueAfterFailure = false
        guard let server = ProcessInfo.processInfo.environment["VTAMP_UITEST_IMPORT_SERVER"], !server.isEmpty else {
            throw XCTSkip("Set TEST_RUNNER_VTAMP_UITEST_IMPORT_SERVER to the isolated import fixture server")
        }
        self.server = server
    }

    @MainActor
    private func launch(dark: Bool = false, large: Bool = false) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchEnvironment["VTAMP_SERVER"] = server
        app.launchEnvironment["VTAMP_MUTED"] = "1"
        XCUIDevice.shared.appearance = dark ? .dark : .light
        if large { app.launchArguments += ["-UIPreferredContentSizeCategoryName", "UICTContentSizeCategoryAccessibilityXXXL"] }
        app.launch()
        let tab = app.tabBars.buttons["Import"]
        XCTAssertTrue(tab.waitForExistence(timeout: 15))
        tab.tap()
        XCTAssertTrue(app.textFields["importURL"].waitForExistence(timeout: 5))
        return app
    }

    @MainActor
    func testFormConsentNativePasteAndHistoryPages() throws {
        let app = launch()
        let url = app.textFields["importURL"]
        let video = app.switches["importVideo"]
        XCTAssertEqual(video.value as? String, "0")
        XCTAssertFalse(app.buttons["Paste"].exists, "Paste belongs to the native text edit menu")
        snapshot(app, "import-light-empty")

        // SwiftUI exposes the entire labeled row as a Switch; tap the thumb.
        video.coordinate(withNormalizedOffset: CGVector(dx: 0.92, dy: 0.5)).tap()
        XCTAssertEqual(video.value as? String, "1")
        url.tap()
        url.typeText("invalid-url")
        let submit = app.collectionViews.buttons["Import"]
        submit.tap()
        // An invalid source is rejected before a job starts; retain the draft.
        XCTAssertTrue(submit.waitForExistence(timeout: 5))
        XCTAssertEqual(url.value as? String, "invalid-url")
        XCTAssertEqual(video.value as? String, "1")

        url.tap()
        url.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: "invalid-url".count))
        let source = "https://www.youtube.com/watch?v=lO3lG-qXU14"
        UIPasteboard.general.string = source
        url.press(forDuration: 1.2)
        let paste = app.menuItems["Paste"].exists ? app.menuItems["Paste"] : app.buttons["Paste"]
        XCTAssertTrue(paste.waitForExistence(timeout: 5))
        paste.tap()
        let allowPaste = app.alerts.buttons["Allow Paste"]
        if allowPaste.waitForExistence(timeout: 2) { allowPaste.tap() }
        XCTAssertEqual(url.value as? String, source)
        submit.tap()
        let reset = NSPredicate(format: "value == '0'")
        expectation(for: reset, evaluatedWith: video)
        waitForExpectations(timeout: 10)
        XCTAssertNotEqual(url.value as? String, source)
        snapshot(app, "import-light-submitted")

        let oldest = app.staticTexts["Import history 001"]
        XCTAssertFalse(oldest.exists, "The first page does not render the entire history")
        for _ in 0..<25 {
            if oldest.isHittable { break }
            app.swipeUp()
        }
        XCTAssertTrue(oldest.isHittable, "Scrolling reaches the fourth history page")
        snapshot(app, "import-history-last-page")
    }

    @MainActor
    func testDarkAppearanceAndLargeText() throws {
        defer { XCUIDevice.shared.appearance = .light }
        let app = launch(dark: true, large: true)
        XCTAssertTrue(app.switches["importVideo"].isHittable)
        snapshot(app, "import-dark-large-text")
    }

    @MainActor
    private func snapshot(_ app: XCUIApplication, _ name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
