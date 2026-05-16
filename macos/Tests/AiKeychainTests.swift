import AppKit
import Foundation

/// Regression tests for the AI api-key migration
///
/// These touch the real macOS Keychain because the security-
/// framework wrapper takes a service + account pair. To avoid
/// trampling a real user's stored key during local test runs, we
/// (a) write a sentinel value, (b) round-trip it, (c) delete it
/// before test exits. If a developer happens to have a real
/// `com.gyors.ai/api_key` entry, it gets temporarily clobbered -
/// but only on test process's main thread, and the cleanup
/// `delete()` runs even on failure via deferred blocks
func runAiKeychainTests() {

    runGroup("AiKeychain write/read/delete round-trip") {
        let original = AiKeychain.read()
        defer {
            // Restore whatever was there before (if anything), or
            // just delete test value
            if let original = original {
                _ = AiKeychain.write(original)
            } else {
                _ = AiKeychain.delete()
            }
        }
        let sentinel = "sec-a02-test-\(UUID().uuidString)"
        expect(AiKeychain.write(sentinel), "write must succeed")
        expect(AiKeychain.read() == sentinel,
            "read must return the value we just wrote, got \(String(describing: AiKeychain.read()))")
        // Idempotent write: same key again still succeeds
        expect(AiKeychain.write(sentinel), "second write to same slot must succeed")
        // Different value updates the slot
        let second = "sec-a02-replacement-\(UUID().uuidString)"
        expect(AiKeychain.write(second), "update to different value must succeed")
        expect(AiKeychain.read() == second,
            "read after update must return the new value")
        // Delete clears
        expect(AiKeychain.delete(), "delete must succeed")
        expect(AiKeychain.read() == nil,
            "post-delete read must be nil, got \(String(describing: AiKeychain.read()))")
        // Idempotent delete: missing entry is also success
        expect(AiKeychain.delete(), "second delete (now missing) must still succeed")
    }

    runGroup("Config.load migrates ai.api_key out of config.json into Keychain") {
        // Stash any existing key so we dont lose it
        let original = AiKeychain.read()
        defer {
            _ = AiKeychain.delete()
            if let original = original { _ = AiKeychain.write(original) }
        }
        _ = AiKeychain.delete()

        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("gyors-cfg-\(UUID().uuidString).json")
        let stagedKey = "sec-a02-migration-\(UUID().uuidString)"
        let body = """
        {
          "ai": { "provider": "openai", "api_key": "\(stagedKey)" },
          "hotkey": "opt+space"
        }
        """
        try? body.write(to: tmp, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: tmp) }

        let cfg = Config.load(from: tmp)
        // Config-level effectiveApiKey reads from Keychain first
        expect(cfg.ai?.effectiveApiKey == stagedKey,
            "post-migration effectiveApiKey must return the staged value, got \(String(describing: cfg.ai?.effectiveApiKey))")
        // Direct Keychain read confirms it landed there
        expect(AiKeychain.read() == stagedKey,
            "Keychain must hold the migrated key")
        // The on-disk JSON must NOT contain api_key any more
        let after = (try? String(contentsOf: tmp)) ?? ""
        expect(!after.contains("api_key"),
            "config.json must no longer contain api_key after migration, got: \(after)")
        // Provider field should survive migration
        expect(after.contains("openai"),
            "non-secret ai fields must be preserved, got: \(after)")
    }

    runGroup("Config.load is idempotent when there's nothing to migrate") {
        let original = AiKeychain.read()
        defer {
            _ = AiKeychain.delete()
            if let original = original { _ = AiKeychain.write(original) }
        }
        _ = AiKeychain.delete()

        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("gyors-cfg-\(UUID().uuidString).json")
        try? "{\"hotkey\": \"opt+space\"}".write(to: tmp, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: tmp) }

        let cfg = Config.load(from: tmp)
        expect(cfg.hotkey == "opt+space", "non-AI config still loads")
        expect(cfg.ai?.effectiveApiKey == nil,
            "no key -> nil, got \(String(describing: cfg.ai?.effectiveApiKey))")
        expect(AiKeychain.read() == nil,
            "Keychain stays empty when there's nothing to migrate")
    }

    runGroup("AiConfig.effectiveApiKey prefers Keychain over inline file value") {
        let original = AiKeychain.read()
        defer {
            _ = AiKeychain.delete()
            if let original = original { _ = AiKeychain.write(original) }
        }

        let keychainValue = "sec-a02-precedence-\(UUID().uuidString)"
        _ = AiKeychain.write(keychainValue)
        // Construct an AiConfig that ALSO has an inline api_key
        // (mirrors the transient state between Keychain write and
        // file rewrite if file rewrite fails). Expect the
        // Keychain value to win
        let cfg = AiConfig(provider: "openai", apiKey: "stale-inline-value")
        expect(cfg.effectiveApiKey == keychainValue,
            "Keychain value must take precedence over file's inline value, got \(String(describing: cfg.effectiveApiKey))")
        _ = AiKeychain.delete()
        // With Keychain empty, fall back to file value
        expect(cfg.effectiveApiKey == "stale-inline-value",
            "with Keychain empty, fall back to inline value, got \(String(describing: cfg.effectiveApiKey))")
    }
}
