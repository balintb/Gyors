import Foundation

/// AI router - ViewModel-side state (palette row injection, preview shape)
func runAiRouterVmTests() {
runGroup("VM ROUTER: resultsAreEmptyExceptPalette detects single palette row") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [Candidate(
        id: "ai_palette::100 dollars",
        title: "Ask AI: 100 dollars",
        subtitle: "", iconKind: 0, iconValue: "",
        kind: 0, score: 0, actions: []
    )]
    expect(vm.resultsAreEmptyExceptPalette(),
        "single palette row → true")
}

runGroup("VM ROUTER: resultsAreEmptyExceptPalette rejects palette + real row") {
    // Two rows means a real provider matched - we dont
    // want to overshadow real hits with router speculation
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [
        Candidate(id: "ai_palette::x", title: "x", subtitle: "",
            iconKind: 0, iconValue: "", kind: 0, score: 0, actions: []),
        Candidate(id: "calc::35", title: "35", subtitle: "",
            iconKind: 0, iconValue: "", kind: 0, score: 0, actions: []),
    ]
    expect(!vm.resultsAreEmptyExceptPalette(),
        "two rows incl. palette → false")
}

runGroup("VM ROUTER: resultsAreEmptyExceptPalette rejects empty list") {
    // Genuinely zero rows means we're between query updates;
    // the router has nothing to fire about either
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = []
    expect(!vm.resultsAreEmptyExceptPalette(),
        "empty list → false")
}

runGroup("VM ROUTER: resultsAreEmptyExceptPalette rejects single non-palette row") {
    // The palette is the SIGNAL that nothing else matched;
    // a single row from any other provider means real
    // routing succeeded
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [Candidate(
        id: "calc::35", title: "35", subtitle: "",
        iconKind: 0, iconValue: "", kind: 0, score: 0, actions: []
    )]
    expect(!vm.resultsAreEmptyExceptPalette(),
        "single calc row → false")
}

runGroup("VM ROUTER: injectRouterPreview produces correctly-shaped row") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [Candidate(
        id: "ai_palette::100 dollars in euros",
        title: "Ask AI: 100 dollars in euros",
        subtitle: "Apple Intelligence · ⌘↵",
        iconKind: 1, iconValue: "sparkles",
        kind: 0, score: 0, actions: []
    )]
    vm.injectRouterPreview(keyword: "100 USD in EUR",
        toolName: "currency__convert")
    expect(vm.results.count == 1, "results replaced (count=1)")
    let row = vm.results[0]
    expect(row.id == "ai_router::100 USD in EUR",
        "id encodes keyword form")
    expect(row.title == "100 USD in EUR",
        "title is the keyword form, got `\(row.title)`")
    expect(row.subtitle.contains("currency"),
        "subtitle names the provider, got `\(row.subtitle)`")
    // Activation hint is `Enter` (Return) since this row is
    // selected at index 0 and Enter activates it. The
    // cmd+return shortcut belongs to the AI palette row, not the
    // router preview - keep them visually distinct
    expect(row.subtitle.contains("↵"),
        "subtitle hints at the activation key, got `\(row.subtitle)`")
    expect(row.iconKind == 1, "SF Symbol icon kind")
    expect(row.iconValue == "wand.and.stars",
        "wand-and-stars glyph distinguishes router vs palette")
    expect(row.actions.first?.id == "default",
        "primary action present, got \(row.actions)")
    expect(vm.selectedIndex == 0, "selection on the new row")
}

runGroup("VM ROUTER: injectRouterPreview replaces multiple existing rows") {
    // Router preview is meant to be only row when
    // it shows - it OWNS dropdown so user has one
    // unambiguous choice to confirm. Verify replacement
    // semantics regardless of pre-existing list shape
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [
        Candidate(id: "a", title: "A", subtitle: "",
            iconKind: 0, iconValue: "", kind: 0, score: 0, actions: []),
        Candidate(id: "b", title: "B", subtitle: "",
            iconKind: 0, iconValue: "", kind: 0, score: 0, actions: []),
        Candidate(id: "c", title: "C", subtitle: "",
            iconKind: 0, iconValue: "", kind: 0, score: 0, actions: []),
    ]
    vm.injectRouterPreview(keyword: "anything",
        toolName: "calc__evaluate")
    expect(vm.results.count == 1, "replaced not appended")
    expect(vm.results[0].id == "ai_router::anything",
        "the only row is the router preview")
}

runGroup("VM ROUTER: prettyToolName extraction") {
    // Strip `<provider>__<verb>` to provider only. The
    // verb is implicit in the rendered keyword title, so
    // the subtitle stays compact
    expect(GyorsViewModel.prettyToolName("currency__convert") == "currency",
        "currency__convert → currency")
    expect(GyorsViewModel.prettyToolName("calc__evaluate") == "calc",
        "calc__evaluate → calc")
    expect(GyorsViewModel.prettyToolName("regex__test") == "regex",
        "regex__test → regex")
    expect(GyorsViewModel.prettyToolName("notes__find") == "notes",
        "notes__find → notes (forward-compat)")
    expect(GyorsViewModel.prettyToolName("nounderscore") == "nounderscore",
        "names without `__` pass through unchanged")
    expect(GyorsViewModel.prettyToolName("") == "",
        "empty string survives")
    expect(GyorsViewModel.prettyToolName("a__b__c") == "a",
        "first occurrence wins on multi-`__`")
}
}
