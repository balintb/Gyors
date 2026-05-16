import Foundation

/// AI router - config decoding (LooseBool + router_enabled + alongside provider)
func runAiRouterConfigTests() {
runGroup("LOOSE BOOL: decodes JSON booleans") {
    // The shape `config set ai.router_enabled true` writes:
    // a real JSON `true`, not a string. Pin the bool path
    // first since this was original source of the
    // silent-fail bug
    let json = "{\"router_enabled\": true}".data(using: .utf8)!
    let decoded = try? JSONDecoder().decode(AiConfig.self, from: json)
    expect(decoded != nil, "decoded successfully")
    expect(decoded?.routerEnabled?.value == true,
        "JSON true → LooseBool.value true")
}

runGroup("LOOSE BOOL: decodes JSON false") {
    let json = "{\"router_enabled\": false}".data(using: .utf8)!
    let decoded = try? JSONDecoder().decode(AiConfig.self, from: json)
    expect(decoded?.routerEnabled?.value == false,
        "JSON false → LooseBool.value false")
}

runGroup("LOOSE BOOL: decodes truthy/falsy strings") {
    // Hand-edited config files often use strings -
    // accommodate so a typo'd quote doesn't silently
    // drop the whole `ai` block
    for truthy in ["true", "yes", "on", "1", "TRUE", "Yes"] {
        let json = "{\"router_enabled\": \"\(truthy)\"}".data(using: .utf8)!
        let decoded = try? JSONDecoder().decode(AiConfig.self, from: json)
        expect(decoded?.routerEnabled?.value == true,
            "string `\(truthy)` → true")
    }
    for falsy in ["false", "no", "off", "0", "garbage", ""] {
        let json = "{\"router_enabled\": \"\(falsy)\"}".data(using: .utf8)!
        let decoded = try? JSONDecoder().decode(AiConfig.self, from: json)
        expect(decoded?.routerEnabled?.value == false,
            "string `\(falsy)` → false")
    }
}

runGroup("LOOSE BOOL: decodes integer 1/0") {
    // Some users hand-edit with C-style booleans
    let one = "{\"router_enabled\": 1}".data(using: .utf8)!
    expect(
        (try? JSONDecoder().decode(AiConfig.self, from: one))?
            .routerEnabled?.value == true,
        "1 → true")
    let zero = "{\"router_enabled\": 0}".data(using: .utf8)!
    expect(
        (try? JSONDecoder().decode(AiConfig.self, from: zero))?
            .routerEnabled?.value == false,
        "0 → false")
}

runGroup("LOOSE BOOL: missing key decodes as nil") {
    // The flag is optional; absence means "off" via
    // `effectiveRouterEnabled`'s `?? false`. The decode
    // itself must succeed so rest of the
    // `ai` block (provider, model, ...) survives
    let json = "{\"provider\": \"apple\"}".data(using: .utf8)!
    let decoded = try? JSONDecoder().decode(AiConfig.self, from: json)
    expect(decoded != nil, "decoded with missing router_enabled")
    expect(decoded?.routerEnabled == nil, "absent → nil")
    expect(decoded?.provider == "apple", "other fields preserved")
}

runGroup("LOOSE BOOL: nonsense JSON value decodes as false") {
    // REGRESSION (2026-04-28): a typed `String?` field
    // failed JSON decode when the user's `config set`
    // wrote a JSON bool - taking down entire `ai`
    // block silently. Defensive: accept anything, default
    // false on weird shapes (array, object, null), so
    // routing is just "off" rather than "everything
    // disappears."
    let arr = "{\"router_enabled\": []}".data(using: .utf8)!
    let arrDecoded = try? JSONDecoder().decode(AiConfig.self, from: arr)
    expect(arrDecoded != nil, "decoded despite array value")
    expect(arrDecoded?.routerEnabled?.value == false,
        "array → false (not throw)")
}

runGroup("ROUTER: real-world config.json (bool form) parses end-to-end") {
    // Mirror the EXACT shape `config set ai.router_enabled true`
    // writes. End-to-end: decode the full Config, walk through
    // `effectiveRouterEnabled` - both sides must agree on `true`.
    // Without LooseBool this fails-decode and routing silently
    // stays off even though file says true
    let json = """
        {
          "ai": {
            "provider": "apple",
            "router_enabled": true
          },
          "hotkey": "opt+shift+space",
          "theme": "tokyo-night"
        }
        """.data(using: .utf8)!
    let cfg = try? JSONDecoder().decode(Config.self, from: json)
    expect(cfg != nil, "full config decodes")
    expect(cfg?.effectiveRouterEnabled == true,
        "router shows as on at the Config level")
    expect(cfg?.ai?.provider == "apple",
        "ai.provider survived alongside router_enabled")
}
}
