// No-cloud build: SyncPanelHost doesn't exist in non-CLOUD
// shell. Whole file collapses to whitespace when WITH_CLOUD=0
#if CLOUD
import AppKit
import Foundation

/// Regression tests for the 2026-05-12 SyncPanel crash -
/// `EXC_BAD_ACCESS` in `objc_release` of `_NSWindowTransformAnimation`
/// when the user closed Cloud Sync window. Locks in
/// belt-and-braces fix in `SyncPanelLifetime.swift`:
///
/// - hardened window settings
/// - strong-ref retention via `SyncPanelHost.liveWindows`
/// - delegate-driven cleanup deferred one runloop tick
///
/// All tests run on main thread because host is MainActor-
/// isolated and close notification has to round-trip through the
/// main runloop to fire deferred async cleanup
@MainActor
func runSyncPanelLifetimeTests() {

    // Reset state between groups so a leak in one test doesn't
    // poison next. (Append-only `liveWindows` would otherwise
    // grow across whole suite.)
    func resetHost() {
        // Force-close everything tracked, then drain pending
        // main-queue work so deferred removals settle
        let snapshot = SyncPanelHost.liveWindows
        for keeper in snapshot {
            keeper.window.close()
        }
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.05))
        SyncPanelHost.liveWindows.removeAll()
    }

    runGroup("makeWindow returns hardened settings - animation off, not auto-released, modal panel") {
        resetHost()
        let w = SyncPanelHost.makeWindow()
        expect(w.animationBehavior == .none,
            "animationBehavior must be .none - that's the load-bearing crash fix, got \(w.animationBehavior.rawValue)")
        expect(w.isReleasedWhenClosed == false,
            "isReleasedWhenClosed must be false so AppKit doesn't release the window from under us")
        expect(w.level == .modalPanel,
            "level should sit above the launcher panel, got \(w.level.rawValue)")
        expect(w.styleMask.contains(.titled) && w.styleMask.contains(.closable),
            "window must be titled and closable")
        expect(w.styleMask.contains(.fullSizeContentView),
            "fullSizeContentView lets the SwiftUI form draw under the titlebar")
        w.close()
    }

    runGroup("track installs a delegate and adds the window to liveWindows") {
        resetHost()
        let w = SyncPanelHost.makeWindow()
        let beforeCount = SyncPanelHost.liveWindows.count
        let keeper = SyncPanelHost.track(w)
        expect(SyncPanelHost.liveWindows.count == beforeCount + 1,
            "track must append to liveWindows, count went \(beforeCount) → \(SyncPanelHost.liveWindows.count)")
        expect(SyncPanelHost.liveWindows.contains(where: { $0 === keeper }),
            "the returned keeper must be the same instance stored in liveWindows")
        expect(w.delegate === keeper,
            "track must install the keeper as the window's delegate")
        w.close()
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.05))
    }

    runGroup("windowWillClose drops the strong ref and detaches the delegate") {
        resetHost()
        let w = SyncPanelHost.makeWindow()
        let keeper = SyncPanelHost.track(w)
        weak var weakKeeper: SyncPanelWindowKeeper? = keeper
        _ = keeper // silence unused warning; we want the strong-ref test to fire on liveWindows-only

        expect(SyncPanelHost.liveWindows.count == 1, "precondition: exactly one tracked window")
        w.close()
        // Cleanup is deferred one runloop tick - pump loop
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))
        expect(SyncPanelHost.liveWindows.isEmpty,
            "liveWindows must be empty after close, has \(SyncPanelHost.liveWindows.count)")
        expect(w.delegate == nil,
            "delegate must be detached on willClose - keeping it attached re-enters during teardown")
        // Drop our local strong refs and confirm keeper actually
        // gets deallocated (no retain cycle in NSWindow <-> keeper)
        _ = weakKeeper // suppress capture-only warning; touched below
    }

    runGroup("multiple windows tracked independently - closing one doesn't disturb the others") {
        resetHost()
        let a = SyncPanelHost.makeWindow()
        let b = SyncPanelHost.makeWindow()
        let c = SyncPanelHost.makeWindow()
        let kA = SyncPanelHost.track(a)
        _ = SyncPanelHost.track(b)
        let kC = SyncPanelHost.track(c)
        expect(SyncPanelHost.liveWindows.count == 3,
            "three windows tracked, got \(SyncPanelHost.liveWindows.count)")

        b.close()
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.05))
        expect(SyncPanelHost.liveWindows.count == 2,
            "closing middle window should leave 2, got \(SyncPanelHost.liveWindows.count)")
        expect(SyncPanelHost.liveWindows.contains(where: { $0 === kA }),
            "first window's keeper must survive")
        expect(SyncPanelHost.liveWindows.contains(where: { $0 === kC }),
            "third window's keeper must survive")

        a.close(); c.close()
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.05))
        expect(SyncPanelHost.liveWindows.isEmpty,
            "all three closed, got \(SyncPanelHost.liveWindows.count) still tracked")
    }

    runGroup("rapid open+close cycles don't leak windows or crash") {
        // Mimic a user spamming menu item. Pre-fix this would
        // either crash on n-th close or grow liveWindows
        // unbounded
        resetHost()
        for _ in 0..<20 {
            let w = SyncPanelHost.makeWindow()
            SyncPanelHost.track(w)
            w.close()
            // Pump once per cycle so deferred removal runs before
            // next window goes in. Without this, we'd queue 20
            // removals at once and still pass - but we want to verify
            // steady-state, one-at-a-time pattern works too
            RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.005))
        }
        // Final drain
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.1))
        expect(SyncPanelHost.liveWindows.isEmpty,
            "20 cycles should drain to zero, still tracking \(SyncPanelHost.liveWindows.count)")
    }

    runGroup("windowWillClose is safe to call when window isn't tracked - no crash, no removals") {
        // a future caller might post notification on
        // a window we didn't `track`. Should be a no-op rather than
        // throwing or removing wrong keeper
        resetHost()
        let tracked = SyncPanelHost.makeWindow()
        SyncPanelHost.track(tracked)
        expect(SyncPanelHost.liveWindows.count == 1, "precondition")

        let untracked = SyncPanelHost.makeWindow()
        let fakeNotification = Notification(name: NSWindow.willCloseNotification, object: untracked)
        // We can't easily synthesise delegate callback for an
        // untracked window, so instead verify that `windowDidClose`
        // with an unrelated keeper instance does nothing
        let strayKeeper = SyncPanelWindowKeeper(window: untracked)
        SyncPanelHost.windowDidClose(strayKeeper)
        expect(SyncPanelHost.liveWindows.count == 1,
            "removing an unknown keeper must not affect tracked windows, got \(SyncPanelHost.liveWindows.count)")
        _ = fakeNotification

        tracked.close(); untracked.close()
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.05))
    }

    runGroup("keeper deallocates after window closes - no retain cycle") {
        // Without an autoreleasepool around body, keeper
        // (an NSObject subclass) can sit autoreleased until the
        // runloop's pool drains; tests without NSApp dont always
        // give that a chance to happen before the assertion. Wrap
        // explicitly so the check is deterministic
        resetHost()
        weak var weakKeeper: SyncPanelWindowKeeper?
        autoreleasepool {
            let w = SyncPanelHost.makeWindow()
            let k = SyncPanelHost.track(w)
            weakKeeper = k
            w.close()
            // Pump the runloop long enough for the deferred
            // `windowDidClose` to run and release its capture
            RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.15))
            // Drop `k` and `w` by letting autoreleasepool exit
            _ = k
            _ = w
        }
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.05))
        expect(weakKeeper == nil,
            "keeper must dealloc after its window closes - otherwise we leak per panel open")
    }
}

#endif // CLOUD
