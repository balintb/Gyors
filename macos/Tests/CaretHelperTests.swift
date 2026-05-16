import Foundation

/// Caret-position helper - at-end detection used by -> cursor-vs-list routing
func runCaretHelperTests() {
runGroup("CARET HELPER: caret at end (length 0, location == textLength)") {
    // Pure boundary check: no selection, caret parked at the
    // far right of the text. This is the ONLY shape where ->
    // should be reinterpreted as "open actions menu."
    expect(
        CaretPosition.caretIsAtEnd(
            textLength: 5,
            selectedRange: NSRange(location: 5, length: 0)
        ),
        "caret at the end → at-end"
    )
}

runGroup("CARET HELPER: caret mid-text") {
    // The bug the user hit: user is at "go|ogle" (caret at 2),
    // presses ->, expects the caret to move to "goo|gle". With
    // this guard the keyCatcher passes the event through to
    // field editor instead of opening actions menu
    expect(
        !CaretPosition.caretIsAtEnd(
            textLength: 6,
            selectedRange: NSRange(location: 2, length: 0)
        ),
        "caret in middle → NOT at-end"
    )
}

runGroup("CARET HELPER: caret at very start of text") {
    expect(
        !CaretPosition.caretIsAtEnd(
            textLength: 5,
            selectedRange: NSRange(location: 0, length: 0)
        ),
        "caret at index 0 of non-empty text → NOT at-end"
    )
}

runGroup("CARET HELPER: empty text counts as at-end") {
    // No content to walk through, so -> semantically means
    // "open actions menu" same way it always did. (If
    // there are no results either, enterActionsMode itself
    // returns false; that's a separate guard.)
    expect(
        CaretPosition.caretIsAtEnd(
            textLength: 0,
            selectedRange: NSRange(location: 0, length: 0)
        ),
        "empty text - caret trivially at-end"
    )
}

runGroup("CARET HELPER: any selection is treated as NOT at-end") {
    // Standard NSTextField behaviour for -> with an active
    // selection is to collapse selection to its right
    // edge. We honour that - even when selection happens
    // to *end* at textLength, -> must collapse selection
    // before any menu-opening could be considered
    expect(
        !CaretPosition.caretIsAtEnd(
            textLength: 5,
            selectedRange: NSRange(location: 0, length: 5)
        ),
        "full-text selection → NOT at-end (→ collapses)"
    )
    expect(
        !CaretPosition.caretIsAtEnd(
            textLength: 5,
            selectedRange: NSRange(location: 3, length: 2)
        ),
        "partial selection ending at length → NOT at-end"
    )
    expect(
        !CaretPosition.caretIsAtEnd(
            textLength: 5,
            selectedRange: NSRange(location: 1, length: 2)
        ),
        "mid-text selection → NOT at-end"
    )
}

runGroup("CURSOR FIX: → with multi-action candidate opens menu (handler claims the key)") {
    // Counterpart: when there ARE multiple actions, -> opens
    // actions menu and the handler returns true - the
    // KeyCatcher should swallow the key in this case so it
    // doesn't double-fire as a caret move
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [
        Candidate(
            id: "x", title: "x", subtitle: "", iconKind: 0, iconValue: "",
            kind: 0, score: 0,
            actions: [
                CandidateAction(id: "default", label: "Open"),
                CandidateAction(id: "reveal", label: "Reveal in Finder"),
            ]
        )
    ]
    vm.selectedIndex = 0
    expect(vm.enterActionsMode(),
        "→ on multi-action row: handler must claim the key")
    if case .actions = vm.viewMode {} else {
        expect(false, "viewMode should be .actions, got \(vm.viewMode)")
    }
}

runGroup("REGRESSION: ESC in imagePreview returns to main") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.viewMode = .imagePreview(base64: "x", label: "QR")
    expect(vm.handleEscape(), "handled")
    expect(vm.viewMode == .main, "back to main")
}

runGroup("handleLeftKey exits textPreview back to main") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.viewMode = .textPreview(
        text: "hi", label: "JSON → YAML", language: "yaml", editablePath: nil
    )
    expect(vm.handleLeftKey(), "handled")
    expect(vm.viewMode == .main, "back to main")
}

runGroup("ESC in textPreview returns to main") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.viewMode = .textPreview(text: "hi", label: "X", language: nil, editablePath: nil)
    expect(vm.handleEscape(), "handled")
    expect(vm.viewMode == .main, "back to main")
}

runGroup("ShowText effect routes into textPreview with language") {
    let mock = MockBridge()
    mock.nextEffect = .showText(
        text: "a: 1\nb: 2",
        label: "JSON → YAML",
        language: "yaml",
        editablePath: nil
    )
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [
        Candidate(
            id: "fmt::ok::json-yaml::a: 1",
            title: "Converted",
            subtitle: "",
            iconKind: 1,
            iconValue: "",
            kind: 6,
            score: 0,
            actions: [
                CandidateAction(id: "default", label: "Copy"),
                CandidateAction(id: "preview", label: "Preview"),
            ]
        )
    ]
    vm.selectedIndex = 0
    _ = vm.enterActionsMode()
    if case .textPreview(let text, let label, let language, let edit) = vm.viewMode {
        expect(text == "a: 1\nb: 2", "text carried through")
        expect(label == "JSON → YAML", "label carried through")
        expect(language == "yaml", "language carried through")
        expect(edit == nil, "non-note previews have no editable path")
    } else {
        expect(false, "expected textPreview, got \(vm.viewMode)")
    }
    expect(mock.activations.first?.action == "preview", "fired preview action")
}

runGroup("return in textPreview copies and dismisses when no editable path") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.viewMode = .textPreview(
        text: "payload", label: "X", language: nil, editablePath: nil
    )
    let dismiss = vm.handleEnterKey()
    expect(dismiss, "enter dismisses panel after copy")
    expect(vm.viewMode == .main, "back to main")
}

runGroup("return in textPreview opens editor when editablePath set") {
    // Note preview -> Enter is the intuitive way into editor:
    // No menus, no modifier keys, just Enter on the preview
    let (vm, io, _) = editorVM()
    io.files["/tmp/a.md"] = "# Hi"
    vm.viewMode = .textPreview(
        text: "# Hi",
        label: "a",
        language: "markdown",
        editablePath: "/tmp/a.md"
    )
    let dismiss = vm.handleEnterKey()
    expect(!dismiss, "panel stays open while entering editor")
    expect(vm.viewMode == .editor("/tmp/a.md"), "mode transitioned to editor")
    expect(vm.editingContent == "# Hi", "editor loaded the note body")
}

runGroup("Effect decoder parses ShowText JSON with language") {
    let data = #"{"ShowText":{"text":"abc","label":"Pretty JSON","language":"json"}}"#
        .data(using: .utf8)!
    let eff = try? JSONDecoder().decode(Effect.self, from: data)
    if case .showText(let text, let label, let language, _) = eff {
        expect(text == "abc", "text decoded")
        expect(label == "Pretty JSON", "label decoded")
        expect(language == "json", "language decoded")
    } else {
        expect(false, "decoded wrong variant: \(String(describing: eff))")
    }
}

runGroup("Effect decoder parses ShowText JSON without language (backcompat)") {
    let data = #"{"ShowText":{"text":"abc","label":"X"}}"#.data(using: .utf8)!
    let eff = try? JSONDecoder().decode(Effect.self, from: data)
    if case .showText(_, _, let language, let edit) = eff {
        expect(language == nil, "missing language decodes to nil")
        expect(edit == nil, "missing editable_path decodes to nil")
    } else {
        expect(false, "decoded wrong variant: \(String(describing: eff))")
    }
}

runGroup("Effect decoder parses ShowText with editable_path") {
    // Double pound to avoid Swift's raw-string parser tripping
    // on the `"#` sequence inside `"# x"`
    let data = ##"{"ShowText":{"text":"# x","label":"note","language":"markdown","editable_path":"/tmp/note.md"}}"##
        .data(using: .utf8)!
    let eff = try? JSONDecoder().decode(Effect.self, from: data)
    if case .showText(_, _, _, let edit) = eff {
        expect(edit == "/tmp/note.md", "editable_path decoded: \(edit ?? "nil")")
    } else {
        expect(false, "decoded wrong variant: \(String(describing: eff))")
    }
}

runGroup("handleEscape in editor mode exits editor without dismissing") {
    let (vm, io, _) = editorVM()
    io.files["/a.md"] = ""
    vm.enterEditor(path: "/a.md")
    let handled = vm.handleEscape()
    expect(handled, "esc handled (panel stays open)")
    expect(vm.viewMode == .main, "returned to main")
}

runGroup("handleEnterKey in editor mode returns false (newline stays local)") {
    let (vm, io, _) = editorVM()
    io.files["/a.md"] = "body"
    vm.enterEditor(path: "/a.md")
    let dismiss = vm.handleEnterKey()
    expect(!dismiss, "enter doesn't dismiss the panel in editor")
    expect(vm.viewMode == .editor("/a.md"), "still in editor")
}

runGroup("EditNote effect routes through dispatchEffect into editor") {
    let (vm, io, mock) = editorVM()
    io.files["/b.md"] = "hi"
    mock.nextEffect = .editNote("/b.md")
    vm.results = [makeCand(id: "note::/b.md", title: "B")]
    vm.selectedIndex = 0
    let dismiss = vm.activateSelected()
    expect(!dismiss, "should not dismiss panel for EditNote")
    expect(vm.viewMode == .editor("/b.md"), "mode=editor")
    expect(vm.editingContent == "hi", "content loaded via effect")
}

runGroup("reset clears editor state") {
    let (vm, io, _) = editorVM()
    io.files["/a.md"] = "x"
    vm.enterEditor(path: "/a.md")
    vm.updateEditorContent("changed")
    vm.reset()
    expect(vm.viewMode == .main, "mode=main")
    expect(vm.editingContent == "", "content cleared")
    expect(!vm.editingDirty, "not dirty")
    expect(vm.editorStatus == .idle, "status idle")
}
}
