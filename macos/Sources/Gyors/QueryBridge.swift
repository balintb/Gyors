import Foundation

/// Abstraction ViewModel queries. Implemented by `GyorsBridge` in
/// production and by a mock in tests
protocol QueryBridge: AnyObject {
    func query(_ pattern: String) -> [Candidate]
    @discardableResult
    func activate(_ id: String, action: String) -> Effect?
    var appCount: UInt64 { get }
    var clipboardCount: UInt64 { get }
    func recordClipboard(_ content: String)
    func clearClipboardHistory()
    /// Absolute path of configured notes folder; used by editor's
    /// Rename / Move flow to resolve user-typed relative paths
    /// safely
    var notesFolder: String { get }
    /// Runtime state dump (notes folder, scan count, providers).
    /// Used by menu-bar "Diagnostics..." item and startup logging.
    /// Default implementation returns nil so test mocks dont have
    /// to implement method
    func diagnosticsJson() -> String?
    /// Persist a committed query (user pressed Enter). Recording
    /// on every keystroke would flood ring with half-typed noise,
    /// so only completed, activated queries land here
    func recordQuery(_ pattern: String)
    /// Most recent unique queries, newest first. Backs up-arrow
    /// recall in `MainInputField`
    func recentQueries(limit: Int) -> [String]
    /// Most recent query - what `!!` expands to
    func lastQuery() -> String
}

extension QueryBridge {
    func diagnosticsJson() -> String? { nil }
    func recordQuery(_ pattern: String) {}
    func recentQueries(limit: Int) -> [String] { [] }
    func lastQuery() -> String { "" }
}
