import AppKit
import SwiftUI

/// AppKit-backed text field for main input bar.
///
/// SwiftUI's `TextField` doesn't work here: its internal buffer
/// lags behind `@Published` changes through a computed `Binding`,
/// so a pill commit that zeroes `activeBuffer` doesn't propagate
/// before the next keystroke arrives, and that keystroke gets
/// appended to the stale string. The Coordinator on this wrapper
/// force-syncs `stringValue` to the bound value synchronously on
/// every change, so the field reflects a commit before the next
/// key event reaches it. Raycast and VS Code went the same route
/// for the same reason.
struct MainInputField: NSViewRepresentable {
    @Binding var text: String
    var placeholder: String
    var fontSize: CGFloat
    var textColor: NSColor
    /// Bumped by VM to request focus (panel open, editor exit).
    /// Tracked via a coordinator-local counter so we only call
    /// `makeFirstResponder` on actual transitions, not every update
    var focusTick: Int
    /// Enter / Return pressed in field
    var onSubmit: () -> Void

    func makeNSView(context: Context) -> NSTextField {
        let tf = NSTextField()
        tf.isBordered = false
        tf.isBezeled = false
        tf.drawsBackground = false
        tf.focusRingType = .none
        tf.cell?.wraps = false
        tf.cell?.isScrollable = true
        tf.lineBreakMode = .byClipping
        tf.usesSingleLineMode = true
        tf.delegate = context.coordinator
        tf.target = context.coordinator
        tf.action = #selector(Coordinator.submit(_:))
        tf.stringValue = text
        tf.placeholderString = placeholder
        tf.font = NSFont.systemFont(ofSize: fontSize, weight: .light)
        tf.textColor = textColor
        // Disable system autocorrect / text-replacement - this is a
        // launcher, not a prose editor. Matches original SwiftUI
        // TextField's `.autocorrectionDisabled(true)` modifier
        tf.cell?.isContinuous = true
        tf.allowsEditingTextAttributes = false
        // Grab focus immediately on mount - panel opens, user
        // types, nothing in between
        DispatchQueue.main.async { [weak tf] in
            tf?.window?.makeFirstResponder(tf)
        }
        return tf
    }

    func updateNSView(_ tf: NSTextField, context: Context) {
        context.coordinator.parent = self
        tf.placeholderString = placeholder
        tf.font = NSFont.systemFont(ofSize: fontSize, weight: .light)
        tf.textColor = textColor
        // Force sync: if bound `text` differs from what field
        // currently shows, overwrite. This is whole point of
        // representable - it kills SwiftUI-TextField-lag bug that
        // let stale pre-commit text concatenate with fresh
        // keystrokes
        if tf.stringValue != text {
            tf.stringValue = text
        }
        // Refocus on `focusTick` transitions (panel reopen, editor
        // exit). Runs async so NSTextField is mounted into window
        // hierarchy before `makeFirstResponder` fires
        if focusTick != context.coordinator.lastFocusTick {
            context.coordinator.lastFocusTick = focusTick
            DispatchQueue.main.async { [weak tf] in
                tf?.window?.makeFirstResponder(tf)
            }
        }
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(self)
    }

    final class Coordinator: NSObject, NSTextFieldDelegate {
        var parent: MainInputField
        var lastFocusTick: Int = Int.min
        /// Re-entrancy guard: setting `stringValue` from inside
        /// change handler can re-fire `controlTextDidChange` on
        /// some macOS versions. We only need one pass per user
        /// keystroke
        private var syncing = false

        init(_ parent: MainInputField) { self.parent = parent }

        func controlTextDidChange(_ notification: Notification) {
            guard !syncing, let tf = notification.object as? NSTextField else { return }
            let raw = tf.stringValue
            // Push to bound binding. This may trigger VM to commit
            // a pill and reset `text` to "" (or a trimmed form);
            // force-sync below reflects that back into field
            // before user's next keystroke can see it
            parent.text = raw
            if tf.stringValue != parent.text {
                syncing = true
                tf.stringValue = parent.text
                syncing = false
            }
        }

        @objc func submit(_ sender: Any) {
            parent.onSubmit()
        }
    }
}
