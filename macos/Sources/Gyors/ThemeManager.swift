import Combine
import Foundation
import SwiftUI

/// Tracks active theme and writes selection back to `config.json`
@MainActor
final class ThemeManager: ObservableObject {
    static let shared = ThemeManager()

    @Published private(set) var current: Theme
    @Published private(set) var customThemes: [Theme] = []

    var allThemes: [Theme] {
        Themes.all + customThemes
    }

    init(initial: Theme? = nil) {
        // Compute into a local first so initializer can reference it
        // while `self` is still partially initialized
        let custom = ThemeImporter.loadCustomThemes()
        if let initial = initial {
            self.current = initial
        } else if let id = Config.load().theme,
                  let theme = Themes.byId(id) ?? custom.first(where: { $0.id == id }) {
            self.current = theme
        } else {
            self.current = Themes.system
        }
        self.customThemes = custom
    }

    /// Rescan custom themes directory. Call after importing
    func reloadCustomThemes() {
        customThemes = ThemeImporter.loadCustomThemes()
    }

    func apply(_ theme: Theme) {
        guard theme.id != current.id else { return }
        current = theme
        persist(theme.id)
    }

    func apply(id: String) {
        if let theme = Themes.byId(id) ?? customThemes.first(where: { $0.id == id }) {
            apply(theme)
        }
    }

    /// Delete on-disk file backing a custom theme + drop it from
    /// in-memory list. Returns `true` when a file was actually
    /// removed; `false` on unknown id, malformed id, or path-escape
    /// attempt (last is defence-in-depth - importer already
    /// constrains saved ids, but a hand-edited filename in themes
    /// folder could otherwise be passed back through here as menu
    /// item's `representedObject`)
    ///
    /// If `id` matches currently-active theme, fall back to
    /// `Themes.system` so user is never stranded looking at a theme
    /// whose definition just disappeared from disk
    @discardableResult
    func remove(id: String) -> Bool {
        let outcome = Self.deleteThemeFile(id: id, in: ThemeImporter.themesDir())
        guard outcome == .deleted else { return false }
        if current.id == id {
            // Revert BEFORE reloading list so `current` doesn't
            // dangle for even one observer cycle pointing at an
            // absent theme
            current = Themes.system
            persist(Themes.system.id)
        }
        reloadCustomThemes()
        return true
    }

    enum DeleteOutcome: Equatable {
        /// File found + deleted
        case deleted
        /// Id failed safety check (rejected before touching disk)
        case unsafeId
        /// Path resolution drifted outside `dir` (symlink/Unicode/etc)
        case pathEscaped
        /// `dir/<id>.json` didn't exist
        case fileMissing
        /// `removeItem` threw
        case ioError(String)
    }

    /// Filesystem half of `remove(id:)`. Pulled out as `static` so
    /// unit tests can drive deletion against a tempdir without
    /// having to spin up a full `ThemeManager` (which is a
    /// `@MainActor` singleton tied to `Config`/`Themes.system`).
    /// `nonisolated` because it touches only its arguments +
    /// filesystem; no shared MainActor state
    nonisolated static func deleteThemeFile(id: String, in dir: URL) -> DeleteOutcome {
        guard isSafeThemeId(id) else { return .unsafeId }
        let target = dir.appendingPathComponent("\(id).json")

        // Confirm resolved path is still inside themes directory.
        // If a Unicode-normalisation edge case or a symlink in
        // themes dir lets `appendingPathComponent` escape, refuse
        // rather than `rm` wrong file
        let resolvedTarget = target.standardizedFileURL.resolvingSymlinksInPath()
        let resolvedDir = dir.standardizedFileURL.resolvingSymlinksInPath()
        let dirPrefix = resolvedDir.path.hasSuffix("/")
            ? resolvedDir.path
            : resolvedDir.path + "/"
        guard resolvedTarget.path.hasPrefix(dirPrefix) else {
            return .pathEscaped
        }

        guard FileManager.default.fileExists(atPath: target.path) else {
            return .fileMissing
        }
        do {
            try FileManager.default.removeItem(at: target)
        } catch {
            return .ioError("\(error)")
        }
        return .deleted
    }

    /// Allowlist matching import-side filename sanitisation
    /// (`[A-Za-z0-9_-]{1,64}`). Anything outside this charset can't
    /// have been saved by importer in the first place, so refusing
    /// it on delete is consistent rather than restrictive
    nonisolated static func isSafeThemeId(_ id: String) -> Bool {
        guard !id.isEmpty, id.count <= 64 else { return false }
        return id.allSatisfy { c in
            c.isASCII && (c.isLetter || c.isNumber || c == "-" || c == "_")
        }
    }

    /// Writes `theme` into config.json, preserving other keys
    private func persist(_ id: String) {
        let url = Config.configURL()
        var root: [String: Any] = [:]
        if let data = try? Data(contentsOf: url),
           let parsed = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
            root = parsed
        }
        root["theme"] = id
        guard let data = try? JSONSerialization.data(
            withJSONObject: root,
            options: [.prettyPrinted, .sortedKeys]
        ) else { return }
        try? FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try? data.write(to: url, options: [.atomic])
    }
}
