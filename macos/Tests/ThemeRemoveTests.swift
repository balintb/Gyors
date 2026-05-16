import Foundation

/// Regressions for the "remove custom theme" flow in
/// `ThemeManager.swift`. The on-disk half (id allowlist, path-escape
/// guard, file deletion) is exercised via the static
/// `ThemeManager.deleteThemeFile(id:in:)` helper against a tempdir so
/// test never touches `~/Library/Application Support/Gyors/`
///
/// The "fall back to default theme on remove of active" logic is
/// pinned at a higher level: only file-side outcomes can be
/// driven from non-MainActor test land here; the MainActor revert
/// path is one straight-line if-clause covered by the existence of
/// `current = Themes.system` immediately after `deleteThemeFile`
/// reports `.deleted` (visible in `remove(id:)` itself)
func runThemeRemoveTests() {


    runGroup("themeId: ASCII letters / digits / dash / underscore allowed") {
        expect(ThemeManager.isSafeThemeId("dracula"),       "plain")
        expect(ThemeManager.isSafeThemeId("solarized-dark"), "with dash")
        expect(ThemeManager.isSafeThemeId("my_theme_1"),    "underscore + digit")
        expect(ThemeManager.isSafeThemeId("A"),             "single char")
        expect(ThemeManager.isSafeThemeId("abc123"),        "alphanumeric")
        expect(
            ThemeManager.isSafeThemeId(String(repeating: "a", count: 64)),
            "exactly 64 chars - boundary"
        )
    }

    runGroup("themeId: empty + over-64 + non-ASCII rejected") {
        expect(!ThemeManager.isSafeThemeId(""),           "empty")
        expect(!ThemeManager.isSafeThemeId(String(repeating: "a", count: 65)), "65 chars")
        expect(!ThemeManager.isSafeThemeId("café"),       "non-ASCII letter")
        expect(!ThemeManager.isSafeThemeId("🌙"),         "emoji")
    }

    runGroup("themeId: path-traversal characters rejected") {
        // The import-side `replacingOccurrences(of: "/", with: "_")`
        // means none of these can have been saved by Gyors. They're
        // here so a hand-crafted menu item's representedObject still
        // can't escape the themes directory
        expect(!ThemeManager.isSafeThemeId(".."),         "dot-dot")
        expect(!ThemeManager.isSafeThemeId("../etc"),     "traversal")
        expect(!ThemeManager.isSafeThemeId("a/b"),        "embedded slash")
        expect(!ThemeManager.isSafeThemeId("a\\b"),       "backslash")
        expect(!ThemeManager.isSafeThemeId(".hidden"),    "leading dot")
        expect(!ThemeManager.isSafeThemeId("a b"),        "whitespace")
        expect(!ThemeManager.isSafeThemeId("a.json"),     "dot in id")
    }

    runGroup("themeId: NUL byte rejected") {
        // POSIX truncates C strings at NUL, so a Swift String
        // containing a NUL could pass through `appendingPathComponent`
        // and write to a different file than the visible name. Reject
        expect(!ThemeManager.isSafeThemeId("foo\0bar"),   "NUL in middle")
        expect(!ThemeManager.isSafeThemeId("\0"),         "lone NUL")
    }


    runGroup("deleteThemeFile: present file is removed") {
        let dir = makeTempThemesDir()
        defer { try? FileManager.default.removeItem(at: dir) }
        let id = "dracula"
        plantTheme(id, in: dir)
        expect(
            FileManager.default.fileExists(atPath: dir.appendingPathComponent("\(id).json").path),
            "pre-condition: plant succeeded"
        )

        let outcome = ThemeManager.deleteThemeFile(id: id, in: dir)
        expect(outcome == .deleted, "outcome = \(outcome)")
        expect(
            !FileManager.default.fileExists(atPath: dir.appendingPathComponent("\(id).json").path),
            "file is gone after delete"
        )
    }

    runGroup("deleteThemeFile: missing file reports .fileMissing") {
        let dir = makeTempThemesDir()
        defer { try? FileManager.default.removeItem(at: dir) }
        let outcome = ThemeManager.deleteThemeFile(id: "never-existed", in: dir)
        expect(outcome == .fileMissing, "outcome = \(outcome)")
    }

    runGroup("deleteThemeFile: unsafe id refused before touching disk") {
        let dir = makeTempThemesDir()
        defer { try? FileManager.default.removeItem(at: dir) }
        // Plant a real sibling so test would actually be wrong
        // if the id check were bypassed (file would vanish)
        plantTheme("real", in: dir)
        for bad in ["../escape", "a/b", ".hidden", "", "a\0b", "café"] {
            let outcome = ThemeManager.deleteThemeFile(id: bad, in: dir)
            expect(outcome == .unsafeId, "for `\(bad)`, got \(outcome)")
        }
        expect(
            FileManager.default.fileExists(atPath: dir.appendingPathComponent("real.json").path),
            "sibling file MUST still exist - unsafe id branch must short-circuit"
        )
    }

    runGroup("deleteThemeFile: only deletes within the dir") {
        // Sanity that the resolved-path guard actually fires when a
        // symlink escapes. Plant a real outside file, replace the
        // dir-internal entry with a symlink that targets the outside
        // file, and confirm we refuse to delete + leave both files
        // intact. This is the / "defence in depth" scenario
        let dir = makeTempThemesDir()
        let outsideDir = makeTempThemesDir() // shares parent? No - mktemp gives independent
        defer {
            try? FileManager.default.removeItem(at: dir)
            try? FileManager.default.removeItem(at: outsideDir)
        }
        let outsidePath = outsideDir.appendingPathComponent("victim.json")
        try? Data("\"victim\"".utf8).write(to: outsidePath)
        let linkPath = dir.appendingPathComponent("hostile.json")
        // Symlink hostile.json -> victim.json (in a different dir)
        try? FileManager.default.createSymbolicLink(
            at: linkPath,
            withDestinationURL: outsidePath
        )

        let outcome = ThemeManager.deleteThemeFile(id: "hostile", in: dir)
        expect(outcome == .pathEscaped,
            "expected pathEscaped, got \(outcome)")
        expect(
            FileManager.default.fileExists(atPath: outsidePath.path),
            "the victim file outside the themes dir MUST survive"
        )
    }
}


/// Create a fresh `themes/` directory under `mkdtemp` so each test
/// gets a clean filesystem state without touching the user's real
/// `~/Library/Application Support/`. Returns the directory URL
private func makeTempThemesDir() -> URL {
    let tmpRoot = FileManager.default.temporaryDirectory
    let unique = "gyors-theme-test-\(ProcessInfo.processInfo.processIdentifier)-\(UUID().uuidString.prefix(8))"
    let url = tmpRoot.appendingPathComponent(unique, isDirectory: true)
    try? FileManager.default.createDirectory(
        at: url,
        withIntermediateDirectories: true
    )
    return url
}

private func plantTheme(_ id: String, in dir: URL) {
    let payload = """
        {"id":"\(id)","label":"\(id)","background":"#000","foreground":"#fff"}
        """
    try? Data(payload.utf8).write(
        to: dir.appendingPathComponent("\(id).json")
    )
}
