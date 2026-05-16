// AI-only - collapses to nothing when WITH_AI=0
#if AI
import Foundation
import Security

/// Keychain-backed store for AI provider's API key
///
/// ## Why
///
/// storing AI API key in plaintext `config.json` meant
/// local malware iterating `~/Library/Application Support/*` could
/// exfiltrate key (and drain user's OpenAI / Anthropic balance).
/// Keychain items get a per-bundle ACL that production-signed
/// builds restrict to `com.gyors.gyors`; key never appears on disk
/// in plaintext
///
/// ## Service + account names
///
/// Service: `com.gyors.ai`, account: `api_key`. Mirrors pattern
/// used by `gyors-sync::keychain` (`com.gyors.sync` / `session`).
/// One entry per service/account pair; Keychain looks up by that
/// pair on every operation
enum AiKeychain {
    static let service = "com.gyors.ai"
    static let account = "api_key"

    /// Read stored key, or nil if no entry exists. Surface other
    /// errors via NSLog without throwing - AI path already has a
    /// "missing API key" branch and we want to fall into it rather
    /// than crash on a Keychain hiccup
    static func read() -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        if status == errSecItemNotFound { return nil }
        if status != errSecSuccess {
            NSLog("gyors: ai keychain read failed: status=\(status)")
            return nil
        }
        guard let data = item as? Data, let s = String(data: data, encoding: .utf8) else {
            return nil
        }
        return s
    }

    /// Upsert. Idempotent. Returns true on success
    @discardableResult
    static func write(_ key: String) -> Bool {
        guard let data = key.data(using: .utf8) else { return false }
        // Try update first - we may already have an entry
        let lookup: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        let update: [String: Any] = [kSecValueData as String: data]
        let updateStatus = SecItemUpdate(lookup as CFDictionary, update as CFDictionary)
        if updateStatus == errSecSuccess { return true }
        if updateStatus != errSecItemNotFound {
            NSLog("gyors: ai keychain update failed: status=\(updateStatus)")
            // Fall through to add - on some macOS releases an
            // existing item under a different access group also
            // shows up as ItemNotFound for update + AlreadyExists
            // for add; we handle latter below
        }
        var add = lookup
        add[kSecValueData as String] = data
        let addStatus = SecItemAdd(add as CFDictionary, nil)
        if addStatus == errSecSuccess { return true }
        if addStatus == errSecDuplicateItem {
            // Race: someone else added between our update + add.
            // Try update one more time
            return SecItemUpdate(lookup as CFDictionary, update as CFDictionary) == errSecSuccess
        }
        NSLog("gyors: ai keychain add failed: status=\(addStatus)")
        return false
    }

    /// Idempotent delete - missing entry counts as success
    @discardableResult
    static func delete() -> Bool {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        let status = SecItemDelete(query as CFDictionary)
        return status == errSecSuccess || status == errSecItemNotFound
    }
}

#endif // AI
