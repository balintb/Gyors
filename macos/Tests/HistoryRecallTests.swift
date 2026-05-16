import Foundation

func runHistoryRecallTests() {

runGroup("history: fresh panel with empty buffer, ↑ recalls newest") {
    let mock = MockBridge()
    mock.stubRecentQueries = ["calc 2+2", "note foo", "safari"]
    let vm = GyorsViewModel(bridge: mock)
    vm.reset() // snapshots from bridge
    vm.handleUpKey()
    expect(vm.activeBuffer == "calc 2+2", "newest recalled, got \(vm.activeBuffer)")
}

runGroup("history: ↑ ↑ walks back to older entries") {
    let mock = MockBridge()
    mock.stubRecentQueries = ["calc 2+2", "note foo", "safari"]
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    vm.handleUpKey()
    vm.handleUpKey()
    expect(vm.activeBuffer == "note foo", "second-most-recent, got \(vm.activeBuffer)")
    vm.handleUpKey()
    expect(vm.activeBuffer == "safari", "oldest, got \(vm.activeBuffer)")
}

runGroup("history: ↑ past oldest entry stays put") {
    let mock = MockBridge()
    mock.stubRecentQueries = ["only one"]
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    vm.handleUpKey()
    expect(vm.activeBuffer == "only one", "first hit")
    vm.handleUpKey()
    expect(vm.activeBuffer == "only one", "further ↑ doesn't scramble")
}

runGroup("history: ↓ walks back forward toward current") {
    let mock = MockBridge()
    mock.stubRecentQueries = ["newest", "older"]
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    vm.handleUpKey() // newest
    vm.handleUpKey() // older
    expect(vm.activeBuffer == "older", "walked back")
    vm.handleDownKey() // back to newest
    expect(vm.activeBuffer == "newest", "stepped forward")
}

runGroup("history: ↓ past newest restores pre-recall buffer") {
    // Pre-recall buffer is "" by contract (recall only triggers
    // from an empty buffer). Down-past-top must return there so the
    // input bar visibly "exits" recall mode
    let mock = MockBridge()
    mock.stubRecentQueries = ["a prior"]
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    vm.handleUpKey()
    expect(vm.activeBuffer == "a prior", "recalled")
    vm.handleDownKey()
    expect(vm.activeBuffer == "", "exited recall, buffer clear - got \(vm.activeBuffer)")
}

runGroup("history: typing exits recall") {
    let mock = MockBridge()
    mock.queryResponse = []
    mock.stubRecentQueries = ["prior"]
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    vm.handleUpKey()
    expect(vm.activeBuffer == "prior", "recalled")
    vm.setActiveBuffer("prior!") // user edits
    // Next Up should NOT re-walk history (buffer isn't empty,
    // and recall state was cleared)
    vm.results = [Candidate(id: "x", title: "X", subtitle: "", iconKind: 0, iconValue: "", kind: 0, score: 0, actions: [])]
    vm.selectedIndex = 0
    vm.handleUpKey()
    expect(vm.activeBuffer == "prior!", "typing preserved, recall exited")
}

runGroup("history: buffer with text suppresses recall on first ↑") {
    let mock = MockBridge()
    mock.stubRecentQueries = ["prior"]
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    vm.setActiveBuffer("typing")
    vm.results = [Candidate(id: "a", title: "A", subtitle: "", iconKind: 0, iconValue: "", kind: 0, score: 0, actions: [])]
    vm.selectedIndex = 0
    vm.handleUpKey()
    expect(vm.activeBuffer == "typing", "buffer unchanged - ↑ moved selection instead")
}

runGroup("history: empty history leaves ↑ as selection mover") {
    let mock = MockBridge()
    mock.stubRecentQueries = []
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    vm.results = [
        Candidate(id: "a", title: "A", subtitle: "", iconKind: 0, iconValue: "", kind: 0, score: 0, actions: []),
        Candidate(id: "b", title: "B", subtitle: "", iconKind: 0, iconValue: "", kind: 0, score: 0, actions: []),
    ]
    vm.selectedIndex = 1
    vm.handleUpKey()
    expect(vm.activeBuffer == "", "buffer still empty")
    expect(vm.selectedIndex == 0, "selection moved up instead of no-op")
}

runGroup("history: discovery rows on empty input let ↑ walk dropdown, not recall") {
    // REGRESSION (2026-04-28): with empty-state discovery rows
    // showing, pressing Up snapped input to most recent
    // history entry instead of walking visible list.
    // Eligibility now also checks `results.isEmpty`, so when
    // theres a list to navigate dropdown wins; recall
    // only fires when user has dismissed discovery and
    // list is genuinely empty
    let mock = MockBridge()
    mock.stubRecentQueries = ["last query", "older"]
    mock.queryResponse = [
        Candidate(id: "discovery::100 usd in eur", title: "100 usd in eur",
                  subtitle: "", iconKind: 0, iconValue: "", kind: 0, score: 0, actions: []),
        Candidate(id: "discovery::ai ", title: "ai",
                  subtitle: "", iconKind: 0, iconValue: "", kind: 0, score: 0, actions: []),
        Candidate(id: "discovery::__dismiss", title: "Don't show",
                  subtitle: "", iconKind: 0, iconValue: "", kind: 0, score: 0, actions: []),
    ]
    let vm = GyorsViewModel(bridge: mock)
    vm.reset() // populates results from mock with empty pattern
    expect(vm.results.count == 3, "discovery rows loaded, got \(vm.results.count)")
    expect(vm.activeBuffer == "", "buffer still empty after reset")
    // Move down to mid-list, then press up - must walk, not recall
    vm.selectedIndex = 2
    vm.handleUpKey()
    expect(vm.activeBuffer == "",
        "history did NOT recall; buffer empty, got `\(vm.activeBuffer)`")
    expect(vm.selectedIndex == 1, "selection walked up to row 1, got \(vm.selectedIndex)")
}

runGroup("history: empty results + empty input falls through to recall") {
    // Mirror image of above: when discovery has been
    // dismissed (or otherwise produces no rows), Up on empty
    // input should still recall most recent query -
    // we dont want priority change to silently kill
    // shell-style history feature for power users
    let mock = MockBridge()
    mock.stubRecentQueries = ["last query"]
    mock.queryResponse = [] // discovery dismissed -> no rows
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    expect(vm.results.isEmpty, "no rows present")
    vm.handleUpKey()
    expect(vm.activeBuffer == "last query",
        "recall fired, got `\(vm.activeBuffer)`")
}

runGroup("history: ↓ walks dropdown when discovery rows present") {
    // Symmetric guarantee for Down - dropdown is selectable
    // both directions while results exist
    let mock = MockBridge()
    mock.queryResponse = [
        Candidate(id: "discovery::a", title: "A",
                  subtitle: "", iconKind: 0, iconValue: "", kind: 0, score: 0, actions: []),
        Candidate(id: "discovery::b", title: "B",
                  subtitle: "", iconKind: 0, iconValue: "", kind: 0, score: 0, actions: []),
    ]
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    expect(vm.selectedIndex == 0, "starts at top")
    vm.handleDownKey()
    expect(vm.selectedIndex == 1, "walked down, got \(vm.selectedIndex)")
}

runGroup("history: recordQuery fires on activate") {
    let mock = MockBridge()
    mock.nextEffect = nil
    mock.queryResponse = [
        Candidate(id: "note::foo", title: "foo", subtitle: "", iconKind: 0, iconValue: "", kind: 0, score: 0, actions: [])
    ]
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    vm.setActiveBuffer("note foo")
    _ = vm.activateSelected()
    expect(mock.recordedQueries == ["note foo"], "recordedQueries = \(mock.recordedQueries)")
}

// `!!` recall: ghost-text preview, Tab-expand, Enter-and-run
//
// Recall flow has three observable behaviours, each tested below:
//   1. Typing `!!` populates `autocompleteSuffix` with a marker-prefixed
//      payload showing last query (preview state, no activation).
//   2. Tab on `!!` replaces buffer with last query verbatim
//      (the marker is NOT left in buffer).
//   3. Enter on `!!` swaps buffer AND immediately activates the
//      top result (no second-Enter confirm).
// Old behaviour (Enter just expands, user confirms) regressed
// here intentionally - ghost preview makes second confirm
// redundant and double-Enter wastes a keystroke for common case

runGroup("bangbang: typing `!!` populates ghost-text preview with last query") {
    let mock = MockBridge()
    mock.stubRecentQueries = ["calc 2+2"]
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    vm.setActiveBuffer("!!")
    expect(
        vm.autocompleteSuffix == "\(GyorsViewModel.bangBangMarker)calc 2+2",
        "expected marker-prefixed preview, got \(vm.autocompleteSuffix)"
    )
}

runGroup("bangbang: surrounding whitespace still triggers preview") {
    // Real input fields sometimes leave trailing spaces; the
    // isBangBang check must trim before comparing. Tests both
    // leading and trailing spaces in one pass since the
    // implementation uses .trimmingCharacters(in: .whitespaces)
    let mock = MockBridge()
    mock.stubRecentQueries = ["safari"]
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    vm.setActiveBuffer("  !!  ")
    expect(
        vm.autocompleteSuffix == "\(GyorsViewModel.bangBangMarker)safari",
        "trimmed `!!` still triggers preview, got \(vm.autocompleteSuffix)"
    )
}

runGroup("bangbang: empty history yields no preview") {
    let mock = MockBridge()
    mock.stubRecentQueries = []
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    vm.setActiveBuffer("!!")
    expect(
        vm.autocompleteSuffix == "",
        "no last query -> no ghost, got \(vm.autocompleteSuffix)"
    )
}

runGroup("bangbang: non-`!!` input does NOT trigger preview") {
    // Strings that contain `!!` but aren't a lone `!!` (e.g. someone
    // typed `gh !!` or `note !!important`) must NOT hijack the
    // ghost-text path - the autoclose / orchestrator suffix should
    // win in those cases
    let mock = MockBridge()
    mock.stubRecentQueries = ["calc 2+2"]
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    for input in ["!!extra", "gh !!", "!", "!!!"] {
        vm.setActiveBuffer(input)
        expect(
            !vm.autocompleteSuffix.hasPrefix(GyorsViewModel.bangBangMarker),
            "`\(input)` should NOT trigger bangbang preview, got `\(vm.autocompleteSuffix)`"
        )
    }
}

runGroup("bangbang: Tab on `!!` replaces buffer with last query verbatim") {
    let mock = MockBridge()
    mock.stubRecentQueries = ["kill Chrome"]
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    vm.setActiveBuffer("!!")
    let accepted = vm.tryAutoComplete()
    expect(accepted, "Tab should consume the bangbang preview")
    expect(
        vm.activeBuffer == "kill Chrome",
        "buffer should be the recalled query alone, got `\(vm.activeBuffer)`"
    )
    expect(
        !vm.activeBuffer.contains(GyorsViewModel.bangBangMarker),
        "the visual marker must NOT leak into the buffer"
    )
    expect(
        vm.autocompleteSuffix == "",
        "suffix cleared after acceptance"
    )
}

runGroup("bangbang: Tab on `!!` with empty history is a graceful no-op") {
    let mock = MockBridge()
    mock.stubRecentQueries = []
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    vm.setActiveBuffer("!!")
    let accepted = vm.tryAutoComplete()
    expect(
        !accepted,
        "no last query -> Tab falls through (no preview to accept)"
    )
    expect(vm.activeBuffer == "!!", "buffer untouched, got `\(vm.activeBuffer)`")
}

runGroup("bangbang: Enter on `!!` swaps buffer AND activates top result") {
    let mock = MockBridge()
    mock.stubRecentQueries = ["kill Chrome"]
    // Seed response FFI returns when VM re-queries
    // after setInput("kill Chrome"). One actionable row so
    // activateSelected fires
    mock.queryResponse = [
        Candidate(
            id: "kill::Chrome",
            title: "Kill Chrome",
            subtitle: "",
            iconKind: 0,
            iconValue: "💀",
            kind: 0,
            score: 100,
            actions: [CandidateAction(id: "default", label: "Kill")]
        )
    ]
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    vm.setActiveBuffer("!!")
    _ = vm.handleEnterKey()
    expect(
        vm.activeBuffer == "kill Chrome",
        "buffer should reflect the recalled query, got `\(vm.activeBuffer)`"
    )
    expect(
        mock.activations.contains(where: { $0.id == "kill::Chrome" }),
        "Enter should have activated the top row, got \(mock.activations)"
    )
}

runGroup("bangbang: Enter on `!!` with empty history falls through to normal activate") {
    let mock = MockBridge()
    mock.stubRecentQueries = []
    mock.queryResponse = []
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    vm.setActiveBuffer("!!")
    let dismissed = vm.handleEnterKey()
    expect(!dismissed, "no results to activate -> stay open")
    expect(vm.activeBuffer == "!!", "buffer untouched, got `\(vm.activeBuffer)`")
    expect(mock.activations.isEmpty, "nothing should have been activated")
}

runGroup("bangbang: Enter on `!!` records the recalled query into history") {
    // After Enter, recalled query should land in
    // `recordedQueries` so a subsequent `!!` recalls IT (not the
    // original predecessor), matching shell-style histexpand
    let mock = MockBridge()
    mock.stubRecentQueries = ["safari"]
    mock.queryResponse = [
        Candidate(
            id: "websearch::safari",
            title: "Search safari",
            subtitle: "",
            iconKind: 0,
            iconValue: "🌐",
            kind: 0,
            score: 100,
            actions: [CandidateAction(id: "default", label: "Open")]
        )
    ]
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    vm.setActiveBuffer("!!")
    _ = vm.handleEnterKey()
    expect(
        mock.recordedQueries.contains("safari"),
        "the recalled query should land in history, got \(mock.recordedQueries)"
    )
}

runGroup("bangbang: Enter on `!!` is a no-op while a chain is committed") {
    // Chain pills represent a multi-stage pipeline; recalling the
    // whole last query mid-chain would scramble pills. The
    // existing guard rejects bangbang when chainCommits isn't
    // empty - regression-test that branch
    let mock = MockBridge()
    mock.stubRecentQueries = ["calc 2+2"]
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    vm.chainCommits = ["b64"]   // simulate one committed pill
    vm.setActiveBuffer("!!")
    _ = vm.handleEnterKey()
    expect(
        !mock.activations.contains(where: { $0.id.contains("calc") }),
        "bangbang must not fire inside a chain, got \(mock.activations)"
    )
}

runGroup("bangbang: helpers recognise variants + extract payload") {
    // Pin static helpers so any future refactor that touches
    // marker / trim rule fails loudly here rather than via
    // some downstream UX regression. `isBangBang` is purely about
    // trimmed buffer matching exactly `!!` - everything else
    // must fall through to normal autocomplete / activate
    // paths
    expect(GyorsViewModel.isBangBang("!!"), "exact `!!` matches")
    expect(GyorsViewModel.isBangBang("  !!  "), "wrapping whitespace trimmed")
    expect(GyorsViewModel.isBangBang("\t!!\n"), "tab/newline are whitespace")
    expect(GyorsViewModel.isBangBang("!! "), "trailing space trimmed")
    expect(GyorsViewModel.isBangBang(" !!"), "leading space trimmed")
    expect(!GyorsViewModel.isBangBang("!"), "single bang doesn't match")
    expect(!GyorsViewModel.isBangBang("!!!"), "triple bang isn't recall")
    expect(!GyorsViewModel.isBangBang("a!!"), "embedded `!!` not a recall")

    let payload = GyorsViewModel.bangBangPayload(
        "\(GyorsViewModel.bangBangMarker)calc 2+2"
    )
    expect(payload == "calc 2+2", "extracted payload, got \(String(describing: payload))")
    expect(
        GyorsViewModel.bangBangPayload("plain suffix without marker") == nil,
        "non-marker suffixes return nil"
    )
}

runGroup("bangbang: normal autocomplete suffix still works (regression)") {
    // Tab on a regular ghost suffix (orchestrator-returned, no
    // marker) must still append, not replace. Without this
    // assertion bangbang branch's `if let recalled = ...`
    // could be re-ordered above suffix-append branch and
    // break every other autocomplete
    let mock = MockBridge()
    mock.stubRecentQueries = ["something"]
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    vm.setActiveBuffer("{\"k\":")
    // Bypass FFI path: write suffix directly way a
    // real autoclose:: row would
    vm.autocompleteSuffix = "}"
    let accepted = vm.tryAutoComplete()
    expect(accepted, "Tab should accept regular autocomplete")
    expect(
        vm.activeBuffer == "{\"k\":}",
        "regular suffix appends, got `\(vm.activeBuffer)`"
    )
}

runGroup("history: snapshot refresh on reset") {
    let mock = MockBridge()
    mock.stubRecentQueries = ["first-session"]
    let vm = GyorsViewModel(bridge: mock)
    vm.reset()
    // Simulate a new query landing between panel closes
    mock.stubRecentQueries = ["second-session", "first-session"]
    vm.reset()
    vm.handleUpKey()
    expect(vm.activeBuffer == "second-session", "refreshed snapshot, got \(vm.activeBuffer)")
}

}
