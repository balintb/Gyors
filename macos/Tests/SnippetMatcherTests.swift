import Foundation

func runSnippetMatcherTests() {

runGroup("snippet matcher: finds trigger at end of buffer") {
    let snippets = ["sig": "Signed,\nMe", "date": "2026-04-23"]
    let hit = GlobalSnippetMatcher.findTriggerMatch(
        buffer: "hello ;sig;",
        longestTrigger: 4,
        snippets: snippets
    )
    expect(hit == GlobalSnippetMatcher.TriggerMatch(text: "Signed,\nMe", deleteCount: 5),
           "hit=\(String(describing: hit))")
}

runGroup("snippet matcher: requires closing semicolon") {
    let snippets = ["sig": "x"]
    let hit = GlobalSnippetMatcher.findTriggerMatch(
        buffer: "hello ;sig",
        longestTrigger: 4,
        snippets: snippets
    )
    expect(hit == nil, "no closing `;` must not match")
}

runGroup("snippet matcher: ignores unknown trigger") {
    let snippets = ["sig": "x"]
    let hit = GlobalSnippetMatcher.findTriggerMatch(
        buffer: "hello ;ghost;",
        longestTrigger: 4,
        snippets: snippets
    )
    expect(hit == nil, "unknown trigger stays literal")
}

runGroup("snippet matcher: empty trigger stays literal") {
    // `;;` is the user typing two literal semicolons, not a
    // zero-width trigger. Explicit guard against empty
    let hit = GlobalSnippetMatcher.findTriggerMatch(
        buffer: ";;",
        longestTrigger: 4,
        snippets: ["": "should-not-fire"]
    )
    expect(hit == nil, "empty trigger ignored")
}

runGroup("snippet matcher: search window respects longestTrigger") {
    // Opening `;` is further back than `longestTrigger + 1`;
    // we must NOT match - otherwise O(n) scans on huge pastes
    // could trip back-matching across unrelated input
    let snippets = ["needle": "found"]
    let longBuffer = ";needle" + String(repeating: "x", count: 50) + ";"
    let hit = GlobalSnippetMatcher.findTriggerMatch(
        buffer: longBuffer,
        longestTrigger: 6, // "needle".count = 6 but window truncates
        snippets: snippets
    )
    expect(hit == nil, "opening `;` outside scan window must not match")
}

runGroup("snippet matcher: first opening `;` wins") {
    // `;;sig;` has TWO opening semicolons; the one adjacent to
    // `sig` is the correct match, not the earlier one
    let snippets = ["sig": "x"]
    let hit = GlobalSnippetMatcher.findTriggerMatch(
        buffer: ";;sig;",
        longestTrigger: 4,
        snippets: snippets
    )
    expect(hit?.text == "x", "innermost `;` wins, got=\(String(describing: hit))")
    expect(hit?.deleteCount == 5, "delete `;sig;` only, got=\(String(describing: hit?.deleteCount))")
}

runGroup("snippet matcher: buffer shorter than 3 never matches") {
    // `;x` has no terminator; `;;` has empty trigger
    for buf in [";", ";x", ";;"] {
        let hit = GlobalSnippetMatcher.findTriggerMatch(
            buffer: buf,
            longestTrigger: 4,
            snippets: ["x": "X"]
        )
        expect(hit == nil, "buf=\(buf) must not match")
    }
}

runGroup("snippet matcher: appendToBuffer drops control chars") {
    var buffer = "hello"
    // A soft-hyphen (U+00AD, default-ignorable) must be filtered
    GlobalSnippetMatcher.appendToBuffer("\u{00AD}", into: &buffer, max: 128)
    expect(buffer == "hello", "default-ignorable dropped, got \(buffer)")
}

runGroup("snippet matcher: appendToBuffer resets on newline") {
    var buffer = "older-line"
    GlobalSnippetMatcher.appendToBuffer("\n", into: &buffer, max: 128)
    expect(buffer.isEmpty, "newline wipes, got \(buffer)")
}

runGroup("snippet matcher: appendToBuffer resets on tab") {
    var buffer = "partial"
    GlobalSnippetMatcher.appendToBuffer("\t", into: &buffer, max: 128)
    expect(buffer.isEmpty, "tab wipes, got \(buffer)")
}

runGroup("snippet matcher: appendToBuffer caps length") {
    var buffer = String(repeating: "a", count: 30)
    GlobalSnippetMatcher.appendToBuffer("bcdef", into: &buffer, max: 10)
    expect(buffer.count == 10, "count=\(buffer.count)")
    expect(buffer.hasSuffix("abcdef"), "newest chars kept, got \(buffer)")
}

runGroup("snippet matcher: multi-char event string lands in order") {
    // IME / paste events can deliver multiple chars at once -
    // buffer mustn't shuffle their order
    var buffer = ""
    GlobalSnippetMatcher.appendToBuffer("abc", into: &buffer, max: 128)
    expect(buffer == "abc", "got \(buffer)")
}

}
