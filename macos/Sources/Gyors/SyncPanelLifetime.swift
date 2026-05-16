// Collapsed to nothing in a no-cloud build - SyncPanel itself is
// gone, so lifetime helper has no callers
#if CLOUD
import AppKit
import Foundation

/// AppKit-only host for Cloud Sync window. SwiftUI content lives in
/// `SyncPanel.swift`; window lifetime stuff sits here so tests can
/// drive it without dragging SwiftUI in.
///
/// Exists because a local-scope NSWindow outlived its owning
/// function while a close animation was mid-commit, and the
/// autorelease pool freed a block pointing into already-dead
/// animation storage. Crash was EXC_BAD_ACCESS in objc_release of
/// _NSWindowTransformAnimation, 2026-05-12. We hold a strong ref
/// in `liveWindows` until the window's `windowWillClose` fires,
/// set animationBehavior to none so the bad pipeline never runs,
/// and pin isReleasedWhenClosed = false so a stray close() can't
/// over-release on top of our keeper.
@MainActor
enum SyncPanelHost {
    /// Strong refs for windows currently on-screen. Public to
    /// module for tests; cleared via `windowDidClose` after each
    /// window posts `windowWillClose:`
    static var liveWindows: [SyncPanelWindowKeeper] = []

    /// Build a new hardened NSWindow ready to receive an
    /// NSHostingView. Caller adopts returned window via `track(_:)`
    /// once they've installed content view
    static func makeWindow() -> NSWindow {
        let w = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 460, height: 460),
            styleMask: [.titled, .closable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        w.title = "Gyors Sync"
        w.titlebarAppearsTransparent = true
        w.isMovableByWindowBackground = true
        w.level = .modalPanel
        w.isReleasedWhenClosed = false
        w.animationBehavior = .none
        w.center()
        return w
    }

    /// Attach a keeper to `window` so it stays alive for duration
    /// of its visible lifetime. Returns keeper so a test can verify
    /// identity; production callers ignore it
    @discardableResult
    static func track(_ window: NSWindow) -> SyncPanelWindowKeeper {
        let keeper = SyncPanelWindowKeeper(window: window)
        liveWindows.append(keeper)
        return keeper
    }

    /// Remove a keeper from strong-ref list. Called by keeper
    /// itself on `windowWillClose:`, deferred one runloop tick so
    /// anything still holding window sees it valid for rest of
    /// current event
    static func windowDidClose(_ keeper: SyncPanelWindowKeeper) {
        liveWindows.removeAll { $0 === keeper }
    }
}

/// Owns strong reference for a single panel and unhooks it once
/// window posts `windowWillClose:`. Separate object instead of
/// dumping delegate methods on panel namespace because
/// `NSWindowDelegate` requires NSObject inheritance
@MainActor
final class SyncPanelWindowKeeper: NSObject, NSWindowDelegate {
    let window: NSWindow

    init(window: NSWindow) {
        self.window = window
        super.init()
        window.delegate = self
    }

    nonisolated func windowWillClose(_ notification: Notification) {
        // Detach delegate before AppKit tears down, then drop
        // strong ref on main thread next runloop tick so any
        // pending main-thread closures hold onto a still-valid
        // window for one more pass
        MainActor.assumeIsolated { [weak self] in
            guard let self = self else { return }
            self.window.delegate = nil
            DispatchQueue.main.async {
                SyncPanelHost.windowDidClose(self)
            }
        }
    }
}

#endif // CLOUD
