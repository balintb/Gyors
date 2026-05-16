import Foundation

func runSelectionTests() {

// Helper: build a list of N candidates with stable, distinct ids.
// Used by the regression suite below so we can drive
// `moveSelection` against a known-shape result set without
// standing up the full bridge
func makeResults(_ n: Int) -> [Candidate] {
    (0..<n).map { i in
        Candidate(
            id: "row-\(i)",
            title: "row \(i)",
            subtitle: "",
            iconKind: 0,
            iconValue: "",
            kind: 0,
            score: 0,
            actions: []
        )
    }
}

runGroup("selection: down-arrow walks one step at a time, never wraps") {
    // Regression for the "scroll mid-list, hit Down, snap to top"
    // bug. The off-by-one wasn't in `moveSelection` itself -
    // it was in ContentView's per-row index lookup, but the
    // VM-level invariants we pin here are the contract any
    // future view code must honour: Down bumps by exactly one,
    // and stops at `count - 1`
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = makeResults(11) // 11 themes-like list
    vm.selectedIndex = 0
    for expected in 1...10 {
        vm.handleDownKey()
        expect(
            vm.selectedIndex == expected,
            "after \(expected) down presses, selectedIndex=\(vm.selectedIndex), expected \(expected)"
        )
    }
    // Already at bottom - one more Down stays put, never
    // wraps to 0
    vm.handleDownKey()
    expect(vm.selectedIndex == 10, "down past bottom must stay at last index, got \(vm.selectedIndex)")
}

runGroup("selection: up-arrow walks one step at a time, never wraps") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = makeResults(11)
    vm.selectedIndex = 10
    for expected in (0...9).reversed() {
        vm.handleUpKey()
        expect(
            vm.selectedIndex == expected,
            "selectedIndex=\(vm.selectedIndex), expected \(expected)"
        )
    }
    // Already at top - one more Up stays put
    vm.handleUpKey()
    expect(vm.selectedIndex == 0, "up past top must clamp to 0, got \(vm.selectedIndex)")
}

runGroup("selection: down from arbitrary mid-list position never snaps to 0") {
    // Walk through every starting position in a 12-row list,
    // press down once, assert result is exactly +1 (or
    // capped at `count - 1`). This is the precise shape of
    // the user-reported bug - pressing Down at any mid-list
    // index could land on 0
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = makeResults(12)
    for start in 0..<12 {
        vm.selectedIndex = start
        vm.handleDownKey()
        let expected = min(start + 1, 11)
        expect(
            vm.selectedIndex == expected,
            "from \(start), ↓ landed at \(vm.selectedIndex), expected \(expected)"
        )
    }
}

runGroup("selection: arrow keys are no-op on empty results") {
    // An empty result list shouldn't crash, panic,
    // or somehow set selectedIndex to a non-zero value
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = []
    vm.selectedIndex = 0
    vm.handleDownKey()
    expect(vm.selectedIndex == 0, "down on empty stays 0")
    vm.handleUpKey()
    expect(vm.selectedIndex == 0, "up on empty stays 0")
}

runGroup("selection: results-set replacement resets to 0 (intentional)") {
    // Sanity-check existing reset-on-results behaviour:
    // When a fresh `update(query:)` lands a new result list,
    // selectedIndex resets to 0. This isn't the bug - it's
    // the intended contract - but pinning it here means a
    // future "preserve selection across queries" change can't
    // silently regress rest of the keyboard model
    let mock = MockBridge()
    mock.queryResponse = makeResults(5)
    let vm = GyorsViewModel(bridge: mock)
    vm.update(query: "anything")
    vm.selectedIndex = 3
    mock.queryResponse = makeResults(7)
    vm.update(query: "anything else")
    expect(vm.selectedIndex == 0, "fresh results → selectedIndex back to 0")
}

}
