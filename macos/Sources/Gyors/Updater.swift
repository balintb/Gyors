// Optional Sparkle integration
//
// Sparkle ships as an Objective-C framework that we'd rather not
// hard-depend on for two reasons:
//
//   1. Default dev build is unsigned. Sparkle refuses to apply
//      unsigned updates (by design), so even with framework linked
//      it's effectively dead weight until we have a signing
//      identity.
//   2. Vendoring framework into a public repo adds ~6 MB of binary
//      blobs that dont belong in source. Release builder fetches
//      it at build time instead.
//
// So this file is the seam: at compile time, `WITH_SPARKLE=1` flips
// on `SPARKLE` Swift conditional and real Sparkle wrapper gets
// compiled. Without the flag, stub takes over - `isAvailable` is
// `false` and `checkForUpdates()` is a no-op - and menu item that
// exposes it stays hidden. Default `build-app.sh` invocation keeps
// Sparkle off

import AppKit
import Foundation

#if SPARKLE
import Sparkle

/// Thin wrapper around `SPUStandardUpdaterController` so rest of
/// launcher doesn't import Sparkle directly. Keeps menu-bar +
/// AppDelegate code paths identical regardless of whether Sparkle
/// is compiled in
@MainActor
final class Updater {
    static let isAvailable = true

    private let controller: SPUStandardUpdaterController

    init() {
        // `startingUpdater: true` schedules periodic check on a
        // background queue immediately. Sparkle reads `SUFeedURL` /
        // `SUEnableAutomaticChecks` from `Info.plist` so all
        // policy stays declarative
        self.controller = SPUStandardUpdaterController(
            startingUpdater: true,
            updaterDelegate: nil,
            userDriverDelegate: nil
        )
    }

    /// Wired to menu bar "Check for Updates..." item. Sparkle pops
    /// its own modal UI; we just kick the call
    func checkForUpdates() {
        controller.updater.checkForUpdates()
    }
}

#else

/// Stub. Lives in same module so call sites are identical -
/// `Updater.isAvailable` guards menu wire-up, and real type only
/// appears when `SPARKLE` is on. Class still exists in no-Sparkle
/// build so reference sites compile; methods are no-ops +
/// `isAvailable` is `false`
@MainActor
final class Updater {
    static let isAvailable = false

    init() {}

    func checkForUpdates() {
        // Should never be reached: `MenuBar` only surfaces menu
        // item when `isAvailable` is true. If somebody adds a new
        // call site that ignores the guard, log it loudly rather
        // than failing silently
        NSLog("gyors: Updater.checkForUpdates() called in non-Sparkle build")
    }
}

#endif
