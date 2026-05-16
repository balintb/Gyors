import Foundation

/// Home: single owner of "where Gyors data root lives" +
/// chmod-to-0700 hardening on top
///
/// Files inside are already 0600 (Config.swift writes via `.atomic`,
/// ThemeImporter writes individual JSON blobs, clipboard images are
/// user-owned). Directory itself was created with user umask
/// (commonly 0755), which lets a different user on a shared Mac LIST
/// plugin ids, theme names, and clipboard image SHAs even though
/// they can't read contents. Fix is a chmod on root - everything
/// else lives under that gate
enum GyorsPaths {
    /// `~/Library/Application Support/Gyors/` (or whatever
    /// `applicationSupportDirectory` resolves to on this machine).
    /// Pure - never touches disk
    static func dataDir() -> URL {
        if let support = FileManager.default.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first {
            return support.appendingPathComponent("Gyors", isDirectory: true)
        }
        return URL(fileURLWithPath: NSHomeDirectory())
            .appendingPathComponent("Library/Application Support/Gyors", isDirectory: true)
    }

    /// Ensure Gyors data root exists with POSIX perms 0700.
    /// Idempotent + side-effect-only (returns dir for chaining).
    /// Call from any code path that's about to create a subdirectory
    /// or write a file under root
    ///
    /// Returns nil only if `setAttributes` fails after a successful
    /// `createDirectory`, which on a healthy macOS install means
    /// dir was deleted between the two calls - rare enough that a
    /// nil return signals "give up + log" to caller
    @discardableResult
    static func ensureDataDirSecured() -> URL? {
        let dir = dataDir()
        do {
            try FileManager.default.createDirectory(
                at: dir,
                withIntermediateDirectories: true
            )
            try FileManager.default.setAttributes(
                [.posixPermissions: 0o700],
                ofItemAtPath: dir.path
            )
            return dir
        } catch {
            NSLog("gyors: ensureDataDirSecured failed: \(error)")
            return nil
        }
    }
}
