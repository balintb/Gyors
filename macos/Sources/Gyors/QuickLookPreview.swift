import AppKit
import QuickLookUI

/// MacOS Quick Look integration. cmdY on a file-bearing result row
/// pops shared `QLPreviewPanel` over Gyors - same behaviour as
/// Space in Finder. Avoids "open the file just to see what's in it"
/// round-trip when user is triaging search results
///
/// Design:
/// - Singleton + datasource + delegate. `QLPreviewPanel` is a
///   shared macOS-level resource; we hold fixture rather than
///   creating one per preview.
/// - Accepts a list of URLs (usually one), but shape is kept plural
///   because QL's own navigation uses it - a future "preview the
///   whole result list" gesture would just pass all URLs in order.
/// - Responder-chain compliance: `AppDelegate` adopts preview-
///   panel-control protocol. Without it, QL silently dismisses the
///   moment it opens
final class QuickLookPresenter: NSObject, QLPreviewPanelDataSource, QLPreviewPanelDelegate {
    static let shared = QuickLookPresenter()
    private var urls: [URL] = []

    /// Present QL panel for a single URL. Brings it to front, binds
    /// this presenter as datasource. Returns false when URL can't
    /// be previewed (e.g., doesn't exist on disk) so caller can
    /// decide to fall back to inline previews
    @discardableResult
    func present(_ url: URL) -> Bool {
        guard FileManager.default.fileExists(atPath: url.path) else { return false }
        urls = [url]
        guard let panel = QLPreviewPanel.shared() else { return false }
        panel.dataSource = self
        panel.delegate = self
        panel.reloadData()
        panel.makeKeyAndOrderFront(nil)
        return true
    }

    // MARK: QLPreviewPanelDataSource

    func numberOfPreviewItems(in panel: QLPreviewPanel!) -> Int { urls.count }

    func previewPanel(_ panel: QLPreviewPanel!, previewItemAt index: Int) -> QLPreviewItem! {
        urls[index] as NSURL
    }
}

/// Return on-disk URL a candidate represents, if any. Used by cmdY
/// handler to decide whether Quick Look is applicable to selected
/// row
///
/// Candidate id conventions observed today:
/// - `note::<absolute .md path>` - Markdown note.
/// - `file::<absolute path>` (reserved) - generic file result.
/// - Anything else -> nil (apps, snippets, clipboard-text, system
///   commands, AI results - QL doesn't help there).
func quickLookURL(for candidate: Candidate) -> URL? {
    let id = candidate.id
    if let rest = id.strip(prefix: "note::") {
        if rest.hasSuffix(".md") {
            return URL(fileURLWithPath: rest)
        }
    }
    if let rest = id.strip(prefix: "file::") {
        return URL(fileURLWithPath: rest)
    }
    return nil
}

private extension String {
    func strip(prefix: String) -> String? {
        guard hasPrefix(prefix) else { return nil }
        return String(dropFirst(prefix.count))
    }
}
