import AppKit
import Foundation
import QuickLookUI

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private let bridge = GyorsBridge()
    private lazy var panel = PanelController(bridge: bridge)
    private var hotkey: Hotkey?
    private var menuBar: MenuBar?
    private var pasteboardWatcher: PasteboardWatcher?
    /// Lazily constructed because `Updater.init` boots
    /// Sparkle's background scheduler on SPARKLE build. We only
    /// want that to happen post-launch, after AppKit is up
    private var updater: Updater?

    func application(_ application: NSApplication, open urls: [URL]) {
        for url in urls {
            Task { @MainActor in
                if url.isFileURL {
                    handleDroppedFile(url)
                } else {
                    handleGyorsUrl(url)
                }
            }
        }
    }

    /// `.gyorsplugin` files: double-click in Finder, drag onto app
    /// icon, or "open with" from any browser's downloads menu. Reads
    /// manifest JSON + hands it to install panel - same rich preview
    /// `gyors://plugin/install` URI triggers
    @MainActor
    private func handleDroppedFile(_ url: URL) {
        let ext = url.pathExtension.lowercased()
        guard ext == "gyorsplugin" || ext == "json" else { return }
        guard let data = try? Data(contentsOf: url),
              let text = String(data: data, encoding: .utf8)
        else {
            let alert = NSAlert()
            alert.messageText = "Couldn't read \(url.lastPathComponent)"
            alert.informativeText = "File could not be decoded as UTF-8 text."
            alert.alertStyle = .warning
            NSApp.activate(ignoringOtherApps: true)
            alert.runModal()
            return
        }
        PluginInstallPanel.present(specJson: text)
    }

    @MainActor
    private func handleGyorsUrl(_ url: URL) {
        guard url.scheme == "gyors" else { return }
        switch GyorsUrlHandler.parse(url) {
        case .theme:
            importTheme(url)
        case .set(let setting, let value):
            applySettingWithConfirmation(setting, value: value)
        case .toggle(let setting):
            toggleSettingWithConfirmation(setting)
        case .toggleBool(let jsonKey, let displayName):
            toggleBoolKeyWithConfirmation(jsonKey: jsonKey, displayName: displayName)
        case .setBool(let jsonKey, let displayName, let value):
            setBoolKeyWithConfirmation(
                jsonKey: jsonKey,
                displayName: displayName,
                value: value
            )
        case .openWithQuery(let text):
            panel.show(withQuery: text)
        case .pluginInstall(let specJson):
            PluginInstallPanel.present(specJson: specJson)
        case .invalid(let reason):
            showAlert(title: "Couldn't handle gyors:// URL", message: reason, style: .warning)
        }
    }

    @MainActor
    private func importTheme(_ url: URL) {
        switch ThemeImporter.importFromURL(url) {
        case .success(let theme):
            ThemeManager.shared.reloadCustomThemes()
            ThemeManager.shared.apply(theme)
            menuBar?.rebuildMenu()
            showAlert(
                title: "Theme imported",
                message: "\"\(theme.label)\" is now active."
            )
        case .failure(let err):
            showAlert(
                title: "Couldn't import theme",
                message: err.localizedDescription,
                style: .warning
            )
        }
    }

    @MainActor
    private func applySettingWithConfirmation(
        _ setting: GyorsUrlHandler.Setting,
        value: String
    ) {
        let alert = NSAlert()
        alert.messageText = "Allow Gyors to change a setting?"
        alert.informativeText = "A link is asking to set \(setting.displayName) to:\n\n\(value)"
        alert.alertStyle = .informational
        alert.addButton(withTitle: "Allow")
        alert.addButton(withTitle: "Cancel")
        NSApp.activate(ignoringOtherApps: true)
        guard alert.runModal() == .alertFirstButtonReturn else { return }

        switch setting {
        case .theme:
            if let theme = Themes.byId(value)
                ?? ThemeManager.shared.customThemes.first(where: { $0.id == value })
            {
                ThemeManager.shared.apply(theme)
                menuBar?.rebuildMenu()
                showAlert(title: "Theme set", message: "\"\(theme.label)\" is now active.")
            } else {
                showAlert(
                    title: "Unknown theme",
                    message: "No theme with id \"\(value)\".",
                    style: .warning
                )
            }
        case .hotkey:
            guard HotkeyBinding.parse(value) != nil else {
                showAlert(
                    title: "Invalid hotkey",
                    message: "\"\(value)\" isn't a recognised key combo.",
                    style: .warning
                )
                return
            }
            ConfigWriter.setKey("hotkey", value: value)
            showAlert(
                title: "Hotkey saved",
                message: "Restart Gyors for \"\(value)\" to take effect."
            )
        case .notesFolder:
            let expanded = (value as NSString).expandingTildeInPath
            applyNotesFolderChange(to: expanded)
        case .clipboardEnabled:
            // Boolean settings should go via /toggle. Accept "true"/"false"
            // strings as a convenience for link-builders
            let normalized = value.lowercased()
            let newValue: Bool? = switch normalized {
            case "true", "1", "on": .some(true)
            case "false", "0", "off": .some(false)
            default: .none
            }
            guard let v = newValue else {
                showAlert(
                    title: "Invalid value",
                    message: "Use true/false/on/off for clipboard-enabled.",
                    style: .warning
                )
                return
            }
            ConfigWriter.setKey("clipboard_enabled", value: v)
            showAlert(
                title: "Clipboard history \(v ? "enabled" : "disabled")",
                message: "Restart Gyors to apply."
            )
        }
    }

    /// Generic bool-toggle handler for schema-driven settings (any
    /// `FIELDS` entry of type Bool that doesn't have a custom apply
    /// case). Confirmation-gated like everything else
    @MainActor
    private func setBoolKeyWithConfirmation(
        jsonKey: String,
        displayName: String,
        value: Bool
    ) {
        let alert = NSAlert()
        alert.messageText = "Set \(displayName)?"
        alert.informativeText = "Change \(jsonKey) to \(value ? "true" : "false")?"
        alert.alertStyle = .informational
        alert.addButton(withTitle: value ? "Turn On" : "Turn Off")
        alert.addButton(withTitle: "Cancel")
        NSApp.activate(ignoringOtherApps: true)
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        ConfigWriter.setKey(jsonKey, value: value)
        showAlert(
            title: "\(displayName) \(value ? "on" : "off")",
            message: "Saved to config.json."
        )
    }

    @MainActor
    private func toggleBoolKeyWithConfirmation(jsonKey: String, displayName: String) {
        let current = ConfigWriter.readBool(jsonKey, default: true)
        let proposed = !current
        let alert = NSAlert()
        alert.messageText = "Toggle \(displayName)?"
        alert.informativeText = "Currently \(current ? "on" : "off"). Change to \(proposed ? "on" : "off")?"
        alert.alertStyle = .informational
        alert.addButton(withTitle: proposed ? "Turn On" : "Turn Off")
        alert.addButton(withTitle: "Cancel")
        NSApp.activate(ignoringOtherApps: true)
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        ConfigWriter.setKey(jsonKey, value: proposed)
        showAlert(
            title: "\(displayName) \(proposed ? "on" : "off")",
            message: "Saved to config.json."
        )
    }

    /// Move notes folder setting to `newPath` and, based on what's at
    /// destination, either offer to migrate existing notes or warn
    /// user that current notes will be stranded
    ///
    /// Three states at destination:
    ///   - doesn't exist yet / exists but empty -> offer to move old
    ///     folder's .md files across, preserving filenames and
    ///     subfolder structure.
    ///   - exists with notes inside -> save config but surface a
    ///     warning: new folder will be indexed, old one stays where
    ///     it is, files are NOT merged.
    ///   - path is a regular file / unreachable -> reject, keep old
    ///     config; migrating into a broken target would lose notes.
    /// Exposed so `DirectoryPicker` can reuse migration flow after
    /// NSOpenPanel selection
    @MainActor
    func performNotesFolderChange(to newPath: String) {
        applyNotesFolderChange(to: newPath)
    }

    @MainActor
    private func applyNotesFolderChange(to newPath: String) {
        let oldPath = (Config.load().notesFolder ?? "")
            .isEmpty ? nil : (Config.load().notesFolder as NSString?)?.expandingTildeInPath
        if let oldPath = oldPath, oldPath == newPath {
            showAlert(
                title: "Notes folder unchanged",
                message: "Already pointing at \(newPath)."
            )
            return
        }

        switch classifyNotesFolder(at: newPath) {
        case .invalid(let reason):
            showAlert(
                title: "Can't use that folder",
                message: reason,
                style: .warning
            )
            return
        case .newOrEmpty:
            ConfigWriter.setKey("notes_folder", value: newPath)
            let sourceCount = oldPath.map(countMarkdownFiles(at:)) ?? 0
            if let oldPath = oldPath, sourceCount > 0 {
                offerMigration(from: oldPath, to: newPath, count: sourceCount)
            } else {
                showAlert(
                    title: "Notes folder saved",
                    message: "Restart Gyors to re-index from \(newPath)."
                )
            }
        case .hasNotes(let count):
            ConfigWriter.setKey("notes_folder", value: newPath)
            showAlert(
                title: "Notes folder changed - files NOT merged",
                message: """
                    \(newPath) already has \(count) note(s). They'll be indexed \
                    on next launch.

                    Gyors did NOT move any files from your previous notes \
                    folder\(oldPath.map { " (\($0))" } ?? "") - they're \
                    still there and won't be touched.
                    """,
                style: .warning
            )
        }
    }

    private enum NotesFolderStatus {
        case newOrEmpty
        case hasNotes(count: Int)
        case invalid(String)
    }

    /// Peek at destination without creating it. If we can classify
    /// safely (dir is absent, empty, or has files), callers know
    /// which branch to take; an `.invalid` case covers "path exists
    /// as a regular file" / permission denied
    private func classifyNotesFolder(at path: String) -> NotesFolderStatus {
        let fm = FileManager.default
        var isDir: ObjCBool = false
        if !fm.fileExists(atPath: path, isDirectory: &isDir) {
            return .newOrEmpty
        }
        if !isDir.boolValue {
            return .invalid("\(path) exists as a regular file, not a folder.")
        }
        let count = countMarkdownFiles(at: path)
        return count == 0 ? .newOrEmpty : .hasNotes(count: count)
    }

    /// Recursive `.md` / `.markdown` count under `root`. Matches what
    /// NotesProvider scans, so number we surface to user reflects
    /// what'll actually be indexed
    private func countMarkdownFiles(at root: String) -> Int {
        let fm = FileManager.default
        guard let enumerator = fm.enumerator(atPath: root) else { return 0 }
        var count = 0
        for case let path as String in enumerator {
            let lower = path.lowercased()
            if lower.hasSuffix(".md") || lower.hasSuffix(".markdown") {
                count += 1
            }
        }
        return count
    }

    @MainActor
    private func offerMigration(from oldPath: String, to newPath: String, count: Int) {
        let alert = NSAlert()
        alert.messageText = "Move \(count) note(s) to the new folder?"
        alert.informativeText = """
            \(oldPath)
              →
            \(newPath)

            Filenames and subfolder structure are preserved. The old folder \
            will be left in place (empty of notes). Saves are undoable via \
            "Cancel" below or by moving the files back manually.
            """
        alert.alertStyle = .informational
        alert.addButton(withTitle: "Move notes")
        alert.addButton(withTitle: "Leave them")
        NSApp.activate(ignoringOtherApps: true)
        let choice = alert.runModal()
        if choice == .alertFirstButtonReturn {
            let result = migrateNotes(from: oldPath, to: newPath)
            switch result {
            case .success(let moved):
                showAlert(
                    title: "Moved \(moved) note(s)",
                    message: "Restart Gyors to re-index from \(newPath)."
                )
            case .partial(let moved, let errors):
                showAlert(
                    title: "Moved \(moved), \(errors.count) failed",
                    message: errors.prefix(5).joined(separator: "\n"),
                    style: .warning
                )
            }
        } else {
            showAlert(
                title: "Notes folder saved",
                message: "Existing notes remain at \(oldPath). Restart Gyors to re-index from \(newPath)."
            )
        }
    }

    private enum MigrationResult {
        case success(moved: Int)
        case partial(moved: Int, errors: [String])
    }

    /// Move every `.md` / `.markdown` file under `src` into `dst`,
    /// preserving relative paths. Creates intermediate directories as
    /// needed. Skips non-markdown content so companions like
    /// `.obsidian/` / attachments stay behind - users who meant to
    /// bring those too can drag them manually
    private func migrateNotes(from src: String, to dst: String) -> MigrationResult {
        let fm = FileManager.default
        try? fm.createDirectory(atPath: dst, withIntermediateDirectories: true)
        guard let enumerator = fm.enumerator(atPath: src) else {
            return .partial(moved: 0, errors: ["Couldn't enumerate \(src)"])
        }
        var moved = 0
        var errors: [String] = []
        for case let rel as String in enumerator {
            let lower = rel.lowercased()
            guard lower.hasSuffix(".md") || lower.hasSuffix(".markdown") else { continue }
            let from = (src as NSString).appendingPathComponent(rel)
            let to = (dst as NSString).appendingPathComponent(rel)
            let toDir = (to as NSString).deletingLastPathComponent
            do {
                try fm.createDirectory(atPath: toDir, withIntermediateDirectories: true)
                try fm.moveItem(atPath: from, toPath: to)
                moved += 1
            } catch {
                errors.append("• \(rel): \(error.localizedDescription)")
            }
        }
        return errors.isEmpty ? .success(moved: moved) : .partial(moved: moved, errors: errors)
    }

    @MainActor
    private func toggleSettingWithConfirmation(_ setting: GyorsUrlHandler.Setting) {
        // Typed-whitelist toggles live here. Schema-driven bool keys
        // go through `toggleBoolKeyWithConfirmation` via URL
        // handler's `.toggleBool` route
        let current = ConfigWriter.readBool("clipboard_enabled", default: true)
        let proposed = !current
        let alert = NSAlert()
        alert.messageText = "Toggle \(setting.displayName)?"
        alert.informativeText = "Currently \(current ? "on" : "off"). Change to \(proposed ? "on" : "off")?"
        alert.alertStyle = .informational
        alert.addButton(withTitle: proposed ? "Turn On" : "Turn Off")
        alert.addButton(withTitle: "Cancel")
        NSApp.activate(ignoringOtherApps: true)
        guard alert.runModal() == .alertFirstButtonReturn else { return }

        ConfigWriter.setKey("clipboard_enabled", value: proposed)
        showAlert(
            title: "\(setting.displayName) \(proposed ? "on" : "off")",
            message: "Restart Gyors to apply."
        )
    }

    @MainActor
    private func showAlert(title: String, message: String, style: NSAlert.Style = .informational) {
        NSApp.activate(ignoringOtherApps: true)
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = message
        alert.alertStyle = style
        alert.addButton(withTitle: "OK")
        alert.runModal()
    }

    func applicationWillFinishLaunching(_ notification: Notification) {
        // Set BEFORE run-loop starts. Prevents macOS from briefly
        // showing a default main menu during launch animation
        NSApp.setActivationPolicy(.accessory)
        // Install a minimal main menu whose only job is to host
        // standard text-editing key equivalents (cmd+A, cmd+C,
        // cmd+V, cmd+X, cmd+Z). NSTextField routes these shortcuts
        // through Edit menu's selectors - with an empty main menu,
        // cmd+A in query field silently does nothing. Under
        // `.accessory` activation menu bar never renders, so this
        // adds no visual noise; AppKit only consults it for
        // `performKeyEquivalent` dispatch
        NSApp.mainMenu = Self.buildEditOnlyMainMenu()
    }

    /// Builds a root menu containing a single "Edit" submenu wired to
    /// `NSText`/`NSResponder` selectors. `nil`-targeted menu items
    /// walk responder chain, landing on whichever text control is
    /// first responder
    private static func buildEditOnlyMainMenu() -> NSMenu {
        let main = NSMenu()

        let editHost = NSMenuItem()
        let edit = NSMenu(title: "Edit")

        let undo = NSMenuItem(
            title: "Undo",
            action: Selector(("undo:")),
            keyEquivalent: "z"
        )
        let redo = NSMenuItem(
            title: "Redo",
            action: Selector(("redo:")),
            keyEquivalent: "z"
        )
        redo.keyEquivalentModifierMask = [.command, .shift]
        edit.addItem(undo)
        edit.addItem(redo)
        edit.addItem(.separator())
        edit.addItem(NSMenuItem(
            title: "Cut",
            action: #selector(NSText.cut(_:)),
            keyEquivalent: "x"
        ))
        edit.addItem(NSMenuItem(
            title: "Copy",
            action: #selector(NSText.copy(_:)),
            keyEquivalent: "c"
        ))
        edit.addItem(NSMenuItem(
            title: "Paste",
            action: #selector(NSText.paste(_:)),
            keyEquivalent: "v"
        ))
        edit.addItem(NSMenuItem(
            title: "Select All",
            action: #selector(NSResponder.selectAll(_:)),
            keyEquivalent: "a"
        ))

        editHost.submenu = edit
        main.addItem(editHost)
        return main
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Load config schema once, right after Rust bridge has been
        // initialised. Makes ConfigSchema queryable for rest of
        // process lifetime (URL handler, diagnostics, future
        // settings UI)
        ConfigSchemaFfiLoader.loadFromFfi()

        // Wire directory picker - Effect.swift calls this through a
        // closure hook so test harness doesn't need AppDelegate
        EffectRunner.directoryPickerHandler = { key, prompt in
            DirectoryPicker.run(configKey: key, prompt: prompt)
        }

        let config = Config.load()
        let hotkeyLabel = config.hotkey ?? Config.defaultHotkey
        let binding = config.hotkeyBinding

        let hk = Hotkey { [weak self] in
            self?.panel.toggle()
        }
        let ok = hk.register(keyCode: binding.keyCode, modifiers: binding.modifiers)
        self.hotkey = hk

        // Spin up auto-updater. In a non-SPARKLE build this
        // is a no-op stub and menu item that drives it stays hidden;
        // on a SPARKLE build it boots Sparkle's background scheduler
        // so next periodic check fires per `SUScheduledCheckInterval`
        let upd = Updater()
        self.updater = upd

        menuBar = MenuBar(
            panel: panel,
            bridge: bridge,
            hotkeyLabel: hotkeyLabel,
            updater: upd
        )

        let watcher = PasteboardWatcher(bridge: bridge)
        // Re-query panel when a fresh copy lands so user sees
        // just-copied content in `clip` list without needing to
        // close + reopen
        watcher.onChange = { [weak self] in
            self?.panel.refreshIfClipboardVisible()
        }
        watcher.start()
        pasteboardWatcher = watcher

        NSLog("gyors: hotkey \(hotkeyLabel) registered=\(ok)")
        NSLog("gyors: \(bridge.appCount) apps indexed, \(bridge.clipboardCount) clips")
        // One-shot diagnostics dump. Filterable in Console.app via
        // "gyors: diag" - helps root-cause user-reported "X isn't
        // showing up" without having to instrument individual
        // providers. Covers notes folder resolution (common culprit
        // for "note he shows only Create")
        if let diag = bridge.diagnosticsJson() {
            NSLog("gyors: diag %@", diag)
        }

        FirstRun.runIfNeeded(hotkeyLabel: hotkeyLabel)

        // Global snippet expansion is gated behind a config toggle -
        // first launch after user flips `snippets.expand_globally`
        // prompts for Accessibility permission, so leave prompt
        // silent on fresh installs where user hasn't asked for it
        if gyors_snippets_global_enabled() {
            GlobalSnippetExpander.shared.start()
        }

        // Pre-warm panel offscreen so first hotkey press just calls
        // `show()` against a built NSPanel + warmed SwiftUI host -
        // no cold NSHostingView / Auto-Layout / sizeThatFits cost in
        // user-visible path. Deferred one runloop hop so menu bar
        // paints first; user can't trigger panel before this hop
        // runs anyway
        DispatchQueue.main.async { [weak self] in
            self?.panel.prewarm()
        }
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        panel.show()
        return false
    }

    // MARK: Quick Look responder-chain compliance
    //
    // QLPreviewPanel uses standard responder chain to find object
    // that should feed it data. Adopting these methods in
    // AppDelegate means `QuickLookPresenter.shared` stays bound as
    // datasource for panel's full lifetime; otherwise panel
    // dismisses moment user releases cmd+Y

    override func acceptsPreviewPanelControl(_ panel: QLPreviewPanel!) -> Bool {
        true
    }

    override func beginPreviewPanelControl(_ panel: QLPreviewPanel!) {
        panel.dataSource = QuickLookPresenter.shared
        panel.delegate = QuickLookPresenter.shared
    }

    override func endPreviewPanelControl(_ panel: QLPreviewPanel!) {
        // Nothing - presenter is a singleton and its references are
        // weak-held by panel while open
    }
}
