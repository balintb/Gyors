import Foundation

/// Production loader for `ConfigSchema` - separated from
/// `ConfigSchema` itself so Swift test harness (which links no Rust
/// lib) can install a fixture via `ConfigSchema.installFromJson(...)`
/// without pulling in `gyors_config_fields_json`
enum ConfigSchemaFfiLoader {
    /// Fetch schema from Rust FFI and install it. Safe to call
    /// multiple times; each call replaces cache
    static func loadFromFfi() {
        guard let raw = gyors_config_fields_json() else {
            NSLog("gyors: gyors_config_fields_json returned null")
            return
        }
        defer { gyors_free_string(raw) }
        let json = String(cString: raw)
        ConfigSchema.installFromJson(json)
    }
}
