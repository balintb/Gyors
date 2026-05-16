import AppKit
import SwiftUI

/// Markdown-aware text editor - a `TextEditor` replacement with live
/// syntax highlighting. Applies same token-span logic preview uses
/// (`SyntaxHighlighter.markdownSpans`) to `NSTextStorage`, so
/// keystrokes recolour headings / `bold` / fenced code / links as
/// user types
///
/// Uses `NSTextView` rather than SwiftUI's `TextEditor` because latter
/// has no API for per-range attributes. All standard editing
/// affordances (cmd+A, opt+<-/-> word jumps, selection via mouse,
/// undo, cut/copy/paste) come for free from `NSTextView`; we just
/// layer highlighting on top of its storage
struct MarkdownSyntaxTextView: NSViewRepresentable {
    @Binding var text: String
    /// Tracks @FocusState from parent so tapping shift+cmd+P back to
    /// editor mode restores first-responder without a manual click
    let isFocused: Bool
    let theme: Theme
    /// Hooks for same shortcuts SwiftUI's TextEditor handled: cmd+S
    /// save, cmd+return save-and-close, cmd+shift+M move, cmd+shift+P
    /// preview
    let onCommandS: () -> Void
    let onCommandReturn: () -> Void
    let onCommandShiftM: () -> Void
    let onCommandShiftP: () -> Void

    func makeCoordinator() -> Coordinator {
        Coordinator(parent: self)
    }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSTextView.scrollableTextView()
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = false
        scroll.drawsBackground = false
        scroll.borderType = .noBorder

        guard let textView = scroll.documentView as? NSTextView else { return scroll }
        textView.delegate = context.coordinator
        textView.isEditable = true
        textView.isRichText = false
        textView.isAutomaticQuoteSubstitutionEnabled = false
        textView.isAutomaticDashSubstitutionEnabled = false
        textView.isAutomaticSpellingCorrectionEnabled = false
        textView.isAutomaticTextReplacementEnabled = false
        textView.isAutomaticLinkDetectionEnabled = false
        textView.isAutomaticDataDetectionEnabled = false
        textView.allowsUndo = true
        textView.usesFindBar = false
        textView.drawsBackground = false
        textView.backgroundColor = .clear
        textView.textContainerInset = NSSize(width: 6, height: 6)
        textView.font = NSFont.monospacedSystemFont(ofSize: 14, weight: .regular)
        textView.textColor = NSColor(theme.primaryText)
        textView.insertionPointColor = NSColor(theme.accent)
        textView.string = text
        context.coordinator.highlight(textView)
        // Drop cursor at end of document so user can start appending
        // immediately - that's how text editors universally behave
        // when re-opening a file. NSTextView default is position 0,
        // leaving cursor at H1 title, a position nobody actually wants
        let end = (text as NSString).length
        textView.setSelectedRange(NSRange(location: end, length: 0))
        textView.scrollRangeToVisible(NSRange(location: end, length: 0))

        // Assert first-responder after view is attached to a window.
        // Without this, blinking caret never appears - SwiftUI's
        // @FocusState round-trip from parent is too indirect (no
        // .focused() modifier binds our NSTextView), so we bootstrap
        // focus ourselves on creation and rely on updateNSView for
        // subsequent restore-focus cases
        DispatchQueue.main.async {
            textView.window?.makeFirstResponder(textView)
        }

        // Intercept four editor shortcuts before text view consumes
        // them. `performKeyEquivalent` runs before keyDown and
        // returns true when we've handled event
        context.coordinator.attachKeyInterceptor(textView)
        return scroll
    }

    /// Called by SwiftUI when view is being removed from hierarchy
    /// (e.g. editor -> main transition via ESC). Explicitly release
    /// first responder so window doesn't hold a stale reference to
    /// our NSTextView - otherwise SwiftUI's @FocusState on main
    /// TextField can't claim focus and user has to click into query
    /// field to resume typing. Keyboard-only flow requires this
    /// handoff to be clean
    static func dismantleNSView(_ scroll: NSScrollView, coordinator: Coordinator) {
        guard let textView = scroll.documentView as? NSTextView,
              let window = textView.window,
              window.firstResponder === textView
        else { return }
        window.makeFirstResponder(nil)
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        guard let textView = scroll.documentView as? NSTextView else { return }
        if textView.string != text {
            // External write (e.g. rename-with-reload). Preserve
            // selection where possible so user doesn't lose cursor
            // position on auto-save round-trips
            let oldRange = textView.selectedRange()
            textView.string = text
            let clamped = NSRange(
                location: min(oldRange.location, textView.string.utf16.count),
                length: 0
            )
            textView.setSelectedRange(clamped)
            context.coordinator.highlight(textView)
        }
        textView.textColor = NSColor(theme.primaryText)
        if isFocused, textView.window?.firstResponder !== textView {
            // Async so view is attached before we ask to focus.
            // Guarded to avoid a per-render async hop once we already
            // are first responder
            DispatchQueue.main.async {
                textView.window?.makeFirstResponder(textView)
            }
        }
    }

    final class Coordinator: NSObject, NSTextViewDelegate {
        let parent: MarkdownSyntaxTextView
        private var eventMonitor: Any?

        init(parent: MarkdownSyntaxTextView) {
            self.parent = parent
        }

        deinit {
            if let m = eventMonitor { NSEvent.removeMonitor(m) }
        }

        func textDidChange(_ notification: Notification) {
            guard let textView = notification.object as? NSTextView else { return }
            parent.text = textView.string
            highlight(textView)
        }

        /// Catch `Enter` and `Tab` before NSTextView's defaults and
        /// add markdown-aware behavior:
        ///   - Enter continues list / blockquote prefixes on next
        ///     line; incremented for numbered lists; terminates list
        ///     when marker is empty.
        ///   - Tab, when cursor is on a list-item line, indents item
        ///     by two spaces so it becomes a sub-list.
        ///   - Shift-Tab outdents.
        /// Unrelated usages of Enter/Tab fall through unchanged so
        /// user's normal typing flow is untouched
        func textView(
            _ textView: NSTextView,
            doCommandBy commandSelector: Selector
        ) -> Bool {
            switch commandSelector {
            case #selector(NSResponder.insertNewline(_:)):
                return handleNewline(textView)
            case #selector(NSResponder.insertTab(_:)):
                return handleTab(textView, outdent: false)
            case #selector(NSResponder.insertBacktab(_:)):
                return handleTab(textView, outdent: true)
            default:
                return false
            }
        }

        /// Tab on a list-item line indents by two spaces (makes it a
        /// sub-list); Shift-Tab removes up to two leading spaces.
        /// On non-list lines we return false so Tab keeps its default
        /// meaning (focus change / literal tab insertion)
        private func handleTab(_ textView: NSTextView, outdent: Bool) -> Bool {
            let ns = textView.string as NSString
            let sel = textView.selectedRange()
            let lineRange = ns.lineRange(for: sel)
            let line = ns.substring(with: lineRange)
            let trimmed = line.drop { $0 == " " || $0 == "\t" }
            let isList = trimmed.hasPrefix("- ") || trimmed.hasPrefix("* ")
                || trimmed.hasPrefix("+ ") || trimmed.hasPrefix("> ")
                || firstIsNumberedMarker(trimmed)
            guard isList else { return false }
            if outdent {
                // Strip up to 2 leading spaces
                var strip = 0
                for ch in line {
                    if ch == " " && strip < 2 { strip += 1 } else { break }
                }
                guard strip > 0 else { return true } // absorb the keypress
                let stripRange = NSRange(location: lineRange.location, length: strip)
                if textView.shouldChangeText(in: stripRange, replacementString: "") {
                    textView.textStorage?.replaceCharacters(in: stripRange, with: "")
                    let newSel = NSRange(
                        location: max(sel.location - strip, lineRange.location),
                        length: 0
                    )
                    textView.setSelectedRange(newSel)
                    textView.didChangeText()
                }
                return true
            } else {
                let insertRange = NSRange(location: lineRange.location, length: 0)
                if textView.shouldChangeText(in: insertRange, replacementString: "  ") {
                    textView.textStorage?.replaceCharacters(in: insertRange, with: "  ")
                    textView.setSelectedRange(
                        NSRange(location: sel.location + 2, length: 0)
                    )
                    textView.didChangeText()
                }
                return true
            }
        }

        /// Fast check for "line starts with digits + dot + space"
        /// without allocating - mirrors MarkdownListContinuer's logic
        /// for Tab-indent path
        private func firstIsNumberedMarker(_ s: Substring) -> Bool {
            var i = s.startIndex
            while i < s.endIndex, s[i].isNumber { i = s.index(after: i) }
            guard i > s.startIndex, i < s.endIndex, s[i] == "." else { return false }
            let after = s.index(after: i)
            return after < s.endIndex && s[after] == " "
        }

        private func handleNewline(_ textView: NSTextView) -> Bool {
            let ns = textView.string as NSString
            let cursor = textView.selectedRange().location
            guard cursor <= ns.length else { return false }
            let lineRange = ns.lineRange(for: NSRange(location: cursor, length: 0))
            let beforeCursorLen = cursor - lineRange.location
            guard beforeCursorLen >= 0 else { return false }
            let beforeCursor = ns.substring(
                with: NSRange(location: lineRange.location, length: beforeCursorLen)
            )
            switch MarkdownListContinuer.continuation(beforeCursor: beforeCursor) {
            case .none:
                return false
            case .insert(let prefix):
                let insertion = "\n\(prefix)"
                let sel = textView.selectedRange()
                if textView.shouldChangeText(in: sel, replacementString: insertion) {
                    textView.textStorage?.replaceCharacters(in: sel, with: insertion)
                    let newCursor = sel.location + (insertion as NSString).length
                    textView.setSelectedRange(NSRange(location: newCursor, length: 0))
                    textView.didChangeText()
                }
                return true
            case .terminate:
                // Empty list marker at end-of-line -> rub out marker
                // and drop cursor onto a plain new line
                let markerRange = NSRange(
                    location: lineRange.location,
                    length: beforeCursorLen
                )
                if textView.shouldChangeText(in: markerRange, replacementString: "\n") {
                    textView.textStorage?.replaceCharacters(in: markerRange, with: "\n")
                    textView.setSelectedRange(
                        NSRange(location: markerRange.location + 1, length: 0)
                    )
                    textView.didChangeText()
                }
                return true
            }
        }

        /// Re-apply markdown highlighting to text view's storage.
        /// Wraps edit in `begin/endEditing` so layout manager sees one
        /// coalesced change. Keystroke-rate highlighting on a
        /// note-sized document is measured in microseconds - no need
        /// for debouncing
        func highlight(_ textView: NSTextView) {
            guard let storage = textView.textStorage else { return }
            let source = textView.string
            let baseFont = NSFont.monospacedSystemFont(ofSize: 14, weight: .regular)
            let baseColor = NSColor(parent.theme.primaryText)
            let full = NSRange(location: 0, length: (source as NSString).length)

            storage.beginEditing()
            storage.setAttributes(
                [.font: baseFont, .foregroundColor: baseColor],
                range: full
            )
            for span in SyntaxHighlighter.markdownSpans(source: source) {
                let lo = span.start
                let hi = min(span.end, full.length)
                guard lo < hi, lo >= 0 else { continue }
                let range = NSRange(location: lo, length: hi - lo)
                let color = NSColor(
                    SyntaxHighlighter.roleColor(span.role, theme: parent.theme)
                )
                storage.addAttribute(.foregroundColor, value: color, range: range)
                // Headings also get a bold weight for extra legibility
                if span.role == .key {
                    let bold = NSFont.monospacedSystemFont(
                        ofSize: 14, weight: .semibold
                    )
                    storage.addAttribute(.font, value: bold, range: range)
                }
            }
            storage.endEditing()
        }

        /// NSEvent.addLocalMonitorForEvents captures cmd-key combos
        /// before text view gets them. Only intercept ones we care
        /// about; every other cmd-key passes through so system
        /// Edit-menu shortcuts (cmd+A, cmd+C, cmd+V, cmd+X, cmd+Z)
        /// still reach NSTextView's native handlers
        func attachKeyInterceptor(_ textView: NSTextView) {
            if eventMonitor != nil { return }
            eventMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self, weak textView] event in
                guard let self = self,
                      let tv = textView,
                      event.window === tv.window else { return event }
                let cmd = event.modifierFlags.contains(.command)
                let shift = event.modifierFlags.contains(.shift)
                if cmd && !shift {
                    switch event.charactersIgnoringModifiers {
                    case "s":
                        self.parent.onCommandS()
                        return nil
                    default: break
                    }
                }
                if cmd && event.keyCode == 36 /* return */ {
                    self.parent.onCommandReturn()
                    return nil
                }
                if cmd && shift {
                    switch event.charactersIgnoringModifiers?.lowercased() {
                    case "m":
                        self.parent.onCommandShiftM()
                        return nil
                    case "p":
                        self.parent.onCommandShiftP()
                        return nil
                    default: break
                    }
                }
                return event
            }
        }
    }
}

