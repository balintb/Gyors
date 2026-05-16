import Foundation

func runQrFlowTests() {

runGroup("activateActionAtIndex fires nth action in actions mode") {
    let mock = MockBridge()
    mock.nextEffect = .copyToClipboard("second")
    let vm = GyorsViewModel(bridge: mock)
    let cand = Candidate(
        id: "x::y",
        title: "T",
        subtitle: "",
        iconKind: 0,
        iconValue: "",
        kind: 6,
        score: 0,
        actions: [
            CandidateAction(id: "default", label: "First"),
            CandidateAction(id: "second", label: "Second"),
            CandidateAction(id: "third", label: "Third"),
        ]
    )
    vm.viewMode = .actions(cand)
    vm.actionIndex = 0
    let dismiss = vm.activateActionAtIndex(1)
    expect(dismiss, "copy dismisses")
    expect(mock.activations.first?.action == "second", "fired second action")
}

runGroup("activateActionAtIndex is noop outside actions mode") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.viewMode = .main
    expect(!vm.activateActionAtIndex(0), "no-op outside .actions")
}

runGroup("activateActionAtIndex clamps out-of-range safely") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    let cand = Candidate(
        id: "x::y",
        title: "T",
        subtitle: "",
        iconKind: 0,
        iconValue: "",
        kind: 6,
        score: 0,
        actions: [CandidateAction(id: "default", label: "Only")]
    )
    vm.viewMode = .actions(cand)
    expect(!vm.activateActionAtIndex(5), "beyond actions.count returns false")
    expect(mock.activations.isEmpty, "no action fired")
}

runGroup("chainCompletionKeyword extracts last arrow segment") {
    // Title format from Rust side is:
    //   "<base> - <stage1> -> <stage2> -> ... -> <last>"
    // The last `->`-delimited stage is the completion target
    expect(
        GyorsViewModel.chainCompletionKeyword(from: "Hello · md5") == "md5",
        "single-stage title → last segment")
    expect(
        GyorsViewModel.chainCompletionKeyword(from: "Hello · upper → copy") == "copy",
        "multi-stage title → last segment")
    expect(
        GyorsViewModel.chainCompletionKeyword(from: "Hello · upper → trim → sha256") == "sha256",
        "three-stage title → last segment")
    expect(
        GyorsViewModel.chainCompletionKeyword(from: "No dot here") == nil,
        "malformed title → nil")
    expect(
        GyorsViewModel.chainCompletionKeyword(from: "Hello · ") == nil,
        "empty stages portion → nil")
}

runGroup("Tab on a pipeline row replaces active with canonical keyword") {
    // Mirrors the user's scenario: they typed `note helo | md`,
    // selected the `Hello - md5` pipeline row, pressed Tab.
    // The active buffer becomes `md5`, pill stays intact
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.setActiveBuffer("note helo")
    _ = vm.commitActiveAsPill()
    vm.setActiveBuffer("md")
    // Simulate orchestrator's pipeline row for `md5`
    vm.results = [
        Candidate(
            id: "pipeline::anything-base64",
            title: "helo · md5",
            subtitle: "MD5 hash · transform",
            iconKind: 0,
            iconValue: "arrow.right.arrow.left.square",
            kind: 7,
            score: 0,
            actions: [CandidateAction(id: "default", label: "Run pipeline")]
        )
    ]
    vm.selectedIndex = 0
    let ok = vm.tryAutoComplete()
    expect(ok, "tab consumed")
    expect(vm.chainCommits == ["note helo"],
        "pill preserved: \(vm.chainCommits)")
    expect(vm.activeBuffer == "md5",
        "active completed to canonical, got `\(vm.activeBuffer)`")
}

runGroup("Tab on a chain row also completes to the action label") {
    // Same extraction works for chain:: rows (classic base
    // actions like `Hello - Preview`). Action labels are
    // Capitalized; chain parsing is case-insensitive so
    // this still does the right thing downstream
    let vm = GyorsViewModel(bridge: MockBridge())
    vm.setActiveBuffer("note helo")
    _ = vm.commitActiveAsPill()
    vm.setActiveBuffer("pre")
    vm.results = [
        Candidate(
            id: "chain::preview::whatever-base64",
            title: "helo · Preview",
            subtitle: "↵ preview on helo",
            iconKind: 0,
            iconValue: "doc.text",
            kind: 7,
            score: 0,
            actions: [CandidateAction(id: "default", label: "Preview")]
        )
    ]
    vm.selectedIndex = 0
    let ok = vm.tryAutoComplete()
    expect(ok, "tab consumed on chain row")
    expect(vm.chainCommits == ["note helo"], "pill preserved")
    expect(vm.activeBuffer == "Preview",
        "active = action label, got `\(vm.activeBuffer)`")
}

runGroup("Tab on a note row autocompletes `note <title>`") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.query = "note hel"
    vm.results = [
        Candidate(
            id: "note::/tmp/hello-world.md",
            title: "Hello World",
            subtitle: "",
            iconKind: 1,
            iconValue: "doc.text",
            kind: 6,
            score: 0,
            actions: [CandidateAction(id: "default", label: "Edit")]
        )
    ]
    vm.selectedIndex = 0
    let ok = vm.tryAutoComplete()
    expect(ok, "tab consumed")
    // Trailing space lets the user immediately type `|` to
    // start a chain without first pressing space themselves
    expect(vm.query == "note Hello World ",
           "query filled with title + trailing space, got `\(vm.query)`")
}

runGroup("Tab on a note row preserves the `notes ` keyword form") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.query = "notes spr"
    vm.results = [
        Candidate(
            id: "note::/tmp/sprint.md",
            title: "Sprint Planning",
            subtitle: "",
            iconKind: 1,
            iconValue: "doc.text",
            kind: 6,
            score: 0,
            actions: [CandidateAction(id: "default", label: "Edit")]
        )
    ]
    vm.selectedIndex = 0
    _ = vm.tryAutoComplete()
    expect(vm.query == "notes Sprint Planning ",
           "preserves `notes ` + trailing space, got `\(vm.query)`")
}

runGroup("Tab on a note row with `notes all` keeps the `all` form") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.query = "notes all hel"
    vm.results = [
        Candidate(
            id: "note::/tmp/hello.md",
            title: "Hello",
            subtitle: "",
            iconKind: 1,
            iconValue: "",
            kind: 6,
            score: 0,
            actions: [CandidateAction(id: "default", label: "Edit")]
        )
    ]
    vm.selectedIndex = 0
    _ = vm.tryAutoComplete()
    expect(vm.query == "notes all Hello ",
           "preserves `notes all ` + trailing space, got `\(vm.query)`")
}

runGroup("enterActionsMode: 3+ actions opens menu even when preview exists") {
    // Preview short-circuit should NOT fire for notes (8
    // actions) / clipboard URLs (3 actions) - the user needs
    // access to the other actions too. Only the 2-action
    // "Copy + Preview" case skips menu
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [
        Candidate(
            id: "note::/tmp/x.md",
            title: "x",
            subtitle: "",
            iconKind: 1,
            iconValue: "doc.text",
            kind: 6,
            score: 0,
            actions: [
                CandidateAction(id: "default", label: "Edit"),
                CandidateAction(id: "preview", label: "Preview"),
                CandidateAction(id: "reveal", label: "Reveal"),
                CandidateAction(id: "trash", label: "Trash"),
            ]
        )
    ]
    vm.selectedIndex = 0
    _ = vm.enterActionsMode()
    if case .actions = vm.viewMode {
        expect(true, "opened actions menu, not preview")
    } else {
        expect(false, "expected .actions, got \(vm.viewMode)")
    }
    expect(mock.activations.isEmpty, "preview not auto-fired")
}

runGroup("REGRESSION: actions with preview id jumps straight to preview") {
    // Bug: -> on a QR candidate used to open an actions list, and
    // selecting "Show QR" there did nothing because activateAction
    // unconditionally reset viewMode back to main. Now -> invokes
    // the "preview" action directly
    let mock = MockBridge()
    mock.nextEffect = .showImagePng("ZmFrZS1wbmc=") // "fake-png"
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [
        Candidate(
            id: "qr::hi",
            title: "QR: hi",
            subtitle: "↵ copy · → show inline",
            iconKind: 1,
            iconValue: "qrcode",
            kind: 6,
            score: 0,
            actions: [
                CandidateAction(id: "default", label: "Copy QR image"),
                CandidateAction(id: "preview", label: "Show QR"),
            ]
        )
    ]
    vm.selectedIndex = 0
    _ = vm.enterActionsMode()
    // Should skip actions view, jump straight to imagePreview
    if case .imagePreview(let b64, _) = vm.viewMode {
        expect(b64 == "ZmFrZS1wbmc=", "preview b64 carried through")
    } else {
        expect(false, "expected imagePreview, got \(vm.viewMode)")
    }
    expect(mock.activations.first?.action == "preview", "fired preview action")
}

runGroup("REGRESSION: activateAction preserves non-.actions mode transitions") {
    // The old bug: `activateAction` always reset to `.main` even
    // if `dispatchEffect` had transitioned somewhere else (editor
    // / imagePreview). Test covers the case generically
    let mock = MockBridge()
    mock.nextEffect = .showImagePng("payload")
    let vm = GyorsViewModel(bridge: mock)
    let cand = Candidate(
        id: "qr::x",
        title: "QR",
        subtitle: "",
        iconKind: 1,
        iconValue: "qrcode",
        kind: 6,
        score: 0,
        actions: [CandidateAction(id: "preview", label: "Show")]
    )
    vm.viewMode = .actions(cand)
    vm.actionIndex = 0
    _ = vm.activateAction()
    if case .imagePreview = vm.viewMode {
        expect(true, "stayed in preview")
    } else {
        expect(false, "activateAction clobbered viewMode")
    }
}

runGroup("handleLeftKey exits imagePreview back to main") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.viewMode = .imagePreview(base64: "x", label: "QR")
    let handled = vm.handleLeftKey()
    expect(handled, "handled")
    expect(vm.viewMode == .main, "back to main")
}

runGroup("handleLeftKey exits actions back to main") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.viewMode = .actions(
        Candidate(id: "x", title: "X", subtitle: "", iconKind: 0, iconValue: "",
                  kind: 6, score: 0, actions: [])
    )
    expect(vm.handleLeftKey(), "handled")
    expect(vm.viewMode == .main, "back to main")
}

runGroup("handleLeftKey noop in main mode") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.viewMode = .main
    expect(!vm.handleLeftKey(), "not handled")
}

runGroup("CURSOR FIX: ← in main returns false so KeyCatcher passes the key through") {
    // REGRESSION (2026-04-28): KeyCatcher used to swallow <- /->
    // unconditionally, even when the VM's handler said "I
    // didn't act on it." That blocked the focused NSTextField
    // from moving its caret - users couldn't reposition the
    // cursor in input box. Pin the contract loudly: in
    // main mode (the only mode where input has focus), the
    // handlers MUST report false so the catcher falls through
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.viewMode = .main
    // No selection at all -> enterActionsMode can't open a
    // menu; must defer to the TextField
    vm.results = []
    expect(!vm.handleLeftKey(),
        "← in main: handler must report false for caret-left to pass through")
    expect(!vm.enterActionsMode(),
        "→ in main with no rows: handler must report false")
}

}
