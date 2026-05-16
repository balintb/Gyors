import Foundation

/// Mirrors Rust's `FIELDS` constant (exposed via
/// `gyors_config_fields_json` FFI). A single source of truth for
/// known config keys - adding a new option in Rust automatically
/// teaches Swift about it, so `gyors://set/*` and `gyors://toggle/*`
/// work for it without a hardcoded enum change here
struct ConfigField: Decodable {
    /// Canonical JSON key: `clipboard_enabled`, `ai.provider`, ..
    let key: String
    /// URL-slug form: `clipboard-enabled`, `ai-provider`, ..
    let slug: String
    let type: FieldType
    let `default`: String
    let description: String
    let enumValues: [String]?

    enum FieldType: String, Decodable {
        case bool, text, path, `enum`
    }

    enum CodingKeys: String, CodingKey {
        case key, slug, type, description
        case `default`
        case enumValues = "enum_values"
    }
}

enum ConfigSchema {
    /// Loaded once at startup from Rust FFI. Empty before init -
    /// callers that need schema early (before `AppDelegate` has
    /// run) get an empty list and skip whatever they'd have done
    private static var cachedFields: [ConfigField] = []

    /// Inject a schema directly - used by tests that dont link
    /// Rust FFI. In production, call `loadFromFfi()` instead
    static func install(_ fields: [ConfigField]) {
        cachedFields = fields
    }

    /// Parse a JSON blob from FFI. Exposed for tests that want to
    /// assert Rust->Swift schema decode end-to-end without wiring
    /// an FFI linker dance
    static func installFromJson(_ json: String) {
        guard let data = json.data(using: .utf8),
              let parsed = try? JSONDecoder().decode([ConfigField].self, from: data)
        else {
            NSLog("gyors: couldn't decode config schema JSON")
            return
        }
        cachedFields = parsed
    }

    static var all: [ConfigField] { cachedFields }

    /// Look up a field by its URL slug (`preview-markdown`) or its
    /// canonical JSON key (`preview_markdown`). Both are accepted
    /// because users may construct URLs either way
    static func field(bySlug slug: String) -> ConfigField? {
        let lowered = slug.lowercased()
        return cachedFields.first { $0.slug == lowered || $0.key == lowered }
    }

    /// Every field whose type is boolean - these are the legal
    /// targets for `gyors://toggle/*`
    static var booleanSlugs: Set<String> {
        Set(cachedFields.filter { $0.type == .bool }.map { $0.slug })
    }
}
