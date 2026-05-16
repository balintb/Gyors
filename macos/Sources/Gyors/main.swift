import AppKit

// Top-level NSApplication bootstrap. Wrapped in `MainActor
// .assumeIsolated` so we can construct `@MainActor`-isolated
// AppDelegate from this (otherwise nonisolated) script context -
// program literally can't run anywhere else, and Swift 6's strict
// concurrency model wants us to acknowledge that explicitly
MainActor.assumeIsolated {
    let delegate = AppDelegate()
    NSApplication.shared.delegate = delegate
    NSApplication.shared.run()
}
