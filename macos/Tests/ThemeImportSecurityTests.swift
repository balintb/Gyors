import Foundation

/// Regression suite
///
/// Original importer sanitised only `/` and trusted rest of
/// JSON-supplied id, which let `..`, NUL, and unusual Unicode
/// reach `appendingPathComponent`. The on-delete path validates via
/// `ThemeManager.isSafeThemeId` (covered in `ThemeRemoveTests`); this
/// pins the symmetric defence on import
///
/// Two layers exercised:
/// - `ThemeImporter.save(payload:id:in:)` direct unit calls
/// - `ThemeImporter.importFromBase64(_:into:)` end-to-end through a
///   real base64 payload, so we know a hostile JSON `id` field is
///   refused before it touches disk
func runThemeImportSecurityTests() {


    runGroup("save: valid id writes the JSON file") {
        let dir = makeTempImportDir()
        defer { try? FileManager.default.removeItem(at: dir) }
        let payload = Data("ok".utf8)
        do {
            try ThemeImporter.save(payload: payload, id: "dracula-2", in: dir)
            let path = dir.appendingPathComponent("dracula-2.json")
            expect(
                FileManager.default.fileExists(atPath: path.path),
                "file written at \(path.path)"
            )
            let read = try Data(contentsOf: path)
            expect(read == payload, "payload matches what was written")
        } catch {
            expect(false, "save threw unexpectedly: \(error)")
        }
    }

    runGroup("save: rejects every unsafe id charset BEFORE touching disk") {
        let dir = makeTempImportDir()
        defer { try? FileManager.default.removeItem(at: dir) }
        // Plant a real sibling so a missed validation would write
        // OVER (or next to) it - assertion `sibling still
        // present and untouched` catches both partial and complete
        // bypasses
        let sibling = dir.appendingPathComponent("safe.json")
        try? Data("untouched".utf8).write(to: sibling)

        let badIds: [(String, String)] = [
            ("", "empty"),
            ("..", "dot-dot"),
            ("../etc", "traversal"),
            ("a/b", "embedded slash"),
            ("a\\b", "backslash"),
            (".hidden", "leading dot"),
            ("a b", "whitespace"),
            ("foo\0bar", "embedded NUL"),
            ("\0", "lone NUL"),
            ("café", "non-ASCII letter"),
            ("🌙", "emoji"),
            (String(repeating: "a", count: 65), "over-cap 65 chars"),
            ("a.json", "dot in id (would collapse `.json` ext)"),
        ]
        for (bad, label) in badIds {
            do {
                try ThemeImporter.save(payload: Data("x".utf8), id: bad, in: dir)
                expect(false, "should have thrown for `\(label)`")
            } catch let e as ThemeImporter.Error {
                switch e {
                case .unsafeId(let echoed):
                    expect(echoed == bad, "echoed id matches input for `\(label)`")
                default:
                    expect(false, "wrong error for `\(label)`: \(e)")
                }
            } catch {
                expect(false, "non-ThemeImporter error for `\(label)`: \(error)")
            }
        }

        // Sibling must survive every rejected save above
        expect(
            FileManager.default.fileExists(atPath: sibling.path),
            "sibling MUST survive all rejected saves"
        )
        let surviving = try? Data(contentsOf: sibling)
        expect(surviving == Data("untouched".utf8), "sibling content unchanged")
    }

    runGroup("save: 64-char id at the boundary is accepted") {
        let dir = makeTempImportDir()
        defer { try? FileManager.default.removeItem(at: dir) }
        let boundary = String(repeating: "a", count: 64)
        do {
            try ThemeImporter.save(payload: Data("x".utf8), id: boundary, in: dir)
            let path = dir.appendingPathComponent("\(boundary).json")
            expect(
                FileManager.default.fileExists(atPath: path.path),
                "64-char id wrote successfully"
            )
        } catch {
            expect(false, "64-char id rejected: \(error)")
        }
    }


    runGroup("importFromBase64: hostile id in JSON surfaces .unsafeId") {
        // A JSON payload that asks importer to write to
        // `../etc/passwd.json` - saved file would land OUTSIDE
        // themes dir if validation didn't fire. We assert
        // failure type AND verify nothing was written either
        // inside dir or one level up
        let dir = makeTempImportDir()
        defer { try? FileManager.default.removeItem(at: dir) }
        let parent = dir.deletingLastPathComponent()
        let beforeParentEntries = (try? FileManager.default.contentsOfDirectory(
            atPath: parent.path
        )) ?? []

        let payload = themeJson(id: "../escape")
        let b64 = payload.base64EncodedString()
        let result = ThemeImporter.importFromBase64(b64, into: dir)
        switch result {
        case .failure(let e):
            switch e {
            case .unsafeId(let echoed):
                expect(echoed == "../escape", "echoed id is `../escape`, got `\(echoed)`")
            default:
                expect(false, "expected .unsafeId, got \(e)")
            }
        case .success:
            expect(false, "import unexpectedly succeeded for `../escape`")
        }

        // No file inside dir
        let inside = (try? FileManager.default.contentsOfDirectory(atPath: dir.path)) ?? []
        expect(inside.isEmpty, "no file written inside themes dir, got \(inside)")
        // No new entry in parent
        let afterParentEntries = (try? FileManager.default.contentsOfDirectory(
            atPath: parent.path
        )) ?? []
        expect(
            Set(afterParentEntries) == Set(beforeParentEntries),
            "parent dir entries unchanged"
        )
    }

    runGroup("importFromBase64: NUL byte id surfaces .unsafeId") {
        let dir = makeTempImportDir()
        defer { try? FileManager.default.removeItem(at: dir) }
        let payload = themeJson(id: "foo\u{0000}bar")
        let b64 = payload.base64EncodedString()
        let result = ThemeImporter.importFromBase64(b64, into: dir)
        switch result {
        case .failure(.unsafeId(let echoed)):
            expect(echoed.contains("\u{0000}"), "echoed id preserved NUL")
        case .failure(let e):
            expect(false, "expected .unsafeId, got \(e)")
        case .success:
            expect(false, "import accepted NUL-byte id - should refuse")
        }
        let inside = (try? FileManager.default.contentsOfDirectory(atPath: dir.path)) ?? []
        expect(inside.isEmpty, "no file written for NUL-byte id")
    }

    runGroup("importFromBase64: valid id writes through to disk") {
        let dir = makeTempImportDir()
        defer { try? FileManager.default.removeItem(at: dir) }
        let payload = themeJson(id: "lavender-2")
        let b64 = payload.base64EncodedString()
        let result = ThemeImporter.importFromBase64(b64, into: dir)
        switch result {
        case .success(let theme):
            expect(theme.id == "lavender-2", "round-trip preserved id")
            let path = dir.appendingPathComponent("lavender-2.json")
            expect(
                FileManager.default.fileExists(atPath: path.path),
                "happy-path file landed in tempdir"
            )
        case .failure(let e):
            expect(false, "valid theme import failed: \(e)")
        }
    }

    runGroup("importFromBase64: invalid JSON returns .invalidJson NOT .unsafeId") {
        // Error hierarchy must not collapse - .unsafeId should
        // only fire for id-charset failure, not for general
        // payload corruption. A future contributor might be tempted
        // to validate id BEFORE JSON parse for "safety"; pin the
        // current order so error messages stay accurate
        let dir = makeTempImportDir()
        defer { try? FileManager.default.removeItem(at: dir) }
        let garbage = Data("not json at all".utf8).base64EncodedString()
        let result = ThemeImporter.importFromBase64(garbage, into: dir)
        switch result {
        case .failure(.invalidJson):
            break // expected
        case .failure(let e):
            expect(false, "expected .invalidJson, got \(e)")
        case .success:
            expect(false, "garbage payload unexpectedly succeeded")
        }
    }


    runGroup("importFromBase64: payload cap is 512KB") {
        // Pin cap so silent relaxation triggers a test edit
        expect(
            ThemeImporter.MAX_THEME_BASE64_BYTES == 512 * 1024,
            "cap is 512KB"
        )
    }

    runGroup("importFromBase64: under-cap payload still imports") {
        let dir = makeTempImportDir()
        defer { try? FileManager.default.removeItem(at: dir) }
        // A 10KB padded label produces ~13KB of base64 - well
        // under 512KB cap. Confirms cap doesn't break
        // legitimate-but-larger custom themes (gradient-heavy
        // hand-authored themes can run 5-10KB)
        let pad = String(repeating: "a", count: 10 * 1024)
        let json = themeJson(id: "padded", labelPadding: pad)
        let b64 = json.base64EncodedString()
        let result = ThemeImporter.importFromBase64(b64, into: dir)
        switch result {
        case .success(let theme):
            expect(theme.id == "padded", "padded theme imports")
        case .failure(let e):
            expect(false, "padded import failed unexpectedly: \(e)")
        }
    }

    runGroup("importFromBase64: over-cap payload surfaces .oversizedPayload") {
        // Build a base64 string just over cap. The check runs
        // BEFORE decode, so we dont need a valid JSON underneath
        // to exercise path - we want to confirm we refuse the
        // allocation, not that we'd decode a corrupt body
        let dir = makeTempImportDir()
        defer { try? FileManager.default.removeItem(at: dir) }
        // 600 KB of base64 alphabet chars = valid-ish base64
        // structure (Data(base64:) might still reject it as not
        // matching real content, but we error out before that)
        let oversized = String(repeating: "A", count: 600 * 1024)
        let result = ThemeImporter.importFromBase64(oversized, into: dir)
        switch result {
        case .failure(.oversizedPayload(let bytes)):
            expect(bytes == 600 * 1024, "echoed byte count matches input")
        case .failure(let e):
            expect(false, "expected .oversizedPayload, got \(e)")
        case .success:
            expect(false, "import accepted 600KB blob regression")
        }
        // No file landed in dir
        let entries = (try? FileManager.default.contentsOfDirectory(atPath: dir.path)) ?? []
        expect(entries.isEmpty, "over-cap import must not touch disk")
    }

    runGroup("importFromBase64: exact-cap payload accepted (boundary)") {
        // Boundary: a payload of exactly MAX_THEME_BASE64_BYTES
        // bytes passes size check. Whether it then decodes to
        // valid JSON depends on content - we use a string-of-A
        // here knowing that decode + JSON parse will fail. The
        // EXACT failure mode (invalidBase64 or invalidJson)
        // doesn't matter for this test; point is we got PAST
        // .oversizedPayload gate
        let dir = makeTempImportDir()
        defer { try? FileManager.default.removeItem(at: dir) }
        let exact = String(repeating: "A", count: ThemeImporter.MAX_THEME_BASE64_BYTES)
        let result = ThemeImporter.importFromBase64(exact, into: dir)
        switch result {
        case .failure(.oversizedPayload):
            expect(false, "exact-cap MUST NOT be rejected as oversized")
        default:
            break // .invalidJson / .invalidBase64 / etc all acceptable
        }
    }
}


private func makeTempImportDir() -> URL {
    let tmpRoot = FileManager.default.temporaryDirectory
    let unique = "gyors-import-test-\(ProcessInfo.processInfo.processIdentifier)-\(UUID().uuidString.prefix(8))"
    let url = tmpRoot.appendingPathComponent(unique, isDirectory: true)
    try? FileManager.default.createDirectory(
        at: url,
        withIntermediateDirectories: true
    )
    return url
}

/// Build a minimal-but-valid CustomTheme JSON blob with an arbitrary
/// `id`. Tests need full control over id (including hostile
/// values that encoder would also accept) - hand-rolled JSON is
/// simplest way. `labelPadding` lets size-cap tests bulk
/// payload up to known sizes without changing other fields
private func themeJson(id: String, labelPadding: String = "") -> Data {
    // JSON-escape id so embedded NUL bytes, quotes, etc. dont
    // break encoding round-trip
    let escaped = jsonEscape(id)
    let label = "Test Theme" + labelPadding
    let json = """
        {
            "id": "\(escaped)",
            "label": "\(label)",
            "uses_system_material": false,
            "panel_tint": "#101010",
            "accent": "#7F7FFF",
            "primary_text": "#FFFFFF",
            "secondary_text": "#AAAAAA",
            "tertiary_text": "#666666",
            "border": "#202020",
            "border_width": 1.0,
            "corner_radius": 8.0,
            "selection_opacity": 0.18
        }
        """
    return Data(json.utf8)
}

private func jsonEscape(_ s: String) -> String {
    var out = ""
    out.reserveCapacity(s.count)
    for scalar in s.unicodeScalars {
        switch scalar {
        case "\"": out += "\\\""
        case "\\": out += "\\\\"
        case "\n": out += "\\n"
        case "\r": out += "\\r"
        case "\t": out += "\\t"
        case let s where s.value < 0x20:
            out += String(format: "\\u%04x", s.value)
        default:
            out.append(Character(scalar))
        }
    }
    return out
}
