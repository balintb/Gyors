import Foundation

func runTabAutocompleteTests() {

runGroup("tryAutoComplete fills input for hint row") {
    let mock = MockBridge()
    mock.nextEffect = .setInput("md5 ")
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [makeCand(id: "hint::md5", title: "md5 <text>")]
    vm.selectedIndex = 0
    let ok = vm.tryAutoComplete()
    expect(ok, "returned true")
    expect(vm.query == "md5 ", "query prefilled")
    expect(mock.activations.count == 1, "activated exactly once")
}

runGroup("tryAutoComplete ignores rows whose effect isn't SetInput") {
    // New contract: Tab activates and checks the effect, rather
    // than gating on an id prefix. Apps return `.openPath`, so
    // Tab still fires nothing user-visible - query stays
    // put and no side effect runs
    let mock = MockBridge()
    mock.nextEffect = .openPath("/Applications/Safari.app")
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [makeCand(id: "apps::/Applications/Safari.app", title: "Safari")]
    vm.selectedIndex = 0
    let ok = vm.tryAutoComplete()
    expect(!ok, "returned false")
    expect(vm.query == "", "query unchanged")
}

runGroup("splitGhost extracts autoclose suffix and removes the row") {
    // Inline-autocomplete contract: the autoclose candidate
    // becomes a ghost-text tail and never reaches the user's
    // results list
    let autoclose = makeCand(
        id: #"autoclose::json {"a":1}"#,
        title: #"json {"a":1}"#
    )
    let other = makeCand(id: "json::pretty::x", title: "Pretty JSON")
    let (suffix, filtered) = GyorsViewModel.splitGhost(
        results: [autoclose, other],
        query: #"json {"a":1"#
    )
    expect(suffix == "}", "suffix extracted (got \(suffix))")
    expect(filtered.count == 1, "autoclose row filtered out")
    expect(filtered.first?.id == "json::pretty::x", "other row preserved")
}

runGroup("REGRESSION: splitGhost preserves # note candidates") {
    // The autoclose ghost-text path uses an id prefix filter.
    // Regression guard: typing `#hello` must still yield the
    // note openOrCreate row in the filtered list - not drop it
    let openOrCreate = makeCand(
        id: "note::openOrCreate::hello",
        title: "Create note hello"
    )
    let folder = makeCand(id: "note::folder::work", title: "work")
    let (suffix, filtered) = GyorsViewModel.splitGhost(
        results: [openOrCreate, folder],
        query: "#hello"
    )
    expect(suffix == "", "no ghost for # queries")
    expect(filtered.count == 2, "note rows must survive the filter")
    expect(
        filtered.contains(where: { $0.id == "note::openOrCreate::hello" }),
        "openOrCreate row is present"
    )
}

runGroup("splitGhost drops autoclose row even when prefix doesn't match") {
    // Safety net: if title/id somehow diverge from the
    // query (stale result, rare race), we still drop row
    // so it never leaks into the list as a half-useful entry
    let autoclose = makeCand(
        id: "autoclose::stale stuff",
        title: "stale stuff"
    )
    let (suffix, filtered) = GyorsViewModel.splitGhost(
        results: [autoclose],
        query: "fresh query"
    )
    expect(suffix == "", "no suffix on mismatch")
    expect(filtered.isEmpty, "row still dropped")
}

runGroup("tryAutoComplete commits ghost suffix to query") {
    // Tab appends ghost tail without any FFI roundtrip;
    // suffix was computed when results arrived. Drive via
    // setActiveBuffer so the VM's `syncQuery` sees active
    // content and produces the correct flat form
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.setActiveBuffer(#"json {"a":1"#)
    vm.autocompleteSuffix = "}"
    let ok = vm.tryAutoComplete()
    expect(ok, "tab fired")
    expect(vm.query == #"json {"a":1}"#, "query became balanced form, got \(vm.query)")
}

runGroup("tryAutoComplete noop when no results") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = []
    let ok = vm.tryAutoComplete()
    expect(!ok, "returned false")
    expect(mock.activations.isEmpty, "no activation")
}

runGroup("tryAutoComplete noop in actions mode") {
    let mock = MockBridge()
    mock.nextEffect = .setInput("md5 ")
    let vm = GyorsViewModel(bridge: mock)
    vm.viewMode = .actions(makeCand(id: "hint::md5", title: "md5 <text>"))
    let ok = vm.tryAutoComplete()
    expect(!ok, "returned false")
    expect(mock.activations.isEmpty, "no activation in actions mode")
}

runGroup("tryAutoComplete noop if hint returns non-setInput effect") {
    // a hint that somehow yields another effect shouldn't
    // accidentally execute via Tab
    let mock = MockBridge()
    mock.nextEffect = .openPath("/oops")
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [makeCand(id: "hint::weird", title: "weird")]
    vm.selectedIndex = 0
    let ok = vm.tryAutoComplete()
    expect(!ok, "returned false")
    expect(vm.query == "", "query unchanged")
}

}
