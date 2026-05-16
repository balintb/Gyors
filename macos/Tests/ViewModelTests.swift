import Foundation

func runViewModelTests() {
// `makeCand` is a shared test helper - see TestMocks.swift

runGroup("ViewModel empty query resets") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.query = "x"
    vm.results = [makeCand(id: "a", title: "A")]
    vm.update(query: "")
    expect(vm.results.isEmpty, "results cleared")
    expect(vm.isSearching == false, "isSearching cleared")
}

runGroup("ViewModel whitespace-only query resets") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [makeCand(id: "a", title: "A")]
    vm.update(query: "   ")
    expect(vm.results.isEmpty, "whitespace-only → cleared")
}

runGroup("ViewModel fast path replaces results synchronously") {
    let mock = MockBridge()
    let freshCalc = makeCand(id: "calc::35", title: "35", subtitle: "12+23",
                             kind: CandidateKindCode.calculation.rawValue)
    mock.queryResponse = [freshCalc]
    let vm = GyorsViewModel(bridge: mock)
    vm.query = "12+23"
    // Simulate state leftover from a previous query
    vm.results = [
        makeCand(id: "calc::245", title: "245", subtitle: "12+233",
                 kind: CandidateKindCode.calculation.rawValue),
    ]
    vm.update(query: "12+23")
    // No debounce on fast path -> results immediately reflect the fresh query
    expect(vm.results.count == 1, "results replaced with fresh")
    expect(vm.results.first?.title == "35", "new calc value")
    expect(vm.isSearching == false, "no spinner on fast path")
}

runGroup("ViewModel fast path clears when bridge returns empty") {
    let mock = MockBridge()
    mock.queryResponse = []
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [
        makeCand(id: "calc::245", title: "245", subtitle: "12+233",
                 kind: CandidateKindCode.calculation.rawValue),
    ]
    vm.update(query: "nomatch")
    expect(vm.results.isEmpty, "results cleared")
}

runGroup("ViewModel fast path passes query through to bridge") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.update(query: "safari")
    expect(mock.lastPattern == "safari", "bridge received the query")
}

runGroup("ViewModel files path drops stale calc synchronously") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.query = "'12+23"
    vm.results = [
        makeCand(id: "calc::245", title: "245", subtitle: "12+233",
                 kind: CandidateKindCode.calculation.rawValue),
        makeCand(id: "apps::/Safari.app", title: "Safari", kind: 0),
    ]
    vm.update(query: "'12+23")
    // Sync filter runs first on files path; async query hasn't fired yet
    expect(vm.results.count == 1, "stale calc dropped, app preserved")
    expect(vm.results.first?.id == "apps::/Safari.app", "app preserved")
}

runGroup("ViewModel files path keeps matching calc through debounce") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.query = "'12+23"
    vm.results = [
        makeCand(id: "calc::35", title: "35", subtitle: "12+23",
                 kind: CandidateKindCode.calculation.rawValue),
    ]
    vm.update(query: "'12+23")
    expect(vm.results.count == 1, "matching calc preserved")
}

runGroup("ViewModel isSearching set in files mode") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.update(query: "'safari")
    expect(vm.isSearching == true, "files-mode pattern sets spinner flag")
}

runGroup("ViewModel isSearching not set in default mode") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.update(query: "safari")
    expect(vm.isSearching == false, "default mode = no spinner")
}

runGroup("ViewModel bare apostrophe does not set isSearching") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.update(query: "'")
    expect(vm.isSearching == false, "no effective pattern → no spinner")
}

runGroup("ViewModel moveSelection clamps to results bounds") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [
        makeCand(id: "a", title: "A"),
        makeCand(id: "b", title: "B"),
        makeCand(id: "c", title: "C"),
    ]
    vm.selectedIndex = 0
    vm.moveSelection(by: -1)
    expect(vm.selectedIndex == 0, "can't go below 0")
    vm.moveSelection(by: 10)
    expect(vm.selectedIndex == 2, "clamps to last index")
    vm.moveSelection(by: -1)
    expect(vm.selectedIndex == 1, "decrement works")
}

runGroup("ViewModel moveSelection is noop with no results") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.selectedIndex = 0
    vm.moveSelection(by: 1)
    expect(vm.selectedIndex == 0, "selectedIndex unchanged")
}

runGroup("ViewModel reset clears everything") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.query = "x"
    vm.results = [makeCand(id: "a", title: "A")]
    vm.selectedIndex = 5
    vm.isSearching = true
    vm.viewMode = .actions(makeCand(id: "z", title: "Z"))
    vm.actionIndex = 2
    vm.reset()
    expect(vm.query.isEmpty, "query cleared")
    expect(vm.results.isEmpty, "results cleared")
    expect(vm.selectedIndex == 0, "selection reset")
    expect(vm.isSearching == false, "searching cleared")
    expect(vm.viewMode == .main, "viewMode back to main")
    expect(vm.actionIndex == 0, "action index reset")
}

}
