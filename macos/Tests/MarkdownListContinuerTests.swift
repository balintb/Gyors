import Foundation

func runMarkdownListContinuerTests() {

runGroup("list continuer: bullet line continues with same marker") {
    let action = MarkdownListContinuer.continuation(beforeCursor: "- groceries")
    expect(action == .insert("- "), "got \(action)")
}

runGroup("list continuer: star/plus bullets also continue") {
    expect(
        MarkdownListContinuer.continuation(beforeCursor: "* first") == .insert("* "),
        "asterisk"
    )
    expect(
        MarkdownListContinuer.continuation(beforeCursor: "+ first") == .insert("+ "),
        "plus"
    )
}

runGroup("list continuer: indented bullet preserves indent") {
    let action = MarkdownListContinuer.continuation(beforeCursor: "  - sub item")
    expect(action == .insert("  - "), "got \(action)")
}

runGroup("list continuer: empty bullet terminates list") {
    expect(
        MarkdownListContinuer.continuation(beforeCursor: "- ") == .terminate,
        "empty dash → terminate"
    )
    expect(
        MarkdownListContinuer.continuation(beforeCursor: "  * ") == .terminate,
        "indented empty star → terminate"
    )
}

runGroup("list continuer: numbered items increment") {
    let action = MarkdownListContinuer.continuation(beforeCursor: "1. one")
    expect(action == .insert("2. "), "got \(action)")
    let action2 = MarkdownListContinuer.continuation(beforeCursor: "42. answer")
    expect(action2 == .insert("43. "), "got \(action2)")
}

runGroup("list continuer: empty numbered terminates") {
    let action = MarkdownListContinuer.continuation(beforeCursor: "3. ")
    expect(action == .terminate, "got \(action)")
}

runGroup("list continuer: blockquote continues") {
    let action = MarkdownListContinuer.continuation(beforeCursor: "> quoted")
    expect(action == .insert("> "), "got \(action)")
}

runGroup("list continuer: empty blockquote terminates") {
    expect(
        MarkdownListContinuer.continuation(beforeCursor: "> ") == .terminate,
        "empty `> ` → terminate"
    )
}

runGroup("list continuer: plain prose returns .none") {
    expect(
        MarkdownListContinuer.continuation(beforeCursor: "hello world") == .none,
        "non-list line"
    )
    expect(
        MarkdownListContinuer.continuation(beforeCursor: "") == .none,
        "empty line"
    )
    expect(
        MarkdownListContinuer.continuation(beforeCursor: "# heading") == .none,
        "headings are not continuation targets"
    )
}

runGroup("list continuer: numeric prefix without `. ` is not numbered") {
    expect(
        MarkdownListContinuer.continuation(beforeCursor: "1 not numbered") == .none,
        "missing dot"
    )
    expect(
        MarkdownListContinuer.continuation(beforeCursor: "1.no-space") == .none,
        "missing space"
    )
}

runGroup("exitEditor bumps focusTick so TextField re-focuses") {
    // REGRESSION: after "save & new command" (ESC in editor)
    // query TextField came back unfocused - user had to
    // click it. ContentView re-focuses via .onChange(focusTick),
    // so exitEditor must bump the tick
    let (vm, io, _) = editorVM()
    io.files["/a.md"] = "body"
    let before = vm.focusTick
    vm.enterEditor(path: "/a.md")
    _ = vm.exitEditor()
    expect(vm.focusTick != before, "focusTick bumped (\(before) → \(vm.focusTick))")
}

runGroup("saveEditorAndRequestClose flushes dirty buffer and signals dismiss") {
    let (vm, io, _) = editorVM()
    io.files["/b.md"] = ""
    vm.enterEditor(path: "/b.md")
    vm.updateEditorContent("final text")
    let ok = vm.saveEditorAndRequestClose()
    expect(ok, "returned true for caller to dismiss")
    expect(vm.viewMode == .main, "mode=main")
    expect(io.files["/b.md"] == "final text", "buffer flushed")
    expect(vm.editingContent == "", "editor state cleared")
}

runGroup("saveEditorAndRequestClose refuses outside editor mode") {
    let (vm, _, _) = editorVM()
    vm.viewMode = .main
    let ok = vm.saveEditorAndRequestClose()
    expect(!ok, "no-op when not editing")
}

}
