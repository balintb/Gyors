import Foundation

/// On-disk user config. Edit at:
///   ~/Library/Application Support/Gyors/config.json
///
/// Missing fields fall back to defaults. A malformed file is logged and
/// ignored
struct Config: Decodable {
    let hotkey: String?
    let ai: AiConfig?
    let theme: String?
    let notesFolder: String?
    /// Render markdown in inline previews (AI answers, note previews).
    /// Nil -> default true. Toggle via `gyors://toggle/preview-markdown`
    let previewMarkdown: Bool?
    /// Colorize json/yaml/toml in inline previews. Nil -> default true.
    /// Toggle via `gyors://toggle/preview-syntax`
    let previewSyntax: Bool?
    /// Which Terminal-style app to launch shell / ssh rows in. Bundle
    /// name as it appears under `/Applications` ("Terminal", "iTerm",
    /// "Ghostty", "Alacritty", "WezTerm", "kitty"). Nil -> "Terminal"
    let terminalApp: String?
    /// User override for active theme's `backgroundOpacity`. Stored
    /// as a Text field so empty / missing means "use theme's
    /// default" - gives users a one-knob escape hatch without
    /// forcing every theme to commit to a single look. Range 0.0 -
    /// 1.0; clamped at read time. Named `panel_opacity` (not
    /// `theme.opacity`) so dotted-set parser doesn't collapse
    /// user's theme-name string into a nested object
    let panelOpacity: String?
    /// User override for active theme's `usesBlur`. Same
    /// nil-or-blank-means-use-theme convention as `panelOpacity`.
    /// Accepts true/false/yes/no/on/off/1/0
    let panelBlur: String?

    enum CodingKeys: String, CodingKey {
        case hotkey, ai, theme
        case notesFolder = "notes_folder"
        case previewMarkdown = "preview_markdown"
        case previewSyntax = "preview_syntax"
        case terminalApp = "terminal_app"
        case panelOpacity = "panel_opacity"
        case panelBlur = "panel_blur"
    }

    /// Parsed override for `panel_opacity`. nil -> use active
    /// theme's default. Otherwise clamped to [0.0, 1.0]
    var effectivePanelOpacity: Double? {
        guard let raw = panelOpacity?.trimmingCharacters(in: .whitespaces),
              !raw.isEmpty,
              let v = Double(raw)
        else { return nil }
        return max(0.0, min(1.0, v))
    }

    /// Parsed override for `panel_blur`. nil -> use active theme's
    /// default. Truthy values: true / yes / on / 1
    var effectivePanelBlur: Bool? {
        guard let raw = panelBlur?.trimmingCharacters(in: .whitespaces).lowercased(),
              !raw.isEmpty
        else { return nil }
        switch raw {
        case "true", "yes", "on", "1": return true
        case "false", "no", "off", "0": return false
        default: return nil
        }
    }

    /// Convenience: read `ai.router_enabled` from nested AI block.
    /// Flag lives under `ai.*` because it gates an AI behaviour and
    /// benefits from sharing JSON namespace with `ai.provider`,
    /// `ai.model`, etc. Off when missing
    var effectiveRouterEnabled: Bool {
        ai?.effectiveRouterEnabled ?? false
    }

    static let defaultHotkey = "opt+shift+space"
    static let defaultTerminalApp = "Terminal"

    var effectiveTerminalApp: String {
        let v = (terminalApp ?? "").trimmingCharacters(in: .whitespaces)
        return v.isEmpty ? Self.defaultTerminalApp : v
    }

    /// Preview-renderer defaults: both on. Feature parity with
    /// Raycast / Alfred; users can flip them off per-key if colour
    /// scheme clashes with their theme
    static let defaultPreviewMarkdown = true
    static let defaultPreviewSyntax = true

    var effectivePreviewMarkdown: Bool { previewMarkdown ?? Self.defaultPreviewMarkdown }
    var effectivePreviewSyntax: Bool { previewSyntax ?? Self.defaultPreviewSyntax }

    static func load(from customURL: URL? = nil) -> Config {
        let url = customURL ?? configURL()
        if !FileManager.default.fileExists(atPath: url.path) {
            if customURL == nil { writeDefault(to: url) }
            return empty()
        }
        guard let data = try? Data(contentsOf: url) else {
            return empty()
        }
        // Legacy installs have `ai.api_key` in
        // config.json. Migrate it to Keychain on every load
        // (idempotent after first run). Migration runs for both
        // canonical path and explicit `customURL` arguments -
        // tests assert on it directly, and Keychain write is
        // path-independent anyway. Best-effort: a failed Keychain
        // write leaves file alone, so we dont lose user's key on
        // a Keychain hiccup.
        // Also harden file's permissions to 0600 - even
        // after migration, other sensitive fields (jwt.secret,
        // future plugin tokens) may still be there
        migrateAiApiKeyToKeychainIfNeeded(at: url, data: data)
        tightenPermissions(at: url)
        // Re-read after possible migration so decoded Config
        // reflects rewritten file (no in-memory `api_key`)
        let finalData = (try? Data(contentsOf: url)) ?? data
        do {
            return try JSONDecoder().decode(Config.self, from: finalData)
        } catch {
            NSLog("gyors: malformed config.json - using defaults (\(error))")
            return empty()
        }
    }

    /// One-shot migration: if `ai.api_key` is present in on-disk
    /// config, copy it into Keychain and strip it from file.
    /// Subsequent loads see no `api_key` in JSON and
    /// `AiConfig.effectiveApiKey` falls back to Keychain
    private static func migrateAiApiKeyToKeychainIfNeeded(at url: URL, data: Data) {
        #if !AI
        // No AI build = no AiKeychain. Migration is also moot
        // because nothing reads `ai.api_key`. Leave on-disk file
        // alone
        _ = (url, data)
        return
        #else
        guard
            let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            var ai = root["ai"] as? [String: Any],
            let key = ai["api_key"] as? String,
            !key.isEmpty
        else { return }
        guard AiKeychain.write(key) else {
            NSLog("gyors: migration: Keychain write failed; leaving config.json untouched")
            return
        }
        // Drop key from in-memory copy and write back
        ai.removeValue(forKey: "api_key")
        var newRoot = root
        if ai.isEmpty {
            newRoot.removeValue(forKey: "ai")
        } else {
            newRoot["ai"] = ai
        }
        do {
            let out = try JSONSerialization.data(
                withJSONObject: newRoot,
                options: [.prettyPrinted, .sortedKeys]
            )
            try out.write(to: url, options: .atomic)
            NSLog("gyors: migration: ai.api_key moved from config.json to Keychain")
        } catch {
            // We've already put key in Keychain - if we can't strip
            // it from file, file's stale copy stays until next
            // load. Try again next time
            NSLog("gyors: migration: failed to rewrite config.json: \(error)")
        }
        #endif
    }

    /// Clamp config.json to 0600. File holds (or recently
    /// held) sensitive material; a one-time `chmod 644` from a
    /// backup tool or helpful "fix" permanently widens read
    /// surface. We re-tighten on every load - cheap, and undoes
    /// external loosening
    private static func tightenPermissions(at url: URL) {
        do {
            try FileManager.default.setAttributes(
                [.posixPermissions: 0o600],
                ofItemAtPath: url.path
            )
        } catch {
            NSLog("gyors: chmod 600 on config.json failed: \(error)")
        }
    }

    private static func empty() -> Config {
        Config(
            hotkey: nil, ai: nil, theme: nil, notesFolder: nil,
            previewMarkdown: nil, previewSyntax: nil,
            terminalApp: nil,
            panelOpacity: nil, panelBlur: nil
        )
    }

    /// Public so `ThemeManager` can write file while preserving
    /// other keys
    static func configURL() -> URL {
        privateConfigURL()
    }

    var hotkeyBinding: HotkeyBinding {
        HotkeyBinding.parse(hotkey ?? Self.defaultHotkey)
            ?? HotkeyBinding.parse(Self.defaultHotkey)!
    }

    private static func privateConfigURL() -> URL {
        // Route through GyorsPaths so Gyors root gets
        // chmod 0700 on every config-touch path. Previously dir was
        // created with default umask (0755) and only file inside
        // ended up 0600
        let base = GyorsPaths.ensureDataDirSecured() ?? GyorsPaths.dataDir()
        return base.appendingPathComponent("config.json")
    }

    private static func writeDefault(to url: URL) {
        let defaults = """
        {
          "hotkey": "\(defaultHotkey)"
        }

        """
        try? defaults.write(to: url, atomically: true, encoding: .utf8)
    }
}

struct AiConfig: Decodable {
    let provider: String?
    let model: String?
    let endpoint: String?
    let apiKey: String?
    /// Opt-in flag for AI command router. Decoded by `LooseBool` so
    /// it accepts BOTH JSON-bool form (`true`/`false`) that
    /// `config set ai.router_enabled true` writes AND truthy/falsy
    /// string aliases (`"true"`/`"yes"`/`"on"`/`"1"`) that
    /// hand-edited configs often use. Strict typing here was a real
    /// bug: when stored JSON didn't match `String?`, Swift's
    /// decoder failed entire `ai` block, AI provider plus this flag
    /// both silently disappeared, and router refused to fire even
    /// after user had toggled it on via config UI
    let routerEnabled: LooseBool?

    enum CodingKeys: String, CodingKey {
        case provider, model, endpoint
        case apiKey = "api_key"
        case routerEnabled = "router_enabled"
    }

    init(
        provider: String? = nil,
        model: String? = nil,
        endpoint: String? = nil,
        apiKey: String? = nil,
        routerEnabled: LooseBool? = nil
    ) {
        self.provider = provider
        self.model = model
        self.endpoint = endpoint
        self.apiKey = apiKey
        self.routerEnabled = routerEnabled
    }

    /// Convenience - false unless `router_enabled` is present and
    /// resolved truthy via `LooseBool`
    var effectiveRouterEnabled: Bool {
        routerEnabled?.value ?? false
    }

    /// Prefer Keychain-resident copy of API key over any
    /// leftover in `config.json`. On-disk path is a migration relic
    /// - new installs never write it; existing ones get migrated
    /// on first launch via `Config.load`. Reading from Keychain on
    /// every access is fine; Apple's Security framework caches
    /// unlocked entry
    var effectiveApiKey: String? {
        #if AI
        if let stored = AiKeychain.read(), !stored.isEmpty { return stored }
        #endif
        // Fallback for migration window (Keychain write succeeded
        // but file rewrite failed): file's copy is still
        // authoritative until next load
        if let inline = apiKey, !inline.isEmpty { return inline }
        return nil
    }
}

/// A `Bool`-shaped config knob that decodes from either JSON
/// `true`/`false` literals OR typical truthy/falsy strings
/// (`"true" / "yes" / "on" / "1"` for true; anything else for
/// false). Defaults to false on unparseable shapes so a typo in
/// hand-edited config never silently means "on"
///
/// Lives at module scope (not nested in `AiConfig`) so other fields
/// can reuse it later - every Bool-shaped knob in `config.json` has
/// same dual-form problem in principle
struct LooseBool: Decodable, Equatable {
    let value: Bool

    init(_ value: Bool) {
        self.value = value
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let b = try? container.decode(Bool.self) {
            self.value = b
            return
        }
        if let s = try? container.decode(String.self) {
            switch s.trimmingCharacters(in: .whitespaces).lowercased() {
            case "true", "yes", "on", "1": self.value = true
            default: self.value = false
            }
            return
        }
        // Numeric form (`1` / `0`) some hand-edited files use -
        // accept both Int and Double for flexibility
        if let i = try? container.decode(Int.self) {
            self.value = i != 0
            return
        }
        if let d = try? container.decode(Double.self) {
            self.value = d != 0
            return
        }
        // Anything else: false. We swallow rather than throw so a
        // stray non-bool here can't fail-decode entire `ai` block
        // (which would also nuke `provider`, `model`, etc)
        self.value = false
    }
}

struct HotkeyBinding {
    let keyCode: Int
    let modifiers: [HotkeyModifier]

    /// Parse strings like "opt+shift+space", "cmd+ctrl+k", "option+tab".
    /// Accepts `+`, ` `, or `-` as separators. Case-insensitive
    static func parse(_ raw: String) -> HotkeyBinding? {
        let separators = CharacterSet(charactersIn: "+- ")
        let tokens = raw
            .lowercased()
            .components(separatedBy: separators)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }

        var modifiers: [HotkeyModifier] = []
        var keyCode: Int?

        for t in tokens {
            if let m = modifier(from: t) {
                modifiers.append(m)
            } else if let k = keyCodeFromToken(t) {
                if keyCode != nil { return nil } // only one non-modifier key
                keyCode = k
            } else {
                return nil
            }
        }
        guard let keyCode = keyCode else { return nil }
        return HotkeyBinding(keyCode: keyCode, modifiers: modifiers)
    }

    private static func modifier(from s: String) -> HotkeyModifier? {
        switch s {
        case "cmd", "command", "⌘": return .command
        case "opt", "option", "alt", "⌥": return .option
        case "ctrl", "control", "⌃": return .control
        case "shift", "⇧": return .shift
        default: return nil
        }
    }

    /// MacOS virtual key codes for subset we expose in configs
    private static func keyCodeFromToken(_ s: String) -> Int? {
        switch s {
        case "space": return 49
        case "tab":   return 48
        case "return", "enter": return 36
        case "escape", "esc": return 53
        case "delete", "backspace": return 51
        case "left":  return 123
        case "right": return 124
        case "down":  return 125
        case "up":    return 126
        case "f1":  return 122
        case "f2":  return 120
        case "f3":  return 99
        case "f4":  return 118
        case "f5":  return 96
        case "f6":  return 97
        case "f7":  return 98
        case "f8":  return 100
        case "f9":  return 101
        case "f10": return 109
        case "f11": return 103
        case "f12": return 111
        default:
            if s.count == 1, let ch = s.first {
                return keyCodeForLetter(ch)
            }
            return nil
        }
    }

    private static func keyCodeForLetter(_ c: Character) -> Int? {
        switch c {
        case "a": return 0
        case "b": return 11
        case "c": return 8
        case "d": return 2
        case "e": return 14
        case "f": return 3
        case "g": return 5
        case "h": return 4
        case "i": return 34
        case "j": return 38
        case "k": return 40
        case "l": return 37
        case "m": return 46
        case "n": return 45
        case "o": return 31
        case "p": return 35
        case "q": return 12
        case "r": return 15
        case "s": return 1
        case "t": return 17
        case "u": return 32
        case "v": return 9
        case "w": return 13
        case "x": return 7
        case "y": return 16
        case "z": return 6
        case "0": return 29
        case "1": return 18
        case "2": return 19
        case "3": return 20
        case "4": return 21
        case "5": return 23
        case "6": return 22
        case "7": return 26
        case "8": return 28
        case "9": return 25
        default: return nil
        }
    }
}
