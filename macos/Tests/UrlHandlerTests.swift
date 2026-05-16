import Foundation

func runUrlHandlerTests() {


func encodePluginSpecBase64(_ json: String) -> String {
    let data = json.data(using: .utf8)!
    // URL-safe base64 (Gyors's parser tolerates both variants,
    // but we emit URL-safe form that browsers survive)
    var s = data.base64EncodedString()
        .replacingOccurrences(of: "+", with: "-")
        .replacingOccurrences(of: "/", with: "_")
    // Strip padding - decoder re-adds it
    while s.hasSuffix("=") { s.removeLast() }
    return s
}

runGroup("parse gyors://plugin/install accepts base64 spec") {
    let spec = #"{"id":"weather","name":"Weather","keywords":["w"],"command":"curl wttr.in/{query}"}"#
    let b64 = encodePluginSpecBase64(spec)
    let u = URL(string: "gyors://plugin/install?spec=\(b64)")!
    if case .pluginInstall(let specJson) = GyorsUrlHandler.parse(u) {
        expect(specJson.contains("weather"), "decoded spec has `weather`, got: \(specJson)")
        expect(specJson.hasPrefix("{"), "decoded spec is JSON object: \(specJson)")
    } else {
        expect(false, "expected .pluginInstall, got \(GyorsUrlHandler.parse(u))")
    }
}

runGroup("parse gyors://plugin/install accepts percent-encoded JSON spec") {
    let spec = #"{"id":"w","name":"W","keywords":["w"],"command":"echo x"}"#
    let encoded = spec.addingPercentEncoding(
        withAllowedCharacters: .urlQueryAllowed
    )!
    let u = URL(string: "gyors://plugin/install?spec=\(encoded)")!
    if case .pluginInstall(let specJson) = GyorsUrlHandler.parse(u) {
        expect(specJson == spec, "percent-decoded JSON survives verbatim")
    } else {
        expect(false, "expected .pluginInstall, got \(GyorsUrlHandler.parse(u))")
    }
}

runGroup("parse gyors://plugin without spec is invalid") {
    let u = URL(string: "gyors://plugin/install")!
    if case .invalid = GyorsUrlHandler.parse(u) {
        expect(true, "invalid")
    } else {
        expect(false, "expected .invalid, got \(GyorsUrlHandler.parse(u))")
    }
}

runGroup("parse gyors://plugin/install with garbage spec is invalid") {
    // Neither base64 of an object nor percent-encoded JSON
    let u = URL(string: "gyors://plugin/install?spec=not-a-spec")!
    if case .invalid = GyorsUrlHandler.parse(u) {
        expect(true, "invalid")
    } else {
        expect(false, "expected .invalid, got \(GyorsUrlHandler.parse(u))")
    }
}

runGroup("parse gyors://plugin/<other-subcommand> is invalid") {
    // Protocol is install-only for now; any other subcommand
    // must be explicitly rejected so future additions can't
    // silently misfire
    for sub in ["uninstall", "list", "update"] {
        let u = URL(string: "gyors://plugin/\(sub)?spec=x")!
        if case .invalid = GyorsUrlHandler.parse(u) {
            expect(true, sub)
        } else {
            expect(false, "expected .invalid for plugin/\(sub)")
        }
    }
}

runGroup("parse gyors://plugin/install tolerates base64 without padding") {
    let spec = #"{"id":"x","name":"X","keywords":["x"],"command":"echo x"}"#
    let raw = spec.data(using: .utf8)!.base64EncodedString()
    // Ensure we actually have padding to strip on at least
    // one valid input; re-pad and check decode works
    let unpadded = raw.replacingOccurrences(of: "=", with: "")
    let u = URL(string: "gyors://plugin/install?spec=\(unpadded)")!
    if case .pluginInstall(let json) = GyorsUrlHandler.parse(u) {
        expect(json == spec, "unpadded base64 survives decode")
    } else {
        expect(false, "expected .pluginInstall, got \(GyorsUrlHandler.parse(u))")
    }
}

runGroup("parse rejects non-gyors:// schemes") {
    let u = URL(string: "https://example.com/set/theme?value=x")!
    if case .invalid = GyorsUrlHandler.parse(u) { expect(true, "invalid") }
    else { expect(false, "expected invalid") }
}

runGroup("parse routes gyors://theme to .theme") {
    let u = URL(string: "gyors://theme?import=abc")!
    expect(GyorsUrlHandler.parse(u) == .theme, "routed")
}

runGroup("parse gyors://set/theme?value=midnight") {
    let u = URL(string: "gyors://set/theme?value=midnight")!
    expect(
        GyorsUrlHandler.parse(u) == .set(.theme, "midnight"),
        "set(theme, midnight)"
    )
}

runGroup("parse gyors://set?key=theme&value=x also works") {
    let u = URL(string: "gyors://set?key=theme&value=nord")!
    expect(
        GyorsUrlHandler.parse(u) == .set(.theme, "nord"),
        "query-form set"
    )
}

runGroup("parse gyors://set/notes-folder accepts dash") {
    let u = URL(string: "gyors://set/notes-folder?value=/tmp/x")!
    expect(
        GyorsUrlHandler.parse(u) == .set(.notesFolder, "/tmp/x"),
        "dashed key"
    )
}

runGroup("parse set with empty value is invalid") {
    let u = URL(string: "gyors://set/theme?value=")!
    if case .invalid = GyorsUrlHandler.parse(u) { expect(true, "invalid") }
    else { expect(false, "expected invalid for empty value") }
}

runGroup("parse set with unknown key is invalid") {
    // Unknown keys USED to fall through to a free-form
    // `setRaw` write (still confirmation-gated, but able to
    // propose any config.json edit). That was too much power for
    // a URL handler. Unknown keys are now rejected outright; new
    // settable keys go in typed whitelist or Rust's FIELDS
    let u = URL(string: "gyors://set/test-key?value=false")!
    if case .invalid(let reason) = GyorsUrlHandler.parse(u) {
        expect(reason.contains("test-key"), "reason names the key: \(reason)")
    } else {
        expect(false, "expected .invalid for unknown key, got \(GyorsUrlHandler.parse(u))")
    }
}

runGroup("parse set with empty key is invalid") {
    let u = URL(string: "gyors://set?value=x")!
    if case .invalid = GyorsUrlHandler.parse(u) { expect(true, "invalid") }
    else { expect(false, "expected invalid for empty key") }
}

runGroup("parse gyors://toggle/clipboard-enabled") {
    let u = URL(string: "gyors://toggle/clipboard-enabled")!
    expect(
        GyorsUrlHandler.parse(u) == .toggle(.clipboardEnabled),
        "toggle(clipboard)"
    )
}

runGroup("parse toggle on non-boolean key is invalid") {
    let u = URL(string: "gyors://toggle/theme")!
    if case .invalid = GyorsUrlHandler.parse(u) { expect(true, "invalid") }
    else { expect(false, "expected invalid for non-boolean toggle") }
}

runGroup("parse unknown host is invalid") {
    let u = URL(string: "gyors://nope/whatever")!
    if case .invalid = GyorsUrlHandler.parse(u) { expect(true, "invalid") }
    else { expect(false, "expected invalid for unknown host") }
}

// gyors://open?q= sigil + control-char rejection

runGroup("parse open with a regular query is fine") {
    let u = URL(string: "gyors://open?q=hello%20world")!
    if case .openWithQuery(let text) = GyorsUrlHandler.parse(u) {
        expect(text == "hello world", "text decoded: \(text)")
    } else {
        expect(false, "expected .openWithQuery, got \(GyorsUrlHandler.parse(u))")
    }
}

runGroup("parse open rejects shell sigil >") {
    let u = URL(string: "gyors://open?q=%3E%20ls")!  // "> ls"
    if case .invalid(let reason) = GyorsUrlHandler.parse(u) {
        expect(reason.contains("sigil"), "reason mentions sigil: \(reason)")
    } else {
        expect(false, "expected .invalid for > sigil")
    }
}

runGroup("parse open rejects force-shell sigil >!") {
    let u = URL(string: "gyors://open?q=%3E%21%20ls")!  // ">! ls"
    if case .invalid = GyorsUrlHandler.parse(u) {
        expect(true, "invalid")
    } else {
        expect(false, "expected .invalid for >! sigil")
    }
}

runGroup("parse open rejects chain sigil &") {
    let u = URL(string: "gyors://open?q=%26step")!  // "&step"
    if case .invalid = GyorsUrlHandler.parse(u) {
        expect(true, "invalid")
    } else {
        expect(false, "expected .invalid for & sigil")
    }
}

runGroup("parse open rejects sigil after leading whitespace") {
    // Trimmed-content check - leading space / tab in URL
    // shouldn't disguise a sigil
    let u = URL(string: "gyors://open?q=%20%3E%20ls")!  // " > ls"
    if case .invalid = GyorsUrlHandler.parse(u) {
        expect(true, "invalid")
    } else {
        expect(false, "expected .invalid for trimmed > sigil")
    }
}

runGroup("parse open allows > embedded mid-string") {
    // The risk is privileged routing on the FIRST non-whitespace
    // character; a literal `>` mid-string is fine (URLs about
    // git diffs, search "a > b" comparisons, etc)
    let u = URL(string: "gyors://open?q=a%20%3E%20b")!  // "a > b"
    if case .openWithQuery(let text) = GyorsUrlHandler.parse(u) {
        expect(text == "a > b", "mid-string > kept: \(text)")
    } else {
        expect(false, "expected .openWithQuery, got \(GyorsUrlHandler.parse(u))")
    }
}

runGroup("parse open rejects embedded NUL") {
    let u = URL(string: "gyors://open?q=hello%00world")!
    if case .invalid(let reason) = GyorsUrlHandler.parse(u) {
        expect(reason.contains("control"), "reason mentions control: \(reason)")
    } else {
        expect(false, "expected .invalid for NUL")
    }
}

runGroup("parse open rejects embedded SOH (\\x01)") {
    let u = URL(string: "gyors://open?q=hello%01world")!
    if case .invalid = GyorsUrlHandler.parse(u) {
        expect(true, "invalid")
    } else {
        expect(false, "expected .invalid for \\x01")
    }
}

}
