import AppKit

/// Presents a native folder picker (`NSOpenPanel`) and writes
/// chosen path under given config key. Keeps GUI-opening logic out
/// of Rust side so providers stay pure + testable
///
/// Per-key behaviour:
///   - `notes_folder` -> routes through
///     `AppDelegate.applyNotesFolderChange`, which runs existing
///     new/empty vs has-notes branching (offer migration or warn,
///     never silently strand files).
///   - Anything else -> `ConfigWriter.setKey` after a single
///     confirm alert. Good enough for future folder settings;
///     individual apply handlers can be added as they earn them
enum DirectoryPicker {
    @MainActor
    static func run(configKey: String, prompt: String) {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true
        panel.showsHiddenFiles = false
        panel.resolvesAliases = true
        panel.message = prompt
        panel.prompt = "Use This Folder"
        panel.title = "Pick a folder for \(configKey)"
        // Start in current value's parent when possible - jumping
        // to previous folder beats dropping user in ~/
        if let current = currentPath(for: configKey),
           FileManager.default.fileExists(atPath: current)
        {
            panel.directoryURL = URL(fileURLWithPath: current)
        }

        NSApp.activate(ignoringOtherApps: true)
        guard panel.runModal() == .OK, let url = panel.url else { return }
        let path = url.path

        apply(configKey: configKey, path: path)
    }

    @MainActor
    private static func apply(configKey: String, path: String) {
        // Keys with bespoke apply handlers (migration, hotkey
        // re-reg, theme swap) go through AppDelegate path so logic
        // stays in one place. Everything else falls back to a
        // plain write
        if configKey == "notes_folder" {
            guard let appDelegate = NSApp.delegate as? AppDelegate else {
                ConfigWriter.setKey(configKey, value: path)
                return
            }
            appDelegate.performNotesFolderChange(to: path)
            return
        }
        // Generic path key: confirm + write
        let alert = NSAlert()
        alert.messageText = "Set \(configKey)?"
        alert.informativeText = "Will write:\n\(path)\n\nto config.json"
        alert.alertStyle = .informational
        alert.addButton(withTitle: "Save")
        alert.addButton(withTitle: "Cancel")
        NSApp.activate(ignoringOtherApps: true)
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        ConfigWriter.setKey(configKey, value: path)
    }

    /// Read current value of `key` from config.json. Used to seed
    /// panel's starting directory. Returns nil if key is absent or
    /// not a string
    private static func currentPath(for key: String) -> String? {
        let url = Config.configURL()
        guard let data = try? Data(contentsOf: url),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return nil }
        // Support dotted keys (`notes_folder`, `jwt.secret` ...)
        // via same convention ConfigWriter uses elsewhere
        return resolveDotted(obj, key: key) as? String
    }

    private static func resolveDotted(_ root: Any, key: String) -> Any? {
        let parts = key.split(separator: ".")
        var cur: Any? = root
        for p in parts {
            if let dict = cur as? [String: Any] {
                cur = dict[String(p)]
            } else {
                return nil
            }
        }
        return cur
    }
}
