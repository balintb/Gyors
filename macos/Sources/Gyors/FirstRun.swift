import AppKit
import Foundation

/// One-time welcome flow. Shown the very first launch (no marker
/// file present), never again. Marker lives alongside rest of
/// Gyors's config so that wiping
/// `~/Library/Application Support/Gyors/` is enough to re-trigger
/// it for testing
enum FirstRun {
    private static let markerName = "first-run-completed"

    static func runIfNeeded(hotkeyLabel: String) {
        let marker = markerURL()
        guard !FileManager.default.fileExists(atPath: marker.path) else { return }
        // Dispatch so AppDelegate's init finishes before we block
        // main thread on a modal
        DispatchQueue.main.async {
            showWelcomePanel(hotkeyLabel: hotkeyLabel)
            try? Data().write(to: marker, options: [.atomic])
        }
    }

    /// Exposed for `Show First-Run Welcome...` menu item so users
    /// can rewatch tour without deleting support files
    static func showManually(hotkeyLabel: String) {
        showWelcomePanel(hotkeyLabel: hotkeyLabel)
    }

    private static func showWelcomePanel(hotkeyLabel: String) {
        NSApp.activate(ignoringOtherApps: true)
        let alert = NSAlert()
        alert.messageText = "Welcome to Gyors"
        alert.informativeText = """
            Press \(prettyHotkey(hotkeyLabel)) anywhere to open Gyors.

            Things to try:
              • type to search apps, files, clipboard history
              • `calc 12 * 7` · `100 km to mi` · `100 usd to eur`
              • `note` · `#my note` · `newnote title` · `n work/`
              • `timer 25m focus` · `tz tokyo` · `re \\d+ :: 12 34`
              • `ssh`, `tab`, `recent`, `json {…}`, `yaml2json`

            Gyors needs two one-time permissions for window ops and
            clipboard/trash actions. Grant them when prompted, or
            from the menu bar cmd → "Request Finder Automation Access".

            This welcome shows once. Re-open it any time from the
            menu bar.
            """
        alert.alertStyle = .informational
        alert.addButton(withTitle: "Got it")
        alert.addButton(withTitle: "Open Config File")
        let choice = alert.runModal()
        if choice == .alertSecondButtonReturn {
            NSWorkspace.shared.open(Config.configURL())
        }
    }

    /// Normalize raw hotkey string to canonical `opt+shift+space`
    /// form for welcome message. We render keys as text (cmd, opt,
    /// shift, ctrl, return, esc, tab, ...) rather than Unicode key
    /// glyphs, so welcome reads naturally and stays grep-able
    static func prettyHotkey(_ raw: String) -> String {
        let tokens = raw.lowercased()
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: " ", with: "+")
            .split(separator: "+")
            .map { String($0) }
        return tokens.map(canonicalKey).joined(separator: "+")
    }

    private static func canonicalKey(_ t: String) -> String {
        switch t {
        case "cmd", "command": return "cmd"
        case "opt", "option", "alt": return "opt"
        case "ctrl", "control": return "ctrl"
        case "shift": return "shift"
        case "return", "enter": return "return"
        case "escape", "esc": return "esc"
        case "tab": return "tab"
        case "space": return "space"
        default: return t
        }
    }

    private static func markerURL() -> URL {
        let base: URL
        if let support = FileManager.default.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first {
            base = support.appendingPathComponent("Gyors", isDirectory: true)
        } else {
            base = URL(fileURLWithPath: NSHomeDirectory())
                .appendingPathComponent("Library/Application Support/Gyors", isDirectory: true)
        }
        try? FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        return base.appendingPathComponent(markerName)
    }
}
