// App Intents: CleanShotZ actions for the Shortcuts app, Spotlight and
// Siri. Each intent runs a cleanshotz:// request through Rust
// (`cleanshotz_run_request`), the same path as the URL scheme, the
// command line and AppleScript.

import AppIntents
import Foundation

@_silgen_name("cleanshotz_run_request")
private func cleanshotzRunRequest(_ url: UnsafePointer<CChar>) -> UnsafeMutablePointer<CChar>?

@_silgen_name("cleanshotz_free_string")
private func cleanshotzFreeString(_ string: UnsafeMutablePointer<CChar>)

struct RequestError: Error, CustomLocalizedStringResourceConvertible {
    let message: String
    var localizedStringResource: LocalizedStringResource { "\(message)" }
}

/// Runs `command` with `parameters` (unset ones left out) in the app.
@MainActor
private func run(_ command: String, _ parameters: [(String, String?)] = []) throws {
    var components = URLComponents()
    components.scheme = "cleanshotz"
    components.host = command
    let items = parameters.compactMap { name, value in
        value.map { URLQueryItem(name: name, value: $0) }
    }
    if !items.isEmpty {
        components.queryItems = items
    }
    guard let url = components.string else {
        throw RequestError(message: "could not build the request")
    }
    if let error = url.withCString({ cleanshotzRunRequest($0) }) {
        let message = String(cString: error)
        cleanshotzFreeString(error)
        throw RequestError(message: message)
    }
}

/// What to do with a screenshot instead of the usual copy and save
/// settings.
enum AfterCapture: String, AppEnum {
    case copy, save, annotate, upload, pin

    static var typeDisplayRepresentation: TypeDisplayRepresentation { "Action After Capture" }
    static var caseDisplayRepresentations: [AfterCapture: DisplayRepresentation] {
        [
            .copy: "Copy to Clipboard",
            .save: "Save to File",
            .annotate: "Open in Editor",
            .upload: "Upload",
            .pin: "Pin to Screen",
        ]
    }
}

struct CaptureAreaIntent: AppIntent {
    static var title: LocalizedStringResource { "Capture Area" }
    static var description: IntentDescription { "Select an area of the screen and capture it." }

    @Parameter(title: "Then")
    var after: AfterCapture?

    @MainActor
    func perform() async throws -> some IntentResult {
        try run("capture-area", [("action", after?.rawValue)])
        return .result()
    }
}

struct CaptureFullscreenIntent: AppIntent {
    static var title: LocalizedStringResource { "Capture Fullscreen" }
    static var description: IntentDescription { "Capture the display under the pointer." }

    @Parameter(title: "Then")
    var after: AfterCapture?

    @MainActor
    func perform() async throws -> some IntentResult {
        try run("capture-fullscreen", [("action", after?.rawValue)])
        return .result()
    }
}

struct CaptureWindowIntent: AppIntent {
    static var title: LocalizedStringResource { "Capture Window" }
    static var description: IntentDescription { "Pick a window and capture it." }

    @Parameter(title: "Then")
    var after: AfterCapture?

    @MainActor
    func perform() async throws -> some IntentResult {
        try run("capture-window", [("action", after?.rawValue)])
        return .result()
    }
}

struct CapturePreviousAreaIntent: AppIntent {
    static var title: LocalizedStringResource { "Capture Previous Area" }
    static var description: IntentDescription { "Capture the last selected area again." }

    @Parameter(title: "Then")
    var after: AfterCapture?

    @MainActor
    func perform() async throws -> some IntentResult {
        try run("capture-previous-area", [("action", after?.rawValue)])
        return .result()
    }
}

struct CaptureTextIntent: AppIntent {
    static var title: LocalizedStringResource { "Capture Text" }
    static var description: IntentDescription {
        "Select an area and copy the text in it to the clipboard."
    }

    @MainActor
    func perform() async throws -> some IntentResult {
        try run("capture-text")
        return .result()
    }
}

struct ScrollingCaptureIntent: AppIntent {
    static var title: LocalizedStringResource { "Scrolling Capture" }
    static var description: IntentDescription { "Capture content that scrolls, down or sideways." }

    @MainActor
    func perform() async throws -> some IntentResult {
        try run("scrolling-capture")
        return .result()
    }
}

struct RecordScreenIntent: AppIntent {
    static var title: LocalizedStringResource { "Record Screen" }
    static var description: IntentDescription {
        "Start a screen recording, or stop the one in progress."
    }

    @MainActor
    func perform() async throws -> some IntentResult {
        try run("record-screen")
        return .result()
    }
}

struct OpenHistoryIntent: AppIntent {
    static var title: LocalizedStringResource { "Open Capture History" }
    static var description: IntentDescription { "Open the history of captures and recordings." }

    @MainActor
    func perform() async throws -> some IntentResult {
        try run("open-history")
        return .result()
    }
}

struct DesktopIconsIntent: AppIntent {
    static var title: LocalizedStringResource { "Hide Desktop Icons" }
    static var description: IntentDescription {
        "Cover desktop icons and widgets with the wallpaper, or show them again."
    }

    @Parameter(title: "Hidden", default: true)
    var hidden: Bool

    @MainActor
    func perform() async throws -> some IntentResult {
        try run(hidden ? "hide-desktop-icons" : "show-desktop-icons")
        return .result()
    }
}

struct RunPipelineIntent: AppIntent {
    static var title: LocalizedStringResource { "Run Pipeline" }
    static var description: IntentDescription {
        "Run a pipeline from the settings file on an image file."
    }

    @Parameter(title: "Pipeline")
    var name: String

    @Parameter(title: "Image")
    var image: IntentFile

    @MainActor
    func perform() async throws -> some IntentResult {
        // Files from other apps may only exist as data: write those out.
        let path: String
        if let url = image.fileURL, FileManager.default.fileExists(atPath: url.path) {
            path = url.path
        } else {
            let url = FileManager.default.temporaryDirectory
                .appendingPathComponent(image.filename.isEmpty ? "image.png" : image.filename)
            try image.data.write(to: url)
            path = url.path
        }
        try run("run-pipeline", [("name", name), ("filepath", path)])
        return .result()
    }
}

struct CleanShotZShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(
            intent: CaptureAreaIntent(),
            phrases: ["Capture an area with \(.applicationName)"],
            shortTitle: "Capture Area",
            systemImageName: "viewfinder"
        )
        AppShortcut(
            intent: CaptureFullscreenIntent(),
            phrases: ["Capture the screen with \(.applicationName)"],
            shortTitle: "Capture Fullscreen",
            systemImageName: "rectangle.dashed"
        )
        AppShortcut(
            intent: RecordScreenIntent(),
            phrases: ["Record the screen with \(.applicationName)"],
            shortTitle: "Record Screen",
            systemImageName: "record.circle"
        )
        AppShortcut(
            intent: CaptureTextIntent(),
            phrases: ["Capture text with \(.applicationName)"],
            shortTitle: "Capture Text",
            systemImageName: "text.viewfinder"
        )
    }
}
