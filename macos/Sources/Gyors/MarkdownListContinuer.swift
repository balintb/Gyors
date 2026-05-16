import Foundation

/// Pure (no AppKit) helper markdown editor's Coordinator delegates
/// to for "what should pressing Enter on this line produce?". Split
/// out so we can unit-test it cheaply - NSTextView integration is a
/// few lines; grammar decisions are where bugs hide
enum MarkdownListContinuer {
    enum Action: Equatable {
        /// Not a list/quote line - let default newline happen
        case none
        /// Insert `\n<prefix>` at cursor - continues list
        case insert(String)
        /// Empty marker on an otherwise-bare line -> drop marker
        /// and insert a plain newline. Signals "I'm done with list"
        case terminate
    }

    /// `beforeCursor` is text from start of current line up to
    /// cursor position. Whitespace prefix (indent) is preserved so
    /// sub-lists stay sub-lists when user Enters at end of
    /// `  - item`
    static func continuation(beforeCursor line: String) -> Action {
        let indent = line.prefix(while: { $0 == " " || $0 == "\t" })
        let rest = line.dropFirst(indent.count)

        // Bullet lists - `- `, `* `, `+ `. Two-character markers
        for marker in ["- ", "* ", "+ "] {
            if rest.hasPrefix(marker) {
                let body = rest.dropFirst(marker.count)
                if body.isEmpty { return .terminate }
                return .insert("\(indent)\(marker)")
            }
        }

        // Numbered list: digits + `. ` + body. Increment number
        // for new item; empty body terminates
        if let numbered = parseNumbered(rest) {
            if numbered.body.isEmpty { return .terminate }
            return .insert("\(indent)\(numbered.next). ")
        }

        // Blockquote - `> `. Same termination rule
        if rest.hasPrefix("> ") {
            let body = rest.dropFirst(2)
            if body.isEmpty { return .terminate }
            return .insert("\(indent)> ")
        }

        return .none
    }

    /// Parse `123. body` where body may be empty. `next` is
    /// incremented number to emit for following item; overflow
    /// wraps via `&+` but that's academic - practical notes never
    /// hit UInt.max
    private static func parseNumbered(_ s: Substring) -> (next: UInt, body: Substring)? {
        var i = s.startIndex
        while i < s.endIndex, s[i].isNumber {
            i = s.index(after: i)
        }
        guard i > s.startIndex else { return nil }
        let digits = s[s.startIndex..<i]
        guard i < s.endIndex, s[i] == "." else { return nil }
        i = s.index(after: i)
        guard i < s.endIndex, s[i] == " " else { return nil }
        let bodyStart = s.index(after: i)
        let current = UInt(digits) ?? 0
        let next = current &+ 1
        return (next: next, body: s[bodyStart..<s.endIndex])
    }
}
