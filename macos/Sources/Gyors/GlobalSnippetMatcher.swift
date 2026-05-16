import Foundation

/// Pure logic for global snippet expander - no CGEventTap, no
/// NSPasteboard, nothing that needs AppKit or Rust FFI. Kept in its
/// own file so unit tests can compile it without dragging in the
/// whole event-tap machinery
enum GlobalSnippetMatcher {
    /// Append incoming characters to rolling buffer, stripping
    /// control chars and resetting on newline / tab (logical "end of
    /// line"). Shared with live CGEventTap so test-driven edge cases
    /// (tabs, emoji, modifier-sequence artefacts) stay honest
    static func appendToBuffer(
        _ chars: String,
        into buffer: inout String,
        max: Int
    ) {
        for scalar in chars.unicodeScalars where !scalar.properties.isDefaultIgnorableCodePoint {
            let c = Character(scalar)
            if c.isNewline || c == "\t" {
                buffer.removeAll(keepingCapacity: true)
                continue
            }
            buffer.append(c)
        }
        if buffer.count > max {
            buffer.removeFirst(buffer.count - max)
        }
    }

    /// Hit returned by `findTriggerMatch`. `text` is replacement and
    /// `deleteCount` is how many characters to rub out from upstream
    /// input (the `;trigger;` user just typed)
    struct TriggerMatch: Equatable {
        let text: String
        let deleteCount: Int
    }

    /// Scan tail of `buffer` for `;<trigger>;`. If last character is
    /// `;` and theres a matching opening `;` within `longestTrigger
    /// + 1` characters of end, and enclosed token is a known
    /// trigger, return replacement + how many characters to delete.
    /// Returns nil for all other shapes - including "opening `;`
    /// found but trigger unknown" case, where we explicitly do NOT
    /// keep walking back (failed trigger's closing `;` is just a
    /// literal character)
    static func findTriggerMatch(
        buffer: String,
        longestTrigger: Int,
        snippets: [String: String]
    ) -> TriggerMatch? {
        guard buffer.last == ";", buffer.count >= 3 else { return nil }
        let body = buffer.dropLast() // trailing `;`
        let maxSpan = longestTrigger + 1
        let floor = body.index(
            body.endIndex,
            offsetBy: -min(maxSpan, body.count)
        )
        var cursor = body.endIndex
        while cursor > floor {
            cursor = body.index(before: cursor)
            if body[cursor] == ";" {
                let start = body.index(after: cursor)
                let trigger = String(body[start..<body.endIndex])
                if let text = snippets[trigger], !trigger.isEmpty {
                    return TriggerMatch(text: text, deleteCount: trigger.count + 2)
                }
                return nil
            }
        }
        return nil
    }
}
