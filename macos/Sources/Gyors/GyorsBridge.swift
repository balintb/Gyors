import Foundation

final class GyorsBridge: QueryBridge {
    init() {
        gyors_init()
    }

    func query(_ pattern: String) -> [Candidate] {
        guard let raw = pattern.withCString({ gyors_query($0) }) else { return [] }
        defer { gyors_free_string(raw) }
        let json = String(cString: raw)
        guard let data = json.data(using: .utf8) else { return [] }
        return (try? JSONDecoder().decode([Candidate].self, from: data)) ?? []
    }

    /// Decodes and returns Effect produced by given activation. Caller
    /// is responsible for dispatching effect (so `setInput` can be
    /// intercepted by ViewModel to update query)
    @discardableResult
    func activate(_ id: String, action: String = "default") -> Effect? {
        var jsonString: String?
        id.withCString { idPtr in
            action.withCString { actionPtr in
                if let raw = gyors_activate(idPtr, actionPtr) {
                    jsonString = String(cString: raw)
                    gyors_free_string(raw)
                }
            }
        }
        guard let json = jsonString,
              let data = json.data(using: .utf8) else { return nil }
        return try? JSONDecoder().decode(Effect.self, from: data)
    }

    var appCount: UInt64 {
        gyors_app_count()
    }

    var clipboardCount: UInt64 {
        gyors_clipboard_count()
    }

    func recordClipboard(_ content: String) {
        content.withCString { gyors_record_clipboard($0) }
    }

    func clearClipboardHistory() {
        gyors_clear_clipboard_history()
    }

    var notesFolder: String {
        guard let raw = gyors_notes_folder() else { return "" }
        defer { gyors_free_string(raw) }
        return String(cString: raw)
    }

    /// Runtime diagnostics JSON - notes folder, scan count, provider
    /// list. Used by menu-bar "Diagnostics..." item and startup
    /// logging so users (and future-me) can see exactly what backend
    /// is looking at when something seems off. Returns nil if
    /// `gyors_init` hasn't run yet
    func diagnosticsJson() -> String? {
        guard let raw = gyors_diagnostics() else { return nil }
        defer { gyors_free_string(raw) }
        return String(cString: raw)
    }

    func recordQuery(_ pattern: String) {
        pattern.withCString { gyors_record_query($0) }
    }

    func recentQueries(limit: Int) -> [String] {
        let clamped = UInt32(max(0, min(limit, Int(UInt32.max))))
        guard let raw = gyors_recent_queries(clamped) else { return [] }
        defer { gyors_free_string(raw) }
        let json = String(cString: raw)
        guard let data = json.data(using: .utf8) else { return [] }
        return (try? JSONDecoder().decode([String].self, from: data)) ?? []
    }

    func lastQuery() -> String {
        guard let raw = gyors_last_query() else { return "" }
        defer { gyors_free_string(raw) }
        return String(cString: raw)
    }
}
