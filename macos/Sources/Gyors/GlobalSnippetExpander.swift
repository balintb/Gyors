import AppKit
import Carbon
import Foundation

/// System-wide snippet expansion via a CGEventTap. Watches keydown
/// for `;<trigger>;`, then backspaces it out, stuffs the snippet
/// text onto the pasteboard, fires a synthetic cmd+V, and restores
/// the previous clipboard contents.
///
/// macOS has no public "type this text" API, so CGEventTap is what
/// Alfred, Raycast and TextExpander all do too. Requires
/// Accessibility permission; we gate startup on it.
///
/// Paste rather than synthesised keystrokes:
/// `CGEvent.keyboardSetUnicodeString` exists but interacts badly
/// with IME layers (dead keys, candidate windows). Paste is boring
/// and works everywhere.
///
/// Opt-in via `snippets.expand_globally = true` - first enable
/// triggers the Accessibility prompt. Trigger buffer is capped
/// (`bufferMax`) so a run-on line can't balloon memory. Tap
/// callbacks have to return fast, so the handler only does match +
/// buffer mutation; the actual paste hops to main.
final class GlobalSnippetExpander {
    static let shared = GlobalSnippetExpander()

    private var tap: CFMachPort?
    private var runLoopSource: CFRunLoopSource?
    /// (trigger -> text) built from `gyors_snippets_json`. Rebuilt
    /// on `reloadSnippets`, which callers fire on TOML changes
    private var snippets: [String: String] = [:]
    /// Longest trigger in table. Caps how far back we scan ring
    /// buffer - no point looking past longest possible match
    private var longestTrigger: Int = 0
    /// Rolling key buffer. We keep at most `bufferMax` chars -
    /// enough for any reasonable trigger with margin, and capped
    /// so a dialect of hand-typed novel doesn't balloon memory
    private var buffer: String = ""
    /// Cap on rolling keystroke buffer
    private let bufferMax: Int = 128

    private init() {}

    /// Start tap if user has opted in AND Accessibility permission
    /// has been granted. Idempotent - calling while already
    /// running is a no-op
    func start() {
        guard gyors_snippets_global_enabled() else { return }
        guard tap == nil else { return }
        guard isProcessTrusted(promptIfNeeded: true) else {
            NSLog("gyors: snippet expander cannot start - Accessibility permission denied")
            return
        }
        reloadSnippets()
        installTap()
    }

    /// Stop tap and release its run-loop source. Idempotent
    func stop() {
        if let source = runLoopSource {
            CFRunLoopRemoveSource(CFRunLoopGetMain(), source, .commonModes)
        }
        if let tap = tap {
            CGEvent.tapEnable(tap: tap, enable: false)
        }
        runLoopSource = nil
        tap = nil
        buffer.removeAll(keepingCapacity: false)
    }

    /// Pull a fresh (trigger -> text) table from Rust. Called at
    /// start and any time snippets file changes. Keeps lookup
    /// table in lockstep with TOML without making hot path
    /// re-read from disk
    func reloadSnippets() {
        var table: [String: String] = [:]
        var longest = 0
        if let raw = gyors_snippets_json() {
            defer { gyors_free_string(raw) }
            let json = String(cString: raw)
            if let data = json.data(using: .utf8),
               let arr = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]] {
                for entry in arr {
                    guard
                        let trigger = entry["trigger"] as? String,
                        let text = entry["text"] as? String,
                        !trigger.isEmpty
                    else { continue }
                    table[trigger] = text
                    longest = max(longest, trigger.count)
                }
            }
        }
        snippets = table
        longestTrigger = longest
    }


    private func installTap() {
        let mask = (1 << CGEventType.keyDown.rawValue)
        let opaque = Unmanaged.passUnretained(self).toOpaque()
        let tap = CGEvent.tapCreate(
            tap: .cgSessionEventTap,
            place: .headInsertEventTap,
            options: .defaultTap,
            eventsOfInterest: CGEventMask(mask),
            callback: { _, type, event, userInfo in
                guard let userInfo = userInfo else { return Unmanaged.passUnretained(event) }
                let me = Unmanaged<GlobalSnippetExpander>.fromOpaque(userInfo).takeUnretainedValue()
                return me.handle(type: type, event: event)
            },
            userInfo: opaque
        )
        guard let tap = tap else {
            NSLog("gyors: failed to create CGEventTap - check Input Monitoring permission")
            return
        }
        let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0)
        CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
        CGEvent.tapEnable(tap: tap, enable: true)
        self.tap = tap
        self.runLoopSource = source
    }

    /// Per-event handler. Must return fast - no I/O, no blocking.
    /// Actual expansion is dispatched to main queue so tap stays
    /// responsive
    private func handle(type: CGEventType, event: CGEvent) -> Unmanaged<CGEvent>? {
        // MacOS can disable a tap that takes too long; re-enable
        // it and skip this event
        if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
            if let tap = tap { CGEvent.tapEnable(tap: tap, enable: true) }
            return Unmanaged.passUnretained(event)
        }
        guard type == .keyDown else { return Unmanaged.passUnretained(event) }

        let chars = unicodeString(from: event)
        if chars.isEmpty { return Unmanaged.passUnretained(event) }

        GlobalSnippetMatcher.appendToBuffer(chars, into: &buffer, max: bufferMax)

        if let hit = GlobalSnippetMatcher.findTriggerMatch(
            buffer: buffer,
            longestTrigger: longestTrigger,
            snippets: snippets
        ) {
            // Clear ring so a back-to-back expansion doesn't treat
            // stale chars as part of next trigger
            buffer.removeAll(keepingCapacity: true)
            // `[weak self]` even though this is a singleton -
            // defensive against a future world where expander gets
            // stopped/restarted, leaving in-flight dispatched
            // blocks holding a strong reference and firing a
            // `performExpansion` after teardown
            DispatchQueue.main.async { [weak self] in
                self?.performExpansion(deleteCount: hit.deleteCount, insert: hit.text)
            }
            return nil // swallow terminating semicolon
        }
        return Unmanaged.passUnretained(event)
    }

    /// Extract printable characters produced by this key event.
    /// Using unicode string (instead of translating keycodes) makes
    /// us layout-agnostic - Dvorak / AZERTY / any IME all just work
    private func unicodeString(from event: CGEvent) -> String {
        var length: Int = 0
        var buf = [UniChar](repeating: 0, count: 4)
        event.keyboardGetUnicodeString(
            maxStringLength: buf.count,
            actualStringLength: &length,
            unicodeString: &buf
        )
        guard length > 0 else { return "" }
        return String(utf16CodeUnits: buf, count: length)
    }


    /// Replace last `deleteCount` characters with `insert` by
    /// synthesizing backspaces + a cmdV after stashing `insert` on
    /// pasteboard. Restores previous pasteboard contents after a
    /// short delay so user's clipboard isn't clobbered
    private func performExpansion(deleteCount: Int, insert: String) {
        // Send deletes first so trigger characters vanish before
        // replacement lands
        let src = CGEventSource(stateID: .combinedSessionState)
        for _ in 0..<deleteCount {
            if let down = CGEvent(keyboardEventSource: src, virtualKey: 0x33, keyDown: true) {
                down.post(tap: .cghidEventTap)
            }
            if let up = CGEvent(keyboardEventSource: src, virtualKey: 0x33, keyDown: false) {
                up.post(tap: .cghidEventTap)
            }
        }
        // Stash clipboard + replace, then cmdV, then restore
        let pb = NSPasteboard.general
        let previous = pb.pasteboardItems?.compactMap { item in
            item.types.reduce(into: [NSPasteboard.PasteboardType: Data]()) { acc, t in
                if let d = item.data(forType: t) { acc[t] = d }
            }
        } ?? []
        pb.clearContents()
        pb.setString(insert, forType: .string)
        pressCommandV(src: src)
        // Restore previous contents after a short delay - long
        // enough that cmdV we just fired has completed its paste cycle
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.15) {
            pb.clearContents()
            for bag in previous {
                let item = NSPasteboardItem()
                for (type, data) in bag {
                    item.setData(data, forType: type)
                }
                pb.writeObjects([item])
            }
        }
    }

    private func pressCommandV(src: CGEventSource?) {
        guard let src = src else { return }
        guard
            let down = CGEvent(keyboardEventSource: src, virtualKey: 0x09, keyDown: true),
            let up = CGEvent(keyboardEventSource: src, virtualKey: 0x09, keyDown: false)
        else { return }
        down.flags = .maskCommand
        up.flags = .maskCommand
        down.post(tap: .cghidEventTap)
        up.post(tap: .cghidEventTap)
    }

    private func isProcessTrusted(promptIfNeeded: Bool) -> Bool {
        let key = "AXTrustedCheckOptionPrompt" as CFString
        let options: CFDictionary =
            [key: promptIfNeeded] as CFDictionary
        return AXIsProcessTrustedWithOptions(options)
    }
}
