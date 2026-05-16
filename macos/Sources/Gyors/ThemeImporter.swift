import AppKit
import Foundation

/// Imports user themes delivered over `gyors://` URL scheme
///
/// URL shape: `gyors://theme?import=<base64>` where base64 payload is
/// UTF-8 JSON matching `CustomTheme`. Imported themes are written to
/// `~/Library/Application Support/Gyors/themes/<id>.json` and picked
/// up by `ThemeManager` on next reload
enum ThemeImporter {
    enum Error: Swift.Error, LocalizedError {
        case wrongScheme
        case wrongHost
        case missingQueryParam
        case oversizedPayload(bytes: Int)
        case invalidBase64
        case invalidJson(String)
        case invalidTheme(String)
        case unsafeId(String)
        case ioError(String)

        var errorDescription: String? {
            switch self {
            case .wrongScheme:       return "URL is not a gyors:// URL."
            case .wrongHost:         return "URL host must be 'theme'."
            case .missingQueryParam: return "URL is missing the ?import=… parameter."
            case .oversizedPayload(let bytes):
                return "Theme payload is \(bytes) bytes, over the \(ThemeImporter.MAX_THEME_BASE64_BYTES)-byte cap."
            case .invalidBase64:     return "The import payload is not valid base64."
            case .invalidJson(let s): return "The payload is not valid theme JSON: \(s)"
            case .invalidTheme(let s): return "The theme is malformed: \(s)"
            case .unsafeId(let s):   return "Theme id \"\(s)\" is not allowed (use A-Z, a-z, 0-9, -, _; 1-64 chars)."
            case .ioError(let s):    return "File error: \(s)"
            }
        }
    }

    /// Cap base64 payload before allocating decoded blob.
    /// A 100MB base64 in a `gyors://theme?import=...` URL would
    /// otherwise allocate ~75MB on decode then ANOTHER `Data` clone
    /// for JSON pass. Real custom themes are a few kilobytes; 512KB
    /// leaves room for tomorrow's richer theme schema without
    /// blessing OOM payloads today
    static let MAX_THEME_BASE64_BYTES = 512 * 1024

    /// Directory where custom themes live
    static func themesDir() -> URL {
        let base: URL
        if let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first {
            base = support.appendingPathComponent("Gyors", isDirectory: true)
                .appendingPathComponent("themes", isDirectory: true)
        } else {
            base = URL(fileURLWithPath: NSHomeDirectory())
                .appendingPathComponent("Library/Application Support/Gyors/themes", isDirectory: true)
        }
        try? FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        return base
    }

    /// Attempt to import from a full gyors:// URL. Persists on success
    static func importFromURL(_ url: URL) -> Result<Theme, Error> {
        guard url.scheme == "gyors" else { return .failure(.wrongScheme) }
        guard url.host == "theme" else { return .failure(.wrongHost) }
        let components = URLComponents(url: url, resolvingAgainstBaseURL: false)
        guard let b64 = components?.queryItems?.first(where: { $0.name == "import" })?.value,
              !b64.isEmpty
        else {
            return .failure(.missingQueryParam)
        }
        return importFromBase64(b64)
    }

    /// Attempt to import from just base64 payload (production path -
    /// writes to `themesDir()`)
    static func importFromBase64(_ base64: String) -> Result<Theme, Error> {
        importFromBase64(base64, into: themesDir())
    }

    /// Dir-overriding variant. Tests use this against a `mkdtemp`
    /// directory so suite never writes into user's real
    /// `~/Library/Application Support/Gyors/themes/`
    static func importFromBase64(_ base64: String, into dir: URL) -> Result<Theme, Error> {
        // Refuse oversized base64 BEFORE decode. Payload
        // travels in URL query so it's already in memory by time we
        // get here, but decoding still allocates ~75% of input
        // again. Cap input length
        let byteLength = base64.utf8.count
        guard byteLength <= MAX_THEME_BASE64_BYTES else {
            return .failure(.oversizedPayload(bytes: byteLength))
        }
        guard let data = Data(base64Encoded: base64, options: [.ignoreUnknownCharacters]) else {
            return .failure(.invalidBase64)
        }
        let custom: CustomTheme
        do {
            custom = try JSONDecoder().decode(CustomTheme.self, from: data)
        } catch {
            return .failure(.invalidJson("\(error)"))
        }
        let theme: Theme
        do {
            theme = try custom.toTheme()
        } catch {
            return .failure(.invalidTheme(error.localizedDescription))
        }
        do {
            try save(payload: data, id: theme.id, in: dir)
        } catch let e as Error {
            return .failure(e)
        } catch {
            return .failure(.ioError("\(error)"))
        }
        return .success(theme)
    }

    /// Serialize theme to a `gyors://` URL users can share
    static func url(for theme: Theme) -> URL? {
        let custom = CustomTheme.from(theme: theme)
        guard let data = try? JSONEncoder().encode(custom) else { return nil }
        let b64 = data.base64EncodedString()
        guard var components = URLComponents(string: "gyors://theme") else { return nil }
        components.queryItems = [URLQueryItem(name: "import", value: b64)]
        return components.url
    }

    /// Load every theme JSON in themes directory. Invalid files are
    /// silently skipped (logged) - we dont want one bad theme to
    /// break menu
    static func loadCustomThemes() -> [Theme] {
        let dir = themesDir()
        guard let entries = try? FileManager.default.contentsOfDirectory(
            at: dir,
            includingPropertiesForKeys: nil
        ) else { return [] }
        return entries.compactMap { url -> Theme? in
            guard url.pathExtension.lowercased() == "json",
                  let data = try? Data(contentsOf: url) else {
                return nil
            }
            do {
                let custom = try JSONDecoder().decode(CustomTheme.self, from: data)
                return try custom.toTheme()
            } catch {
                NSLog("gyors: skipping custom theme \(url.lastPathComponent): \(error)")
                return nil
            }
        }
        .sorted(by: { $0.label.lowercased() < $1.label.lowercased() })
    }

    /// Pinned for tests: `save(payload:id:in:)` is dir-overriding
    /// form so regression suite can write to a tempdir without
    /// touching user's real `~/Library/Application Support/`. Id is
    /// validated against `ThemeManager.isSafeThemeId` BEFORE
    /// touching disk - this is import-side hardening that
    /// backs on-delete defence-in-depth check. Both must agree on
    /// charset (`[A-Za-z0-9_-]{1,64}`) or imports could write files
    /// delete code refuses to read
    static func save(payload: Data, id: String, in dir: URL) throws {
        guard ThemeManager.isSafeThemeId(id) else {
            throw Error.unsafeId(id)
        }
        let path = dir.appendingPathComponent("\(id).json")
        try payload.write(to: path, options: [.atomic])
    }
}
