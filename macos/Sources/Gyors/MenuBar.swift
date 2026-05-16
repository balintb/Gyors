import AppKit

@MainActor
final class MenuBar: NSObject {
    private let statusItem: NSStatusItem
    private let panel: PanelController
    private let bridge: QueryBridge
    private let hotkeyLabel: String
    private let updater: Updater
    private var iconAnimator: MenuBarIconAnimator?

    init(panel: PanelController, bridge: QueryBridge, hotkeyLabel: String, updater: Updater) {
        self.panel = panel
        self.bridge = bridge
        self.hotkeyLabel = hotkeyLabel
        self.updater = updater
        // `variableLength` lets status item grow when we append a
        // live timer countdown; `squareLength` would clip anything
        // past icon ("cmd01:59" gets cut to "cmd1")
        self.statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        super.init()

        if let button = statusItem.button {
            // Brand mark from bundle's Resources (PDF stays vector
            // sharp at every menubar scale). Template mode lets
            // macOS tint it for light/dark menu bars automatically -
            // same contract SF Symbols carry, just with our `g*`
            // glyph
            let icon = Self.menuBarIcon()
            icon?.isTemplate = true
            button.image = icon
            button.toolTip = "Gyors - \(hotkeyLabel)"

            if let icon = icon {
                let animator = MenuBarIconAnimator(statusItem: statusItem, baseIcon: icon)
                iconAnimator = animator
                // Wire BusyTracker's edge-trigger callbacks straight
                // to animator. Both are MainActor - BusyTracker hops
                // to main before invoking these - so no extra
                // dispatch is needed
                BusyTracker.shared.onBusyChange = { [weak animator] busy in
                    animator?.setBusy(busy)
                }
                BusyTracker.shared.onFlash = { [weak animator] in
                    animator?.flash()
                }
            }
        }

        rebuildMenu()

        // Live countdown for running timers: TimerManager pushes
        // shortest remaining time here so status item always
        // reflects current state without bar having to poll
        TimerManager.shared.onMenuBarUpdate = { [weak self] text in
            self?.applyTimerLabel(text)
        }
    }

    private func applyTimerLabel(_ text: String?) {
        guard let button = statusItem.button else { return }
        if let t = text {
            button.title = " \(t)"
            button.imagePosition = .imageLeft
        } else {
            button.title = ""
            button.imagePosition = .imageOnly
        }
    }

    /// Reconstructs menu from scratch - call after importing themes
    func rebuildMenu() {
        let menu = NSMenu()
        menu.autoenablesItems = false

        menu.addItem(item(title: "Show Gyors", action: #selector(showGyors)))

        let hotkeyInfo = item(title: "Hotkey  \(formatHotkey(hotkeyLabel))", action: nil)
        hotkeyInfo.isEnabled = false
        menu.addItem(hotkeyInfo)

        menu.addItem(.separator())

        let themeParent = NSMenuItem(title: "Theme", action: nil, keyEquivalent: "")
        themeParent.submenu = buildThemeMenu()
        menu.addItem(themeParent)

        menu.addItem(item(title: "Clear Clipboard History", action: #selector(clearClipboardHistory)))
        // Cloud Sync menu item only surfaces in CLOUD builds.
        // SyncPanel + FFI symbols dont exist when build-app.sh
        // runs with WITH_CLOUD=0
        #if CLOUD
        menu.addItem(item(title: "Cloud Sync…", action: #selector(showSync)))
        #endif
        menu.addItem(item(title: "Open Config File…", action: #selector(openConfig)))

        menu.addItem(.separator())

        menu.addItem(item(
            title: "Request Finder Automation Access…",
            action: #selector(requestFinderAutomation)
        ))
        menu.addItem(item(
            title: "Show Welcome…",
            action: #selector(showWelcome)
        ))
        menu.addItem(item(
            title: "Diagnostics…",
            action: #selector(showDiagnostics)
        ))

        // Only surface "Check for Updates..." on builds
        // that actually shipped Sparkle. Stub Updater would appear
        // as a dead menu item otherwise
        if Updater.isAvailable {
            menu.addItem(item(title: "Check for Updates…", action: #selector(checkForUpdates)))
        }

        menu.addItem(.separator())

        menu.addItem(item(title: "About Gyors…", action: #selector(showAbout)))
        menu.addItem(item(title: "Quit Gyors", action: #selector(quit), keyEquivalent: "q"))

        statusItem.menu = menu
    }

    private func item(title: String, action: Selector?, keyEquivalent: String = "") -> NSMenuItem {
        let i = NSMenuItem(title: title, action: action, keyEquivalent: keyEquivalent)
        i.target = self
        return i
    }

    private func buildThemeMenu() -> NSMenu {
        let themeMenu = NSMenu()
        themeMenu.autoenablesItems = false
        let currentId = ThemeManager.shared.current.id
        for theme in Themes.all {
            let i = NSMenuItem(
                title: theme.label,
                action: #selector(selectTheme(_:)),
                keyEquivalent: ""
            )
            i.target = self
            i.representedObject = theme.id
            i.state = (theme.id == currentId) ? .on : .off
            themeMenu.addItem(i)
        }

        let customs = ThemeManager.shared.customThemes
        if !customs.isEmpty {
            let header = NSMenuItem(title: "Custom", action: nil, keyEquivalent: "")
            header.isEnabled = false
            themeMenu.addItem(.separator())
            themeMenu.addItem(header)
            for theme in customs {
                // Two rows per custom theme: visible "Apply" row
                // and an Option-alternate "Remove..." row. macOS's
                // NSMenuItem alternate mechanism shows second row
                // in place of first while user holds opt -
                // discoverable to power users (standard convention,
                // same one Finder / Safari use), invisible to
                // everyone else so menu stays one-click-to-apply
                let apply = NSMenuItem(
                    title: theme.label,
                    action: #selector(selectTheme(_:)),
                    keyEquivalent: ""
                )
                apply.target = self
                apply.representedObject = theme.id
                apply.state = (theme.id == currentId) ? .on : .off
                themeMenu.addItem(apply)

                let remove = NSMenuItem(
                    title: "Remove \"\(theme.label)\"…",
                    action: #selector(removeCustomTheme(_:)),
                    keyEquivalent: ""
                )
                remove.target = self
                remove.representedObject = theme.id
                // `keyEquivalentModifierMask = .option` +
                // `isAlternate = true` is pair AppKit needs:
                // Previous item is "regular", this one's
                // "alternate", and they swap on Option-hold. Both
                // must share same key equivalent (here: empty) for
                // swap to fire
                remove.keyEquivalentModifierMask = .option
                remove.isAlternate = true
                themeMenu.addItem(remove)
            }
            // Footer hint so alternate row isn't a hidden
            // power-user trick. Disabled item so it reads as a label
            let hint = NSMenuItem(
                title: "Hold ⌥ on a custom theme to remove",
                action: nil,
                keyEquivalent: ""
            )
            hint.isEnabled = false
            themeMenu.addItem(hint)
        }

        themeMenu.addItem(.separator())
        themeMenu.addItem(item(title: "Import Theme from URL…", action: #selector(importTheme)))
        themeMenu.addItem(item(title: "Copy Current Theme URL", action: #selector(copyCurrentThemeUrl)))
        themeMenu.addItem(item(title: "Reveal Themes Folder", action: #selector(revealThemesFolder)))

        return themeMenu
    }

    /// Confirm + delete a custom theme. Reached via Option-alternate
    /// menu row built in `buildThemeMenu`. Falls back to default
    /// theme inside `ThemeManager.remove` when deleted theme is
    /// currently active, so panel never renders a vanished theme
    @objc private func removeCustomTheme(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? String else { return }
        let label = ThemeManager.shared.customThemes
            .first(where: { $0.id == id })?
            .label ?? id
        let alert = NSAlert()
        alert.messageText = "Remove \"\(label)\"?"
        alert.informativeText = """
            This deletes the theme file from disk. You can re-import \
            it later from its share URL.
            """
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Remove")
        alert.addButton(withTitle: "Cancel")
        // Cancel is keyboard-default so an accidental Enter doesn't
        // delete. Matches macOS HIG for destructive confirmations
        alert.buttons[1].keyEquivalent = "\r"
        alert.buttons[0].keyEquivalent = ""
        NSApp.activate(ignoringOtherApps: true)
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        _ = ThemeManager.shared.remove(id: id)
        rebuildMenu()
    }

    private func formatHotkey(_ raw: String) -> String {
        let tokens = raw
            .lowercased()
            .components(separatedBy: CharacterSet(charactersIn: "+- "))
            .filter { !$0.isEmpty }
        return tokens.map { glyph(for: $0) }.joined()
    }

    private func glyph(for token: String) -> String {
        switch token {
        case "cmd", "command": return "⌘"
        case "opt", "option", "alt": return "⌥"
        case "ctrl", "control": return "⌃"
        case "shift": return "⇧"
        case "space": return "Space"
        case "tab": return "Tab"
        case "return", "enter": return "↩"
        case "escape", "esc": return "⎋"
        case "delete", "backspace": return "⌫"
        case "up": return "↑"
        case "down": return "↓"
        case "left": return "←"
        case "right": return "→"
        default:
            return token.count == 1 ? token.uppercased() : token.capitalized
        }
    }

    private func configURL() -> URL {
        Config.configURL()
    }

    @objc private func showGyors() {
        panel.show()
    }

    @objc private func selectTheme(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? String else { return }
        ThemeManager.shared.apply(id: id)
        rebuildMenu()
    }

    @objc private func importTheme() {
        let alert = NSAlert()
        alert.messageText = "Import theme"
        alert.informativeText = "Paste a gyors://theme?import=… URL (or the base64 payload alone)."
        alert.addButton(withTitle: "Import")
        alert.addButton(withTitle: "Cancel")
        let input = NSTextField(frame: NSRect(x: 0, y: 0, width: 400, height: 24))
        input.stringValue = NSPasteboard.general.string(forType: .string) ?? ""
        alert.accessoryView = input
        NSApp.activate(ignoringOtherApps: true)
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        let raw = input.stringValue.trimmingCharacters(in: .whitespaces)
        if raw.isEmpty { return }

        let result: Result<Theme, ThemeImporter.Error>
        if let url = URL(string: raw), url.scheme == "gyors" {
            result = ThemeImporter.importFromURL(url)
        } else {
            result = ThemeImporter.importFromBase64(raw)
        }

        switch result {
        case .success(let theme):
            ThemeManager.shared.reloadCustomThemes()
            ThemeManager.shared.apply(theme)
            rebuildMenu()
            showNotice(title: "Theme imported", message: "\"\(theme.label)\" is now active.")
        case .failure(let err):
            showNotice(
                title: "Import failed",
                message: err.localizedDescription,
                style: .warning
            )
        }
    }

    @objc private func copyCurrentThemeUrl() {
        let theme = ThemeManager.shared.current
        guard let url = ThemeImporter.url(for: theme) else {
            showNotice(title: "Couldn't build URL", message: "Serialization failed.", style: .warning)
            return
        }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(url.absoluteString, forType: .string)
        showNotice(
            title: "Theme URL copied",
            message: "Share it - recipients open it to import \"\(theme.label)\"."
        )
    }

    @objc private func revealThemesFolder() {
        let dir = ThemeImporter.themesDir()
        NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: dir.path)
    }

    @objc private func clearClipboardHistory() {
        let before = bridge.clipboardCount
        bridge.clearClipboardHistory()
        NSLog("gyors: cleared \(before) clipboard items")
    }

    #if CLOUD
    @objc private func showSync() {
        SyncPanel.present()
    }
    #endif

    @objc private func openConfig() {
        let url = configURL()
        if !FileManager.default.fileExists(atPath: url.path) {
            let content = """
            {
              "hotkey": "\(hotkeyLabel)"
            }

            """
            try? FileManager.default.createDirectory(
                at: url.deletingLastPathComponent(),
                withIntermediateDirectories: true
            )
            try? content.write(to: url, atomically: true, encoding: .utf8)
        }
        NSWorkspace.shared.open(url)
    }

    /// Ad-hoc signed apps dont appear in System Settings -> Privacy
    /// & Security -> Automation until they first try to send an
    /// AppleEvent. Running a tiny, no-op AppleScript is
    /// officially-supported way to register Gyors with TCC's
    /// Automation pane so user can then toggle Finder checkbox. If
    /// prompt has already been denied we open pane directly so it
    /// can be enabled by hand
    @objc private func requestFinderAutomation() {
        DispatchQueue.main.async {
            NSApp.activate(ignoringOtherApps: true)

            // A benign, side-effect-free script - just asks Finder
            // its name. macOS sees target bundle id and triggers
            // one-time consent prompt. Successful result or user
            // approval -> Gyors now appears in Automation list
            let src = #"tell application "Finder" to return name"#
            var errorInfo: NSDictionary?
            let script = NSAppleScript(source: src)
            _ = script?.executeAndReturnError(&errorInfo)

            let alert = NSAlert()
            if let err = errorInfo {
                let code = (err[NSAppleScript.errorNumber] as? NSNumber)?.intValue ?? 0
                let msg = (err[NSAppleScript.errorMessage] as? String)
                    ?? (err[NSAppleScript.errorBriefMessage] as? String)
                    ?? "\(err)"
                if code == -1743 {
                    // Not yet authorised, or previously denied. Open
                    // pane so user can add / enable Gyors manually
                    alert.messageText = "Automation access not granted"
                    alert.informativeText = """
                        macOS didn't let Gyors control Finder this time. \
                        Opening System Settings → Privacy & Security → Automation. \
                        Find "Gyors" in the list and tick the "Finder" checkbox.

                        If Gyors isn't listed yet, try this menu item again after \
                        clicking Allow on the prompt.

                        Details: \(msg)
                        """
                    alert.alertStyle = .warning
                    alert.addButton(withTitle: "Open Automation Settings")
                    alert.addButton(withTitle: "Cancel")
                    if alert.runModal() == .alertFirstButtonReturn,
                       let url = URL(
                           string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Automation"
                       )
                    {
                        NSWorkspace.shared.open(url)
                    }
                } else {
                    alert.messageText = "Automation request failed"
                    alert.informativeText = msg
                    alert.alertStyle = .warning
                    alert.addButton(withTitle: "OK")
                    alert.runModal()
                }
            } else {
                alert.messageText = "Finder automation access granted"
                alert.informativeText = """
                    Gyors is now registered with macOS Automation for Finder. \
                    You can manage the toggle in System Settings → Privacy & \
                    Security → Automation → Gyors.
                    """
                alert.alertStyle = .informational
                alert.addButton(withTitle: "Open Automation Settings")
                alert.addButton(withTitle: "Done")
                if alert.runModal() == .alertFirstButtonReturn,
                   let url = URL(
                       string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Automation"
                   )
                {
                    NSWorkspace.shared.open(url)
                }
            }
        }
    }

    @objc private func showWelcome() {
        FirstRun.showManually(hotkeyLabel: hotkeyLabel)
    }

    @objc private func showDiagnostics() {
        // Pretty-print backend's diagnostics JSON. Users typically
        // open this when a "provider X isn't showing" bug comes in -
        // we auto-copy to clipboard on open AND format as a
        // ready-to-paste GitHub issue body (markdown code fence +
        // header + environment), so user can alt-tab into their
        // browser, cmd+V, and hit Submit
        let raw = bridge.diagnosticsJson() ?? "{}"
        let pretty: String
        if let data = raw.data(using: .utf8),
           let parsed = try? JSONSerialization.jsonObject(with: data),
           let formatted = try? JSONSerialization.data(
               withJSONObject: parsed,
               options: [.prettyPrinted, .sortedKeys]),
           let s = String(data: formatted, encoding: .utf8)
        {
            pretty = s
        } else {
            pretty = raw
        }
        let issueBody = Self.buildIssueBody(diagnosticsJson: pretty)

        // Auto-copy immediately so even if user dismisses without
        // clicking anything, diagnostic is already on clipboard
        // ready to paste
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(issueBody, forType: .string)

        let alert = NSAlert()
        alert.messageText = "Gyors · Diagnostics (copied to clipboard)"
        alert.informativeText = """
            The report below is on your clipboard as a GitHub-issue-ready \
            markdown block. Paste it into https://github.com/balintb/Gyors/issues/new \
            and describe what happened.
            """
        alert.alertStyle = .informational
        // Long diagnostics into a bounded scrollable text view
        // rather than `informativeText`. NSAlert grows its window
        // to fit informativeText with no upper bound, so a few KB
        // of JSON pushes buttons off-screen. Bundling dump into a
        // 580x340 scroller caps window height + makes JSON actually
        // readable + still selectable for manual copy
        alert.accessoryView = Self.makeDiagnosticsTextView(content: pretty)
        alert.addButton(withTitle: "Open GitHub Issue")
        alert.addButton(withTitle: "Copy Again")
        alert.addButton(withTitle: "Close")
        NSApp.activate(ignoringOtherApps: true)
        let choice = alert.runModal()
        switch choice {
        case .alertFirstButtonReturn:
            // Prefill issue body via new-issue query string. GitHub
            // tolerates URLs up to ~8 KB; our payload fits well
            // under that. Percent-encode to survive the trip
            let encoded = issueBody
                .addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)
                ?? issueBody
            if let url = URL(string:
                "https://github.com/balintb/Gyors/issues/new?body=\(encoded)")
            {
                NSWorkspace.shared.open(url)
            }
        case .alertSecondButtonReturn:
            pb.clearContents()
            pb.setString(issueBody, forType: .string)
        default:
            break
        }
    }

    /// Wrap diagnostics dump in a markdown template suitable for a
    /// GitHub bug report. Includes OS version + app version so
    /// issue has context maintainers always ask for
    static func buildIssueBody(diagnosticsJson: String) -> String {
        let os = ProcessInfo.processInfo.operatingSystemVersionString
        return """
        ### What happened

        <!-- Replace with a short description of what went wrong. -->

        ### Steps to reproduce

        <!-- Describe how to trigger the bug. -->

        ### Environment

        - macOS: \(os)
        - Gyors: \(Self.appVersion())

        ### Diagnostics

        ```json
        \(diagnosticsJson)
        ```
        """
    }

    /// Build a fixed-size scrollable text view holding `content`.
    /// Used as an `NSAlert.accessoryView` so alert window has a
    /// bounded height regardless of payload length. Monospaced font
    /// keeps JSON columns aligned; no word-wrap so bracket
    /// structure stays readable even on long string values
    /// (horizontal scrollbar appears when needed)
    private static func makeDiagnosticsTextView(content: String) -> NSScrollView {
        let frame = NSRect(x: 0, y: 0, width: 580, height: 340)
        let scroll = NSScrollView(frame: frame)
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = false
        scroll.borderType = .bezelBorder

        let textView = NSTextView(frame: frame)
        textView.isEditable = false
        textView.isSelectable = true
        textView.drawsBackground = true
        textView.backgroundColor = .textBackgroundColor
        textView.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
        textView.textContainerInset = NSSize(width: 6, height: 6)

        // Disable word wrap so long URLs / inline strings dont
        // break JSON shape. Standard recipe: detach text container
        // from view width and give it a huge intrinsic width
        textView.isHorizontallyResizable = true
        textView.isVerticallyResizable = true
        textView.autoresizingMask = [.width]
        textView.textContainer?.widthTracksTextView = false
        textView.textContainer?.containerSize = NSSize(
            width: CGFloat.greatestFiniteMagnitude,
            height: CGFloat.greatestFiniteMagnitude
        )
        textView.maxSize = NSSize(
            width: CGFloat.greatestFiniteMagnitude,
            height: CGFloat.greatestFiniteMagnitude
        )
        textView.string = content

        scroll.documentView = textView
        return scroll
    }

    /// Read user-visible version from bundle's Info.plist.
    /// `CFBundleShortVersionString` is the SemVer the release
    /// pipeline stamps in; falling back to `CFBundleVersion`
    /// covers a bundle that only has build number, and an empty
    /// string covers anything truly missing (which would also
    /// break Sparkle / notarisation but at least keeps About
    /// panel from showing `nil`)
    static func appVersion() -> String {
        let info = Bundle.main.infoDictionary
        if let v = info?["CFBundleShortVersionString"] as? String, !v.isEmpty {
            return v
        }
        if let v = info?["CFBundleVersion"] as? String, !v.isEmpty {
            return v
        }
        return "unknown"
    }

    @objc private func showAbout() {
        let alert = NSAlert()
        alert.messageText = "Gyors"
        alert.informativeText = """
        Version \(Self.appVersion())

        A keyboard-first launcher for macOS.
        ©️ @balintb
        Rust core • SwiftUI shell • free & open source.

        Hotkey: \(formatHotkey(hotkeyLabel))
        Apps indexed: \(bridge.appCount)
        Clipboard items: \(bridge.clipboardCount)
        """
        alert.alertStyle = .informational
        alert.addButton(withTitle: "OK")
        alert.runModal()
    }

    @objc private func quit() {
        NSApp.terminate(nil)
    }

    @objc private func checkForUpdates() {
        updater.checkForUpdates()
    }

    private func showNotice(title: String, message: String, style: NSAlert.Style = .informational) {
        NSApp.activate(ignoringOtherApps: true)
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = message
        alert.alertStyle = style
        alert.addButton(withTitle: "OK")
        alert.runModal()
    }

    /// Load `MenuBarIcon.pdf` from bundle and clamp it to a
    /// menu-bar-friendly size. PDF stays vector-sharp; explicit
    /// 18pt cap matches SF Symbol baseline so bar height doesn't
    /// shift when we swap in brand mark
    private static func menuBarIcon() -> NSImage? {
        guard
            let url = Bundle.main.url(forResource: "MenuBarIcon", withExtension: "pdf"),
            let img = NSImage(contentsOf: url)
        else {
            // Fall back to SF Symbol so menu bar item is never
            // invisible - users with a malformed bundle still see a
            // launcher glyph
            return NSImage(systemSymbolName: "command", accessibilityDescription: "Gyors")
        }
        img.size = NSSize(width: 18, height: 18)
        return img
    }
}
