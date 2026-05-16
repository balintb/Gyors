import AppKit
import Foundation

/// Regression tests. Password managers paste with a
/// `ConcealedType` UTI to signal "do not retain in clipboard
/// history"; `PasteboardWatcher` honors this. If anyone removes
/// the filter, these break loudly
func runPasteboardConcealedTests() {

    runGroup("PasteboardWatcher.typeIsConcealed recognises every known concealed UTI") {
        // 1Password's original + adopted-by-others UTI
        expect(PasteboardWatcher.typeIsConcealed("org.nspasteboard.ConcealedType"),
            "primary org.nspasteboard.ConcealedType must filter")
        // Legacy agilebits id (older 1Password versions)
        expect(PasteboardWatcher.typeIsConcealed("com.agilebits.onepassword.ConcealedType"),
            "legacy agilebits id must filter")
        // Forward-compatibility: any vendor that follows the
        // `*.ConcealedType` convention is covered without a code change
        expect(PasteboardWatcher.typeIsConcealed("com.bitwarden.ConcealedType"),
            "vendor-suffix convention must filter")
        expect(PasteboardWatcher.typeIsConcealed("com.lastpass.SuperSecret.ConcealedType"),
            "nested-suffix convention must filter")
    }

    runGroup("PasteboardWatcher.typeIsConcealed does NOT filter normal text types") {
        // Plain text - the common case. Filtering this would
        // turn the whole feature off
        expect(!PasteboardWatcher.typeIsConcealed("public.utf8-plain-text"),
            "public.utf8-plain-text must NOT be classed as concealed")
        expect(!PasteboardWatcher.typeIsConcealed("public.plain-text"),
            "public.plain-text must NOT be classed as concealed")
        expect(!PasteboardWatcher.typeIsConcealed("NSStringPboardType"),
            "legacy NSStringPboardType must NOT be classed as concealed")
        expect(!PasteboardWatcher.typeIsConcealed(""),
            "empty type string must NOT be classed as concealed")
        // Things that share substrings but aren't the marker
        expect(!PasteboardWatcher.typeIsConcealed("org.nspasteboard.Concealed"),
            "missing -Type suffix must NOT match")
        expect(!PasteboardWatcher.typeIsConcealed("public.image"),
            "image UTI must NOT match")
    }

    runGroup("PasteboardWatcher.isConcealed flags a real NSPasteboard with the marker") {
        // We can't write to NSPasteboard.general in a test
        // without trampling the user's clipboard. Use a private
        // pasteboard instead
        let pb = NSPasteboard(name: NSPasteboard.Name("gyors-test-concealed-\(UUID().uuidString)"))
        pb.clearContents()
        let item = NSPasteboardItem()
        item.setString("hunter2-not-the-real-password", forType: .string)
        item.setString("", forType: NSPasteboard.PasteboardType("org.nspasteboard.ConcealedType"))
        pb.writeObjects([item])
        expect(PasteboardWatcher.isConcealed(pb),
            "pasteboard with org.nspasteboard.ConcealedType must be flagged")
        pb.releaseGlobally()
    }

    runGroup("PasteboardWatcher.isConcealed leaves normal pasteboards alone") {
        let pb = NSPasteboard(name: NSPasteboard.Name("gyors-test-normal-\(UUID().uuidString)"))
        pb.clearContents()
        let item = NSPasteboardItem()
        item.setString("hello world", forType: .string)
        pb.writeObjects([item])
        expect(!PasteboardWatcher.isConcealed(pb),
            "plain-text pasteboard must NOT be flagged as concealed")
        pb.releaseGlobally()
    }
}
