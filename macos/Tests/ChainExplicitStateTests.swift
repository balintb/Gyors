import Foundation

func runChainExplicitStateTests() {
// Chain: explicit state, typed `|` stays literal
//
// Chains commit ONLY via the cmd| keyboard gesture, not by typing
// `|` in the TextField. This avoids conflicts with providers
// that legitimately accept `|` in their input: regex alternation,
// YAML block scalars, shell pipelines, AI free text. Pill state
// is authoritative on the VM - NOT derived from `query`

runGroup("vm starts with empty chain state") {
    let vm = GyorsViewModel(bridge: MockBridge())
    expect(vm.chainCommits.isEmpty, "no pills initially")
    expect(vm.activeBuffer == "", "no active text")
    expect(vm.query == "", "no query")
}

runGroup("setActiveBuffer updates active and flat query (no commits)") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.setActiveBuffer("note he")
    expect(vm.chainCommits.isEmpty, "no pill from typing")
    expect(vm.activeBuffer == "note he", "active matches")
    expect(vm.query == "note he", "query is the active text")
    expect(mock.lastPattern == "note he",
        "bridge got unchanged query, not `\(mock.lastPattern)`")
}

runGroup("typed bare `|` stays literal (no space padding)") {
    // `foo|bar` without spaces around the pipe must not chain -
    // this is the shape regex alternation uses (`re cat|dog`)
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.setActiveBuffer("re cat|dog")
    expect(vm.chainCommits.isEmpty,
        "typed bare `|` must not create a pill, got \(vm.chainCommits)")
    expect(vm.activeBuffer == "re cat|dog", "text stays literal")
    expect(mock.lastPattern == "re cat|dog",
        "backend receives the literal text")
}

runGroup("typed ` | ` (space-padded) auto-commits for non-pipe-using base") {
    // Default chain gesture: type space-pipe-space and the
    // text before it becomes a pill automatically
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.setActiveBuffer("note hello | ")
    expect(vm.chainCommits == ["note hello"],
        "committed on typed ` | `, got \(vm.chainCommits)")
    expect(vm.activeBuffer == "", "tail is empty so far")
}

runGroup("typed `| ` (pipe-then-space) auto-commits too") {
    // Covers natural "type pipe without lifting, then
    // space to start next stage" rhythm. Reported missing
    let vm = GyorsViewModel(bridge: MockBridge())
    vm.setActiveBuffer("note helo| ")
    expect(vm.chainCommits == ["note helo"],
        "committed on `| `, got \(vm.chainCommits)")
    expect(vm.activeBuffer == "", "tail empty, ready for next stage")
}

runGroup("typed ` |` (space-then-pipe) trailing commits") {
    // Symmetric to the above: space first, then pipe at EOL
    let vm = GyorsViewModel(bridge: MockBridge())
    vm.setActiveBuffer("note helo |")
    expect(vm.chainCommits == ["note helo"],
        "committed on ` |` trailing, got \(vm.chainCommits)")
    expect(vm.activeBuffer == "", "tail empty")
}

runGroup("typed `|bar` (space-then-pipe without space after) commits") {
    // `foo |bar` - space before pipe, no space after. Still
    // clear separator intent
    let vm = GyorsViewModel(bridge: MockBridge())
    vm.setActiveBuffer("foo |bar")
    expect(vm.chainCommits == ["foo"], "commit")
    expect(vm.activeBuffer == "bar",
        "tail is `bar`, got `\(vm.activeBuffer)`")
}

runGroup("typed `foo|` (bare trailing pipe) stays literal") {
    // No whitespace either side -> user may be mid-typing a
    // literal pipe (regex, URL, etc.). Dont commit yet;
    // wait for a whitespace signal
    let vm = GyorsViewModel(bridge: MockBridge())
    vm.setActiveBuffer("foo|")
    expect(vm.chainCommits.isEmpty, "no commit without whitespace signal")
    expect(vm.activeBuffer == "foo|", "literal text")
}

runGroup("typed ` | ` with hint commits base, keeps hint active") {
    let vm = GyorsViewModel(bridge: MockBridge())
    vm.setActiveBuffer("note hello | copy")
    expect(vm.chainCommits == ["note hello"], "base committed")
    expect(vm.activeBuffer == "copy",
        "hint carried into active, got `\(vm.activeBuffer)`")
}

runGroup("typed ` | ` bails for pipe-using base (regex)") {
    // User typing `re cat | dog` (regex alternation with flanking
    // spaces) means the LITERAL pattern `cat | dog`. Must not
    // commit a pill
    let vm = GyorsViewModel(bridge: MockBridge())
    vm.setActiveBuffer("re cat | dog")
    expect(vm.chainCommits.isEmpty,
        "regex `|` stays literal, got \(vm.chainCommits)")
    expect(vm.activeBuffer == "re cat | dog", "text preserved")
}

runGroup("typed ` | ` bails for pipe-using base (ai transforms)") {
    for kw in ["summarize", "explain", "rewrite", "ai", "ask"] {
        let vm = GyorsViewModel(bridge: MockBridge())
        vm.setActiveBuffer("\(kw) something | else")
        expect(vm.chainCommits.isEmpty,
            "`\(kw) …` must not chain, got \(vm.chainCommits)")
    }
}

runGroup("typed ` | ` bails for pipe-using base (format converters)") {
    for kw in ["yaml2json", "json2yaml", "toml2yaml", "yaml2toml"] {
        let vm = GyorsViewModel(bridge: MockBridge())
        vm.setActiveBuffer("\(kw) text | more")
        expect(vm.chainCommits.isEmpty,
            "`\(kw) …` must not chain, got \(vm.chainCommits)")
    }
}

runGroup("commitActiveAsPill (opt|) bypasses bail list") {
    // Explicit gesture: even in regex context, user can force
    // a commit. The pill-commit path doesn't consult the bail
    // list - that's whole point of exposing a gesture
    let vm = GyorsViewModel(bridge: MockBridge())
    vm.setActiveBuffer("re cat|dog")
    expect(vm.commitActiveAsPill(), "force commit succeeded")
    expect(vm.chainCommits == ["re cat|dog"],
        "literal text committed as pill, got \(vm.chainCommits)")
}

runGroup("commitActiveAsPill promotes active into a pill") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.setActiveBuffer("note hello")
    expect(vm.commitActiveAsPill(), "commit succeeded")
    expect(vm.chainCommits == ["note hello"], "one pill: note hello")
    expect(vm.activeBuffer == "", "active field cleared")
    expect(vm.query == "note hello | ",
        "query is `<pill> | <empty active>`, got `\(vm.query)`")
}

runGroup("commitActiveAsPill refuses blank active") {
    let vm = GyorsViewModel(bridge: MockBridge())
    expect(!vm.commitActiveAsPill(), "nothing to commit from empty")
    expect(vm.chainCommits.isEmpty, "no pill created")
    vm.setActiveBuffer("   ")
    expect(!vm.commitActiveAsPill(), "whitespace-only doesn't commit either")
}

runGroup("commitActiveAsPill trims the pill text") {
    let vm = GyorsViewModel(bridge: MockBridge())
    vm.setActiveBuffer("  padded note  ")
    _ = vm.commitActiveAsPill()
    expect(vm.chainCommits == ["padded note"], "pill trimmed")
}

runGroup("second pill commit appends, doesn't replace") {
    let vm = GyorsViewModel(bridge: MockBridge())
    vm.setActiveBuffer("first")
    _ = vm.commitActiveAsPill()
    vm.setActiveBuffer("second")
    _ = vm.commitActiveAsPill()
    expect(vm.chainCommits == ["first", "second"], "two pills in order")
}

runGroup("popLastChainCommit: refuses when active has text") {
    let vm = GyorsViewModel(bridge: MockBridge())
    vm.setActiveBuffer("note hello")
    _ = vm.commitActiveAsPill()
    vm.setActiveBuffer("cop")
    expect(!vm.popLastChainCommit(), "refused while active is non-empty")
    expect(vm.chainCommits == ["note hello"], "state untouched")
    expect(vm.activeBuffer == "cop", "active untouched")
}

runGroup("popLastChainCommit: pops last pill back into active") {
    let vm = GyorsViewModel(bridge: MockBridge())
    vm.setActiveBuffer("note hello")
    _ = vm.commitActiveAsPill()
    expect(vm.activeBuffer == "", "active cleared after commit")
    expect(vm.popLastChainCommit(), "pop succeeds on empty active")
    expect(vm.chainCommits.isEmpty, "no pills remaining")
    expect(vm.activeBuffer == "note hello", "pill text is now editable")
}

runGroup("popLastChainCommit: no-op without pills") {
    let vm = GyorsViewModel(bridge: MockBridge())
    expect(!vm.popLastChainCommit(), "nothing to pop")
}

runGroup("popLastChainCommit: blocked outside main mode") {
    // Backspace in the note editor must not pop chain pills -
    // that would silently clobber buffered text in editor
    let vm = GyorsViewModel(bridge: MockBridge())
    vm.setActiveBuffer("note hello")
    _ = vm.commitActiveAsPill()
    vm.viewMode = .editor("/tmp/x.md")
    expect(!vm.popLastChainCommit(), "editor mode blocks pop")
}

runGroup("setInput clears pills and replaces active") {
    // Used by setInput effects, Tab-to-title, URL handlers, etc
    let vm = GyorsViewModel(bridge: MockBridge())
    vm.setActiveBuffer("note hello")
    _ = vm.commitActiveAsPill()
    vm.setActiveBuffer("cop")
    vm.setInput("newnote something")
    expect(vm.chainCommits.isEmpty, "pills wiped on setInput")
    expect(vm.activeBuffer == "newnote something",
        "active carries the new text verbatim")
    expect(vm.query == "newnote something",
        "query matches active, no pill prefix")
}

runGroup("reset wipes chain state") {
    // Critical for ESC->reopen to land fresh. Previously a pill
    // left over across panel sessions would invisibly wrap the
    // next query in a chain
    let vm = GyorsViewModel(bridge: MockBridge())
    vm.setActiveBuffer("note hello")
    _ = vm.commitActiveAsPill()
    vm.setActiveBuffer("cop")
    vm.reset()
    expect(vm.chainCommits.isEmpty, "pills cleared")
    expect(vm.activeBuffer == "", "active cleared")
    expect(vm.query == "", "query cleared")
}

}
