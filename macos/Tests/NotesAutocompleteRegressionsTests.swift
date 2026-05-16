import Foundation

func runNotesAutocompleteRegressionsTests() {
// regressions

// End-to-end: note autocomplete typing flow
//
// The "note he shows only Create" bug has resurfaced more than
// once. These tests drive setActiveBuffer char-by-char against
// MockBridge and pin every property of the VM + bridge at each
// step, so any re-regression lights up immediately

runGroup("e2e notes: fresh typing `note he` sends clean pattern") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    var buf = ""
    for c in "note he" {
        buf.append(c)
        vm.setActiveBuffer(buf)
    }
    expect(vm.chainCommits.isEmpty,
        "no pills for a plain note filter, got \(vm.chainCommits)")
    expect(vm.activeBuffer == "note he",
        "active is literal text, got `\(vm.activeBuffer)`")
    expect(vm.query == "note he",
        "query is literal, got `\(vm.query)`")
    expect(mock.lastPattern == "note he",
        "bridge received `\(mock.lastPattern)`")
}

runGroup("e2e notes: every keystroke fires bridge with prefix-so-far") {
    // Confirms incremental querying - not batched or mangled
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    let expected = ["n", "no", "not", "note", "note ",
                    "note h", "note he"]
    var patterns: [String] = []
    var buf = ""
    for c in "note he" {
        buf.append(c)
        vm.setActiveBuffer(buf)
        patterns.append(mock.lastPattern)
    }
    expect(patterns == expected,
        "expected prefix-so-far pattern stream, got \(patterns)")
}

runGroup("e2e notes: reset before typing clears any stale state") {
    // Users open and close the panel repeatedly; each show()
    // must reset so leftover pills from a previous session
    // dont leak into next query
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.setActiveBuffer("old query")
    _ = vm.commitActiveAsPill()
    expect(vm.chainCommits == ["old query"], "pre-state has a pill")
    vm.reset()  // what PanelController.show() calls
    expect(vm.chainCommits.isEmpty, "reset wiped pills")
    expect(vm.activeBuffer == "", "reset wiped active")
    // Fresh typing from here MUST NOT inherit the stale pill
    vm.setActiveBuffer("note he")
    expect(vm.chainCommits.isEmpty,
        "fresh typing after reset is pill-free, got \(vm.chainCommits)")
    expect(mock.lastPattern == "note he",
        "bridge sees `note he`, got `\(mock.lastPattern)`")
}

runGroup("e2e notes: various keyword aliases all send the literal form") {
    for query in ["note he", "notes he", "n he"] {
        let mock = MockBridge()
        let vm = GyorsViewModel(bridge: mock)
        vm.setActiveBuffer(query)
        expect(mock.lastPattern == query,
            "alias `\(query)` sent `\(mock.lastPattern)`")
    }
}

runGroup("e2e notes: `#` shorthand sent literally") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.setActiveBuffer("#he")
    expect(mock.lastPattern == "#he", "got `\(mock.lastPattern)`")
    expect(vm.chainCommits.isEmpty, "# doesn't commit pill")
}

runGroup("e2e notes: `findnote` + content query") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.setActiveBuffer("findnote world")
    expect(mock.lastPattern == "findnote world",
        "got `\(mock.lastPattern)`")
}

runGroup("e2e notes: trailing space on filter preserved") {
    // User types `note he ` before continuing to ` |` etc.
    // The trailing space must NOT vanish
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.setActiveBuffer("note he ")
    expect(vm.activeBuffer == "note he ",
        "trailing space kept, got `\(vm.activeBuffer)`")
    expect(mock.lastPattern == "note he ",
        "bridge sees trailing space, got `\(mock.lastPattern)`")
}

runGroup("e2e notes: chaining on note keeps pattern shape") {
    // `note helo | preview` - pill forms, bridge sees the
    // canonical flat form Rust expects
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    var buf = ""
    for c in "note helo | preview" {
        buf.append(c)
        vm.setActiveBuffer(buf)
    }
    expect(vm.chainCommits == ["note helo"],
        "pill minted, got \(vm.chainCommits)")
    expect(vm.activeBuffer == "preview",
        "tail is `preview`, got `\(vm.activeBuffer)`")
    expect(mock.lastPattern == "note helo | preview",
        "bridge sees canonical flat, got `\(mock.lastPattern)`")
}

runGroup("e2e notes: backspace from pill-only state recovers base") {
    // After committing `note helo | `, backspace on empty
    // active pops pill back into editable text - the user
    // should then see `note helo` in buffer and correct
    // matches from backend
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.setActiveBuffer("note helo")
    _ = vm.commitActiveAsPill()
    expect(vm.chainCommits == ["note helo"] && vm.activeBuffer == "",
        "pre-state")
    _ = vm.popLastChainCommit()
    expect(vm.chainCommits.isEmpty, "pill popped")
    expect(vm.activeBuffer == "note helo",
        "active restored to pill text, got `\(vm.activeBuffer)`")
    expect(mock.lastPattern == "note helo",
        "bridge sees clean `note helo`, got `\(mock.lastPattern)`")
}

runGroup("regression: typing `note he` returns notes, not just create row") {
    // The bug: backend was receiving a chain-shaped query with
    // leftover pills, so `note he` got parsed as something else.
    // With explicit pill state + setInput on reset, vm.query
    // equals exactly what user typed
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.setActiveBuffer("note he")
    expect(mock.lastPattern == "note he",
        "bridge received `\(mock.lastPattern)`, expected `note he`")
    expect(vm.query == "note he",
        "vm.query is `\(vm.query)`, expected `note he`")
}

runGroup("regression: typed bare `|` never creates pills") {
    // Char-by-char typing of `note helo|d` (no spaces) must
    // stay a literal 12-char string. No commits
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    var buffer = ""
    for c in "note helo|d" {
        buffer.append(c)
        vm.setActiveBuffer(buffer)
    }
    expect(vm.chainCommits.isEmpty,
        "no pill from bare `|`, got \(vm.chainCommits)")
    expect(vm.activeBuffer == "note helo|d",
        "literal string preserved")
    expect(mock.lastPattern == "note helo|d",
        "backend received the literal")
}

runGroup("regression: typed space-pipe-space char-by-char → one pill") {
    // Typing `note hello | copy` one char at a time must end
    // up as commits=["note hello"], active="copy" - and NOT
    // multiple pills from stale-buffer re-parsing
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    var buffer = ""
    for c in "note hello | copy" {
        buffer.append(c)
        vm.setActiveBuffer(buffer)
    }
    expect(vm.chainCommits == ["note hello"],
        "exactly one pill, got \(vm.chainCommits)")
    expect(vm.activeBuffer == "copy",
        "active is `copy`, got `\(vm.activeBuffer)`")
}

runGroup("regression: stale-buffer delivery of committed-prefix doesn't duplicate") {
    // Direct simulation: after a commit, the TextField still
    // has "note hello | " in its internal buffer. Next
    // keystroke delivers the stale string plus a char. The
    // setActiveBuffer strip-prefix guard must detect and
    // handle this without creating a duplicate pill
    let vm = GyorsViewModel(bridge: MockBridge())
    vm.setActiveBuffer("note hello | ")
    expect(vm.chainCommits == ["note hello"], "first commit fired")
    expect(vm.activeBuffer == "", "active cleared")
    // Simulate the stale TextField sending "note hello | d"
    vm.setActiveBuffer("note hello | d")
    expect(vm.chainCommits == ["note hello"],
        "still exactly one pill, got \(vm.chainCommits)")
    expect(vm.activeBuffer == "d",
        "tail extracted, got `\(vm.activeBuffer)`")
    // Another stale delivery: "note hello | de"
    vm.setActiveBuffer("note hello | de")
    expect(vm.chainCommits == ["note hello"], "still one pill")
    expect(vm.activeBuffer == "de", "tail grown to `de`")
}

runGroup("regression: two-chain sequence char-by-char is stable") {
    // `note hello | copy | preview` -> two pills + `preview` tail.
    // Guards against "each commit spawns two pills" variant
    let vm = GyorsViewModel(bridge: MockBridge())
    var buffer = ""
    for c in "note hello | copy | preview" {
        buffer.append(c)
        vm.setActiveBuffer(buffer)
    }
    expect(vm.chainCommits == ["note hello", "copy"],
        "two pills in order, got \(vm.chainCommits)")
    expect(vm.activeBuffer == "preview",
        "final segment is active")
}

runGroup("regression: committing twice in a row doesn't double") {
    // Classic "each commit produces two pills" regression
    let vm = GyorsViewModel(bridge: MockBridge())
    vm.setActiveBuffer("first")
    _ = vm.commitActiveAsPill()
    vm.setActiveBuffer("second")
    _ = vm.commitActiveAsPill()
    vm.setActiveBuffer("third")
    _ = vm.commitActiveAsPill()
    expect(vm.chainCommits == ["first", "second", "third"],
        "exactly three pills, got \(vm.chainCommits)")
    expect(vm.activeBuffer == "", "active cleared after last commit")
}

runGroup("regression: pop then re-commit doesn't duplicate") {
    let vm = GyorsViewModel(bridge: MockBridge())
    vm.setActiveBuffer("foo")
    _ = vm.commitActiveAsPill()
    _ = vm.popLastChainCommit()
    expect(vm.chainCommits.isEmpty, "pill gone")
    expect(vm.activeBuffer == "foo", "text editable again")
    _ = vm.commitActiveAsPill()
    expect(vm.chainCommits == ["foo"],
        "exactly one pill, got \(vm.chainCommits)")
}

runGroup("regression: arrow stays literal") {
    // `>` is shell-mode trigger only when leading. Mid-string,
    // it's literal text - especially important for queries
    // like `echo foo > bar` in shell mode
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.setActiveBuffer("echo foo > bar")
    expect(vm.chainCommits.isEmpty, "no pill from `>`")
    expect(vm.activeBuffer == "echo foo > bar", "preserved literally")
    expect(mock.lastPattern == "echo foo > bar",
        "backend sees it exactly")
}

}
