import Foundation

/// Pure helper for splitting a result title into "typed prefix" and
/// "rest" so UI code can render them with different styles. Lives
/// in its own file, free of any SwiftUI imports, so unit-test
/// runner can exercise it without pulling in AppKit/SwiftUI linkage
enum TitleHighlight {
    struct Split: Equatable {
        let typed: String
        let rest: String
    }

    /// Returns a split when trimmed, lowercased `query` is a prefix
    /// of trimmed, lowercased `title`. Original casing is preserved
    /// in both halves of returned split so UI can render it exactly
    /// as candidate provider emitted it
    static func split(title: String, query: String) -> Split? {
        let q = query.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return nil }
        let lowerTitle = title.lowercased()
        let lowerQ = q.lowercased()
        guard lowerTitle.hasPrefix(lowerQ) else { return nil }
        // q.count counts grapheme clusters, matching how Swift
        // indexes strings. Works correctly for multi-byte code
        // points because `startIndex + q.count` is a
        // character-boundary offset
        let cut = title.index(title.startIndex, offsetBy: q.count)
        return Split(typed: String(title[..<cut]), rest: String(title[cut...]))
    }
}
