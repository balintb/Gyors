import Foundation

/// Pure decision: would a -> keystroke move caret, or has it
/// already reached end of text?
///
/// Treats any active selection as not-at-end so that -> collapses
/// selection in the standard NSTextField way before any "menu"
/// semantics could kick in
///
/// Lives in its own file (rather than as a static on
/// `KeyCatcherView`) so test harness - which excludes SwiftUI-bound
/// files - can link against it directly
enum CaretPosition {
    static func caretIsAtEnd(textLength: Int, selectedRange: NSRange) -> Bool {
        return selectedRange.length == 0 && selectedRange.location == textLength
    }
}
