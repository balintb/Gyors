import AppKit
import ApplicationServices
import Foundation

/// Mirror of Rust `Effect` enum
enum Effect: Decodable {
    case none
    case hide
    case openPath(String)
    case openUrl(String)
    case copyToClipboard(String)
    case revealInFinder(String)
    case runShell(String)
    /// Plugin-sourced shell command. Swift side shows a
    /// confirmation sheet with exact text before running. See
    /// SECURITY.md
    case confirmRunShell(command: String, pluginId: String)
    case runAppleScript(String)
    case setInput(String)
    case copyImagePng(String)
    case askAi(String)
    case arrangeWindow(String)
    case emptyTrash
    case editNote(String)
    case trashFile(String)
    case startTimer(secs: UInt64, label: String)
    case cancelTimers
    case listTimers
    case showImagePng(String)
    case showText(text: String, label: String, language: String?, editablePath: String?)
    case aiTransform(text: String, instruction: String)
    case askAiThenPipe(prompt: String, instruction: String?, stages: [String])
    case pickDirectory(configKey: String, prompt: String)
    case notification(title: String, body: String?)
    case openInTerminal(String)
    case applyTheme(String)

    private enum TopKey: String, CodingKey {
        case OpenPath, OpenUrl, CopyToClipboard, RevealInFinder
        case RunShell, ConfirmRunShell, RunAppleScript, SetInput, CopyImagePng, ShowImagePng, AskAi
        case ArrangeWindow, EditNote, TrashFile, StartTimer, Notification, ShowText
        case AiTransform, AskAiThenPipe, PickDirectory, OpenInTerminal, ApplyTheme
    }

    private struct ConfirmRunShellPayload: Decodable {
        let command: String
        let pluginId: String

        enum CodingKeys: String, CodingKey {
            case command
            case pluginId = "plugin_id"
        }
    }

    private struct AskAiThenPipePayload: Decodable {
        let prompt: String
        let instruction: String?
        let stages: [String]
    }

    private struct PickDirectoryPayload: Decodable {
        let configKey: String
        let prompt: String

        enum CodingKeys: String, CodingKey {
            case configKey = "config_key"
            case prompt
        }
    }

    private struct ShowTextPayload: Decodable {
        let text: String
        let label: String
        let language: String?
        let editablePath: String?

        enum CodingKeys: String, CodingKey {
            case text, label, language
            case editablePath = "editable_path"
        }
    }

    private struct AiTransformPayload: Decodable {
        let text: String
        let instruction: String
    }

    private struct StartTimerPayload: Decodable {
        let secs: UInt64
        let label: String
    }

    private struct NotificationPayload: Decodable {
        let title: String
        let body: String?
    }

    init(from decoder: Decoder) throws {
        if let single = try? decoder.singleValueContainer().decode(String.self) {
            switch single {
            case "None": self = .none
            case "Hide": self = .hide
            case "EmptyTrash": self = .emptyTrash
            case "CancelTimers": self = .cancelTimers
            case "ListTimers": self = .listTimers
            default: self = .none
            }
            return
        }
        let c = try decoder.container(keyedBy: TopKey.self)
        if let s = try? c.decode(String.self, forKey: .OpenPath) {
            self = .openPath(s); return
        }
        if let s = try? c.decode(String.self, forKey: .OpenUrl) {
            self = .openUrl(s); return
        }
        if let s = try? c.decode(String.self, forKey: .CopyToClipboard) {
            self = .copyToClipboard(s); return
        }
        if let s = try? c.decode(String.self, forKey: .RevealInFinder) {
            self = .revealInFinder(s); return
        }
        if let s = try? c.decode(String.self, forKey: .RunShell) {
            self = .runShell(s); return
        }
        if let p = try? c.decode(ConfirmRunShellPayload.self, forKey: .ConfirmRunShell) {
            self = .confirmRunShell(command: p.command, pluginId: p.pluginId); return
        }
        if let s = try? c.decode(String.self, forKey: .RunAppleScript) {
            self = .runAppleScript(s); return
        }
        if let s = try? c.decode(String.self, forKey: .SetInput) {
            self = .setInput(s); return
        }
        if let s = try? c.decode(String.self, forKey: .CopyImagePng) {
            self = .copyImagePng(s); return
        }
        if let s = try? c.decode(String.self, forKey: .ShowImagePng) {
            self = .showImagePng(s); return
        }
        if let s = try? c.decode(String.self, forKey: .AskAi) {
            self = .askAi(s); return
        }
        if let s = try? c.decode(String.self, forKey: .OpenInTerminal) {
            self = .openInTerminal(s); return
        }
        if let s = try? c.decode(String.self, forKey: .ApplyTheme) {
            self = .applyTheme(s); return
        }
        if let s = try? c.decode(String.self, forKey: .ArrangeWindow) {
            self = .arrangeWindow(s); return
        }
        if let s = try? c.decode(String.self, forKey: .EditNote) {
            self = .editNote(s); return
        }
        if let s = try? c.decode(String.self, forKey: .TrashFile) {
            self = .trashFile(s); return
        }
        if let p = try? c.decode(StartTimerPayload.self, forKey: .StartTimer) {
            self = .startTimer(secs: p.secs, label: p.label); return
        }
        if let n = try? c.decode(NotificationPayload.self, forKey: .Notification) {
            self = .notification(title: n.title, body: n.body); return
        }
        if let t = try? c.decode(ShowTextPayload.self, forKey: .ShowText) {
            self = .showText(
                text: t.text,
                label: t.label,
                language: t.language,
                editablePath: t.editablePath
            )
            return
        }
        if let t = try? c.decode(AiTransformPayload.self, forKey: .AiTransform) {
            self = .aiTransform(text: t.text, instruction: t.instruction); return
        }
        if let p = try? c.decode(AskAiThenPipePayload.self, forKey: .AskAiThenPipe) {
            self = .askAiThenPipe(
                prompt: p.prompt,
                instruction: p.instruction,
                stages: p.stages
            )
            return
        }
        if let p = try? c.decode(PickDirectoryPayload.self, forKey: .PickDirectory) {
            self = .pickDirectory(configKey: p.configKey, prompt: p.prompt); return
        }
        self = .none
    }

    /// Scheme allowlist for `Effect.openUrl`
    ///
    /// `NSWorkspace.shared.open(_:)` cheerfully handles `file:`,
    /// `vnc:`, `smb:`, `tel:`, arbitrary custom schemes, and
    /// `javascript:` in some contexts. A plugin returning an
    /// arbitrary URL string should not be able to mount a share,
    /// dial a number, or open an attacker-controlled file. Limit to
    /// four schemes that have legitimate launcher use: web pages
    /// (`http`/`https`), email (`mailto`), and our own in-app URL
    /// handler (`gyors`)
    ///
    /// Helper takes raw string (not a parsed `URL`) so a caller can
    /// reject empty / malformed input same way as a disallowed
    /// scheme. Scheme comparison is case-insensitive (RFC 3986 Sec 3.1)
    static func isAllowedOpenUrlScheme(_ raw: String) -> Bool {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty,
              let url = URL(string: trimmed),
              let scheme = url.scheme?.lowercased()
        else { return false }
        switch scheme {
        case "http", "https", "mailto", "gyors":
            return true
        default:
            return false
        }
    }
}

enum EffectRunner {
    /// Registered at app launch. Kept out-of-line so effect
    /// dispatch doesn't hardcode a reference to `DirectoryPicker`
    /// (which depends on AppDelegate + NSOpenPanel - neither exists
    /// in test harness)
    static var directoryPickerHandler: ((String, String) -> Void)?

    static func run(_ effect: Effect) {
        switch effect {
        case .none, .hide, .setInput, .askAi, .editNote, .showImagePng, .showText, .aiTransform, .askAiThenPipe:
            // All of these are handled by ViewModel (they keep
            // panel open and mutate its state)
            break
        case .pickDirectory(let configKey, let prompt):
            // Handler is injected by production app (AppDelegate
            // wires DirectoryPicker on launch). Test harness leaves
            // this nil - effect decoding still works, and no tests
            // currently need to exercise picker path
            DispatchQueue.main.async {
                EffectRunner.directoryPickerHandler?(configKey, prompt)
            }
        case .startTimer(let secs, let label):
            DispatchQueue.main.async {
                TimerManager.shared.start(secs: secs, label: label)
            }
        case .cancelTimers:
            DispatchQueue.main.async { TimerManager.shared.cancelAll() }
        case .listTimers:
            DispatchQueue.main.async { TimerManager.shared.showPanel() }
        case .openPath(let path):
            openPath(path)
        case .openUrl(let s):
            // Enforce a scheme allowlist before handing
            // string to NSWorkspace. Reject `file:`, `vnc:`,
            // `javascript:`, custom schemes, etc. Log on reject so
            // rejection is auditable; no alert, since this path
            // fires from provider/plugin code that may have
            // legitimate edge cases worth a heads-up but not a
            // modal interruption
            if Effect.isAllowedOpenUrlScheme(s), let u = URL(string: s) {
                NSWorkspace.shared.open(u)
            } else {
                let scheme = URL(string: s)?.scheme ?? "(none)"
                let snippet = String(s.prefix(80))
                NSLog("gyors: openUrl rejected scheme=%@ url=%@", scheme, snippet)
            }
        case .copyToClipboard(let s):
            let pb = NSPasteboard.general
            pb.clearContents()
            pb.setString(s, forType: .string)
            // Brief menu-bar strobe so user gets visual confirmation
            // even when panel hides instantly after activation
            BusyTracker.shared.flash()
        case .revealInFinder(let path):
            NSWorkspace.shared.selectFile(path, inFileViewerRootedAtPath: "")
        case .runShell(let cmd):
            // Defer slightly so panel has a chance to hide and
            // focus returns to previous app before shell command
            // runs
            deferToNextAppFocus { runShell(cmd) }
        case .confirmRunShell(let cmd, let pluginId):
            // Plugin stdout fed into /bin/sh is the attack
            // surface. Show user exact text + attributed plugin id
            // before running anything. Modal, so a plugin can't
            // flood-confirm by spamming activations
            DispatchQueue.main.async {
                confirmAndRunShell(command: cmd, pluginId: pluginId)
            }
        case .runAppleScript(let source):
            deferToNextAppFocus { runAppleScript(source) }
        case .copyImagePng(let b64):
            copyImagePng(b64)
            BusyTracker.shared.flash()
        case .arrangeWindow(let id):
            // Also deferred so panel hides first and focus returns
            // to previous app before we query
            // `kAXFocusedApplication`
            deferToNextAppFocus { WindowArranger.arrange(geometryId: id) }
        case .emptyTrash:
            deferToNextAppFocus { emptyTrash() }
        case .trashFile(let path):
            trashFile(at: path)
        case .notification(let title, let body):
            NSLog("notification: \(title) - \(body ?? "")")
        case .openInTerminal(let cmd):
            // Defer slightly so panel can hide first - same
            // courtesy as runShell. Otherwise new Terminal window
            // opens behind us
            deferToNextAppFocus { openInTerminal(cmd) }
        case .applyTheme(let id):
            // ThemeManager runs on main actor; runtime dispatch
            // path may be off-actor depending on caller, so hop to
            // main explicitly. `apply(id:)` writes selection
            // through to config.json same way menu-bar picker
            // does - single source of truth
            DispatchQueue.main.async {
                ThemeManager.shared.apply(id: id)
            }
        }
    }

    // Scripts that manipulate windows ("System Events ...") need
    // Gyors to no longer be frontmost, otherwise they target Gyors
    // itself. A small run-loop delay lets panel dismiss and focus
    // return to previous app
    private static func deferToNextAppFocus(_ work: @escaping @Sendable () -> Void) {
        DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(150), execute: work)
    }

    /// Open a path. App bundles take explicit launch path
    /// (`openApplication`) - universal `open(_:)` API can't
    /// statically distinguish "launch this app" from "modify this
    /// app", so on macOS Sequoia (15+) it tripped new App
    /// Management TCC class with a "Gyors was prevented from
    /// modifying apps on your Mac" prompt every time user launched
    /// anything. Documents and folders still go through `open(_:)`
    /// since they dont have that gate
    private static func openPath(_ path: String) {
        let url = URL(fileURLWithPath: path)
        if path.hasSuffix(".app") || path.hasSuffix(".app/") {
            let cfg = NSWorkspace.OpenConfiguration()
            // Activate (bring to front) - matches what users
            // expect when they Enter on an app row
            cfg.activates = true
            NSWorkspace.shared.openApplication(at: url, configuration: cfg) { _, error in
                if let error = error {
                    NSLog("gyors: openApplication failed for %@: %@", path, "\(error)")
                }
            }
        } else {
            NSWorkspace.shared.open(url)
        }
    }

    /// Modal "<plugin> wants to run: <cmd>" alert. Cancel returns
    /// without running. Run defers + executes same way a trusted
    /// `RunShell` does. We never cache "always allow" - every
    /// shell-mode activation re-prompts, because a plugin can
    /// change its stdout silently after a self-update
    private static func confirmAndRunShell(command: String, pluginId: String) {
        NSApp.activate(ignoringOtherApps: true)
        let alert = NSAlert()
        alert.messageText = "Run shell command from plugin?"
        alert.informativeText = """
            Plugin: \(pluginId)

            Will execute:
            \(command)

            Shell-mode plugins can change their output at any time. \
            Only run commands you trust.
            """
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Run")
        alert.addButton(withTitle: "Cancel")
        // Default selection is second button (Cancel) - safer than
        // Enter immediately confirming
        if let first = alert.buttons.first, let second = alert.buttons.dropFirst().first {
            first.keyEquivalent = ""
            second.keyEquivalent = "\r"
        }
        let resp = alert.runModal()
        guard resp == .alertFirstButtonReturn else { return }
        deferToNextAppFocus { runShell(command) }
    }

    private static func runShell(_ cmd: String) {
        let task = Process()
        // Honor user's login shell so aliases, functions, and rc
        // files resolve the way they do in a terminal. Fall back
        // to /bin/sh if $SHELL is unset or points at something
        // that isn't executable
        let shell = ProcessInfo.processInfo.environment["SHELL"]
        if let shell = shell, FileManager.default.isExecutableFile(atPath: shell) {
            task.launchPath = shell
            task.arguments = ["-i", "-c", cmd]
        } else {
            task.launchPath = "/bin/sh"
            task.arguments = ["-c", cmd]
        }
        do {
            try task.run()
        } catch {
            NSLog("gyors: runShell failed: \(error)")
            showErrorAlert(title: "Shell command failed", message: "\(error)")
        }
    }

    /// Run `cmd` in a fresh window of user's preferred terminal
    /// (defaults to Terminal.app). Implemented as a temp `.command`
    /// file opened via Launch Services - no Apple Events, no
    /// Automation TCC prompt, no `tell application`
    ///
    /// `.command` extension is the magic bit: macOS knows to hand
    /// `.command` files to Terminal-like apps, which then exec them
    /// in a new window. iTerm2, Ghostty, WezTerm, Alacritty, and
    /// kitty all honor this convention too
    ///
    /// We chmod 700 + write to `$TMPDIR` so file is readable only
    /// by running user. A delayed cleanup nukes it after 30s - long
    /// enough for Terminal to spawn, short enough that it doesn't
    /// accumulate
    private static func openInTerminal(_ cmd: String) {
        let cfg = Config.load()
        let app = cfg.effectiveTerminalApp
        let tmp = NSTemporaryDirectory() as NSString
        let path = tmp.appendingPathComponent(
            "gyors-\(UUID().uuidString).command"
        )
        // Single shebang + a `cd $HOME` so new shell starts where
        // user expects, not in $TMPDIR
        let body = "#!/bin/sh\ncd \"$HOME\"\n\(cmd)\n"
        do {
            try body.write(toFile: path, atomically: true, encoding: .utf8)
            try FileManager.default.setAttributes(
                [.posixPermissions: 0o700],
                ofItemAtPath: path
            )
        } catch {
            NSLog("gyors: openInTerminal: failed to stage command file: \(error)")
            showErrorAlert(
                title: "Couldn't open terminal",
                message: "Failed to stage temporary command file.\n\n\(error)"
            )
            return
        }

        // `open -a <App> <file>` is Launch Services. No Apple
        // Events, no Automation prompt. Wait for `open` to finish
        // so we can detect "app couldn't be found" exit code (1)
        // and fall back to Terminal - `task.run()` only throws if
        // process can't START, so a non-zero exit silently flies
        // past unless we check `terminationStatus`
        if !runOpenAndAwait(app: app, path: path) {
            NSLog("gyors: openInTerminal: open -a \(app) failed - falling back to Terminal")
            if !runOpenAndAwait(app: "Terminal", path: path) {
                showErrorAlert(
                    title: "Couldn't open terminal",
                    message: "Tried `open -a \(app)` and `open -a Terminal`, both refused. Is the configured terminal app installed?"
                )
            }
        }
        // Fire-and-forget cleanup. 30s is generous for any
        // terminal app to read file before we delete it
        DispatchQueue.main.asyncAfter(deadline: .now() + 30) {
            try? FileManager.default.removeItem(atPath: path)
        }
    }

    /// Run `/usr/bin/open -a <app> <path>` synchronously. Returns
    /// true on exit code 0, false on anything else (including
    /// process-spawn failures and `open`'s "app not found" 1).
    /// Bounded wait: most `open` invocations complete in <250ms; we
    /// cap at 5s so a hung Launch Services dialog doesn't freeze
    /// caller forever
    private static func runOpenAndAwait(app: String, path: String) -> Bool {
        let task = Process()
        task.launchPath = "/usr/bin/open"
        task.arguments = ["-a", app, path]
        do {
            try task.run()
        } catch {
            NSLog("gyors: open -a spawn failed: \(error)")
            return false
        }
        // Bounded wait. `open` exits as soon as it's handed file
        // off to Launch Services - it doesn't block on target app
        // finishing
        let deadline = Date().addingTimeInterval(5)
        while task.isRunning && Date() < deadline {
            RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.05))
        }
        if task.isRunning {
            task.terminate()
            return false
        }
        return task.terminationStatus == 0
    }

    private static func runAppleScript(_ source: String) {
        // System-Events scripts need Accessibility permission.
        // Check first so failures show a helpful prompt instead of
        // failing silently
        if source.contains("System Events") && !AXIsProcessTrusted() {
            Accessibility.promptUserIfNeeded(
                reason: "Gyors uses Accessibility to manipulate windows for you (`wm …`). Grant access in System Settings → Privacy & Security → Accessibility."
            )
            return
        }
        var errorInfo: NSDictionary?
        let script = NSAppleScript(source: source)
        _ = script?.executeAndReturnError(&errorInfo)
        if let errorInfo = errorInfo {
            let msg = errorInfo[NSAppleScript.errorMessage] as? String
                ?? errorInfo[NSAppleScript.errorBriefMessage] as? String
                ?? "\(errorInfo)"
            NSLog("gyors: runAppleScript error: \(msg)")
            showErrorAlert(title: "AppleScript failed", message: msg)
        }
    }

    private static func emptyTrash() {
        // Strategy: try FileManager first. If TCC blocks it
        // (typical for third-party apps without Full Disk Access),
        // fall back to AppleScript `tell Finder`, which uses
        // Automation permission and routes deletion through Finder
        // itself
        if emptyTrashViaFileManager() { return }
        emptyTrashViaFinderAppleScript()
    }

    /// Returns true if attempt succeeded (or trash was empty).
    /// Returns false when TCC denies access, signalling caller to
    /// try AppleScript fallback
    private static func emptyTrashViaFileManager() -> Bool {
        let fm = FileManager.default
        let trash = fm.homeDirectoryForCurrentUser
            .appendingPathComponent(".Trash", isDirectory: true)

        let items: [URL]
        do {
            items = try fm.contentsOfDirectory(at: trash, includingPropertiesForKeys: nil, options: [])
        } catch {
            let ns = error as NSError
            // Permission-denied (NSCocoaErrorDomain 257) -> signal fallback
            if ns.domain == NSCocoaErrorDomain && ns.code == 257 {
                NSLog("gyors: FileManager blocked by TCC, falling back to AppleScript: \(error)")
                return false
            }
            NSLog("gyors: listing ~/.Trash failed: \(error)")
            showTrashFallbackAlert(reason: error.localizedDescription)
            return true
        }

        var removed = 0
        var failed: [(URL, Error)] = []
        for item in items {
            do { try fm.removeItem(at: item); removed += 1 }
            catch { failed.append((item, error)) }
        }
        if !failed.isEmpty {
            let sample = failed.prefix(3)
                .map { "• \($0.0.lastPathComponent): \($0.1.localizedDescription)" }
                .joined(separator: "\n")
            let more = failed.count > 3 ? "\n… and \(failed.count - 3) more" : ""
            showErrorAlert(
                title: "Empty Trash: \(removed) removed, \(failed.count) failed",
                message: "\(sample)\(more)"
            )
        }
        NSLog("gyors: emptied trash via FileManager, removed=\(removed), failed=\(failed.count)")
        return true
    }

    private static func emptyTrashViaFinderAppleScript() {
        let src = #"tell application "Finder" to empty trash"#
        var errorInfo: NSDictionary?
        let script = NSAppleScript(source: src)
        _ = script?.executeAndReturnError(&errorInfo)
        guard let errorInfo = errorInfo else {
            NSLog("gyors: emptied trash via AppleScript")
            return
        }
        let code = (errorInfo[NSAppleScript.errorNumber] as? NSNumber)?.intValue ?? 0
        let msg = (errorInfo[NSAppleScript.errorMessage] as? String)
            ?? (errorInfo[NSAppleScript.errorBriefMessage] as? String)
            ?? "\(errorInfo)"
        NSLog("gyors: AppleScript empty-trash failed (code=\(code)): \(msg)")
        // -1743 = not authorised to send Apple events. Anything
        // else is worth surfacing too, but with same remediation
        // path
        showTrashFallbackAlert(reason: msg)
    }

    /// Explain TCC situation and offer a button that jumps to Full
    /// Disk Access, which fixes both FileManager path (directly)
    /// and tends to unblock Finder automation on future launches
    private static func showTrashFallbackAlert(reason: String) {
        DispatchQueue.main.async {
            NSApp.activate(ignoringOtherApps: true)
            let alert = NSAlert()
            alert.messageText = "Empty Trash needs permission"
            alert.informativeText = """
                macOS blocks third-party apps from emptying the Trash without either \
                Full Disk Access or Finder Automation consent.

                Fix it once:
                System Settings → Privacy & Security → Full Disk Access → + → Gyors.app

                Details: \(reason)
                """
            alert.alertStyle = .warning
            alert.addButton(withTitle: "Open Full Disk Access")
            alert.addButton(withTitle: "Cancel")
            let choice = alert.runModal()
            if choice == .alertFirstButtonReturn,
               let url = URL(
                   string: "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles"
               )
            {
                NSWorkspace.shared.open(url)
            }
        }
    }



    /// Move a single file to user's Trash via
    /// `FileManager.trashItem`. Undoable from Finder, doesn't need
    /// Automation permission
    private static func trashFile(at path: String) {
        let url = URL(fileURLWithPath: path)
        do {
            try FileManager.default.trashItem(at: url, resultingItemURL: nil)
            NSLog("gyors: trashed \(path)")
        } catch {
            NSLog("gyors: trashItem failed: \(error)")
            showErrorAlert(
                title: "Couldn't move to Trash",
                message: "\(path)\n\n\(error.localizedDescription)"
            )
        }
    }

    private static func copyImagePng(_ base64: String) {
        guard let data = Data(base64Encoded: base64),
              let image = NSImage(data: data) else {
            NSLog("gyors: copyImagePng failed to decode")
            return
        }
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.writeObjects([image])
    }

    private static func showErrorAlert(title: String, message: String) {
        DispatchQueue.main.async {
            NSApp.activate(ignoringOtherApps: true)
            let alert = NSAlert()
            alert.messageText = title
            alert.informativeText = message
            alert.alertStyle = .warning
            alert.addButton(withTitle: "OK")
            alert.runModal()
        }
    }
}

/// Accessibility permission helpers
enum Accessibility {
    static func isTrusted() -> Bool {
        AXIsProcessTrusted()
    }

    /// Shows an alert explaining why Accessibility is needed and
    /// offers to trigger system prompt + open System Settings
    static func promptUserIfNeeded(reason: String) {
        if isTrusted() { return }
        DispatchQueue.main.async {
            NSApp.activate(ignoringOtherApps: true)
            let alert = NSAlert()
            alert.messageText = "Accessibility permission required"
            alert.informativeText = reason
            alert.alertStyle = .informational
            alert.addButton(withTitle: "Open System Settings")
            alert.addButton(withTitle: "Cancel")
            let choice = alert.runModal()
            if choice == .alertFirstButtonReturn {
                // Ask macOS to prompt (no-op if user has already
                // denied), and open relevant pane so they can tick
                // checkbox
                let opts: NSDictionary = [
                    kAXTrustedCheckOptionPrompt.takeRetainedValue() as String: true
                ]
                _ = AXIsProcessTrustedWithOptions(opts)
                if let url = URL(
                    string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
                ) {
                    NSWorkspace.shared.open(url)
                }
            }
        }
    }
}
