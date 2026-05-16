import Foundation

/// Coverage. Verifies that `Effect.isAllowedOpenUrlScheme`
/// only greenlights the four schemes the launcher actually wants
/// (`http`, `https`, `mailto`, `gyors`) and rejects everything else
/// - especially `file:`, `javascript:`, and obscure protocols
/// `NSWorkspace.shared.open(_:)` would otherwise dispatch
func runEffectOpenUrlAllowlistTests() {

runGroup("openUrl allowlist - http allowed") {
    expect(Effect.isAllowedOpenUrlScheme("http://example.com"), "http://example.com allowed")
}

runGroup("openUrl allowlist - https allowed") {
    expect(Effect.isAllowedOpenUrlScheme("https://example.com"), "https://example.com allowed")
}

runGroup("openUrl allowlist - mailto allowed") {
    expect(Effect.isAllowedOpenUrlScheme("mailto:user@example.com"), "mailto: allowed")
}

runGroup("openUrl allowlist - gyors:// allowed") {
    expect(Effect.isAllowedOpenUrlScheme("gyors://open?q=foo"), "gyors:// allowed")
}

runGroup("openUrl allowlist - file:/// rejected") {
    expect(!Effect.isAllowedOpenUrlScheme("file:///etc/passwd"), "file:/// rejected")
}

runGroup("openUrl allowlist - javascript: rejected") {
    expect(!Effect.isAllowedOpenUrlScheme("javascript:alert(1)"), "javascript: rejected")
}

runGroup("openUrl allowlist - custom scheme rejected") {
    expect(!Effect.isAllowedOpenUrlScheme("evilapp://payload"), "custom scheme rejected")
    expect(!Effect.isAllowedOpenUrlScheme("vnc://10.0.0.1"), "vnc: rejected")
    expect(!Effect.isAllowedOpenUrlScheme("smb://share/secret"), "smb: rejected")
    expect(!Effect.isAllowedOpenUrlScheme("tel:+15555551212"), "tel: rejected")
}

runGroup("openUrl allowlist - empty string rejected") {
    expect(!Effect.isAllowedOpenUrlScheme(""), "empty rejected")
    expect(!Effect.isAllowedOpenUrlScheme("   "), "whitespace-only rejected")
}

runGroup("openUrl allowlist - malformed URL rejected") {
    expect(!Effect.isAllowedOpenUrlScheme("not a url at all"), "no-scheme text rejected")
    expect(!Effect.isAllowedOpenUrlScheme("://nope"), "missing scheme rejected")
}

runGroup("openUrl allowlist - scheme is case-insensitive") {
    expect(Effect.isAllowedOpenUrlScheme("HTTPS://example.com"), "uppercase HTTPS allowed")
    expect(Effect.isAllowedOpenUrlScheme("Mailto:user@example.com"), "mixed-case mailto allowed")
}

}
