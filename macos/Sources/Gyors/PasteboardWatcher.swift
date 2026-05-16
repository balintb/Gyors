import AppKit
import CryptoKit
import Foundation

/// Polls NSPasteboard for changes and forwards copies to Rust
/// index. Two content shapes are supported:
///
/// - Strings - sent verbatim via `recordClipboard`. Rust
///   provider re-categorises them (URL / hex color / JSON /
///   number / ...) so row icons match.
/// - PNG/TIFF images - written to disk under
///   `~/Library/Application Support/Gyors/clipboard-images/<sha>.png`
///   and recorded as sentinel `gyors-image:<absolute-path>`.
///   Provider strips prefix to recover path, ships it as an
///   `Icon::ImagePath` so row renders a thumbnail, and on
///   activate reads bytes back to copy original image
///
/// `NSPasteboard.changeCount` is documented cheap way to detect a
/// change without reading contents. Poll interval (500 ms) is a
/// balance between responsiveness and CPU
final class PasteboardWatcher {
    /// Sentinel marker that wraps a saved-image path inside same
    /// `clipboard_items.content` text column Rust index uses for
    /// strings. Keeps schema migration unnecessary; provider
    /// recognises prefix and routes accordingly
    static let imageSentinelPrefix = "gyors-image:"

    private let bridge: QueryBridge
    private var timer: Timer?
    private var lastChangeCount: Int

    /// Fires after each successful record (string or image). Used
    /// by `AppDelegate` to refresh panel if user has clipboard
    /// list open - without this, row they just copied doesn't
    /// appear until they close + re-open panel
    var onChange: (() -> Void)?

    init(bridge: QueryBridge) {
        self.bridge = bridge
        self.lastChangeCount = NSPasteboard.general.changeCount
    }

    func start() {
        guard timer == nil else { return }
        let t = Timer(timeInterval: 0.5, repeats: true) { [weak self] _ in
            self?.check()
        }
        t.tolerance = 0.25
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    func stop() {
        timer?.invalidate()
        timer = nil
    }

    private func check() {
        let pb = NSPasteboard.general
        let cc = pb.changeCount
        guard cc != lastChangeCount else { return }
        lastChangeCount = cc

        // Password managers (1Password, Bitwarden,
        // LastPass, KeePassXC, Dashlane) stamp their copies with
        // a UTI that signals "do not retain". Honoring it keeps
        // passwords out of searchable clipboard history (and
        // out of cloud-sync outbox). Multiple vendors use
        // slightly different identifiers; allowlist anything
        // ending in `ConcealedType` and agilebits-specific
        // legacy id - so a future vendor that follows same
        // convention is covered without a code change
        if Self.isConcealed(pb) {
            // Dont even advance our hash chain - just bail.
            // We've already moved `lastChangeCount` so we won't
            // spin on this row
            return
        }

        // Prefer a string when both string and image types are
        // present (typical for screenshot tools that paste "Image
        // copied!" annotations alongside image). Plain text is
        // what user usually wants to paste; image branch covers
        // screenshot apps, Preview's Copy, etc., where pasteboard
        // holds image data only
        if let content = pb.string(forType: .string), !content.isEmpty {
            // Skip strings that look like our own image sentinel -
            // a safety belt in case someone pastes one back to
            // themselves
            if content.hasPrefix(Self.imageSentinelPrefix) { return }
            bridge.recordClipboard(content)
            onChange?()
            return
        }

        // Image branch. Iterate available types, take first PNG /
        // TIFF we can pull out, normalise to PNG bytes, write to
        // disk, record sentinel-wrapped path
        if let pngBytes = readImagePngBytes(from: pb) {
            if let path = saveImageToDisk(pngBytes) {
                bridge.recordClipboard(Self.imageSentinelPrefix + path)
                onChange?()
            }
        }
    }

    /// Pull image bytes from pasteboard and re-encode as PNG. We
    /// normalise everything to PNG so on-disk format is uniform
    /// (simplifies row's `NSImage(contentsOfFile:)` and makes
    /// later round-trips deterministic)
    private func readImagePngBytes(from pb: NSPasteboard) -> Data? {
        // Use NSImage(pasteboard:) so Cocoa converts whatever
        // format is actually present - TIFF, PDF, BMP,
        // CGImageSourceType-anything - into something we can
        // re-encode as PNG. Reading PNG type directly works only
        // when source app put PNG there, which screenshot tools
        // do but Preview's Copy doesn't
        guard let nsImg = NSImage(pasteboard: pb) else { return nil }
        guard let tiff = nsImg.tiffRepresentation,
              let rep = NSBitmapImageRep(data: tiff)
        else { return nil }
        return rep.representation(using: .png, properties: [:])
    }

    /// Persist `pngBytes` under
    /// `~/Library/Application Support/Gyors/clipboard-images/<sha>.png`.
    /// Content-addressed naming dedupes identical re-copies
    private func saveImageToDisk(_ pngBytes: Data) -> String? {
        let dir = Self.clipboardImagesDir()
        do {
            try FileManager.default.createDirectory(
                at: dir,
                withIntermediateDirectories: true
            )
        } catch {
            NSLog("clipboard-image: failed to create dir: %@", String(describing: error))
            return nil
        }
        let hash = SHA256.hash(data: pngBytes)
            .map { String(format: "%02x", $0) }
            .joined()
        let url = dir.appendingPathComponent("\(hash).png")
        if !FileManager.default.fileExists(atPath: url.path) {
            do {
                try pngBytes.write(to: url, options: .atomic)
            } catch {
                NSLog("clipboard-image: failed to write %@: %@",
                      url.path, String(describing: error))
                return nil
            }
        }
        return url.path
    }

    /// True when any of pasteboard's items declares a "do not
    /// retain" / "concealed" UTI. macOS does NOT enforce this -
    /// it's a community convention started by 1Password
    /// (`org.nspasteboard.ConcealedType`) and adopted by major
    /// password managers. We check both published UTI and legacy
    /// agilebits id so older 1Password versions still get
    /// filtered
    ///
    /// Internal so tests can hit it without a live pasteboard
    static func isConcealed(_ pb: NSPasteboard) -> Bool {
        let items = pb.pasteboardItems ?? []
        if items.isEmpty {
            // No item-level info to inspect; fall back to
            // top-level `types` list
            return (pb.types ?? []).contains { typeIsConcealed($0.rawValue) }
        }
        for item in items {
            for t in item.types where typeIsConcealed(t.rawValue) {
                return true
            }
        }
        return false
    }

    /// UTI matcher. Anything ending in `ConcealedType`, plus
    /// legacy agilebits id, counts as "do not retain". Public for
    /// unit test in `Tests/`
    static func typeIsConcealed(_ raw: String) -> Bool {
        if raw.hasSuffix("ConcealedType") { return true }
        if raw == "com.agilebits.onepassword.ConcealedType" { return true }
        return false
    }

    static func clipboardImagesDir() -> URL {
        let appSupport = FileManager.default.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first ?? URL(fileURLWithPath: NSHomeDirectory())
        return appSupport
            .appendingPathComponent("Gyors", isDirectory: true)
            .appendingPathComponent("clipboard-images", isDirectory: true)
    }

    deinit {
        stop()
    }
}
