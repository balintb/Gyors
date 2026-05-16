import Foundation

func runEditorTests() {
// `editorVM()` is a shared test helper - see TestMocks.swift

runGroup("enterEditor loads file content and switches mode") {
    let (vm, io, _) = editorVM()
    io.files["/a.md"] = "# Hello\n\nbody"
    vm.enterEditor(path: "/a.md")
    expect(vm.viewMode == .editor("/a.md"), "mode=editor")
    expect(vm.editingContent == "# Hello\n\nbody", "content loaded")
    expect(!vm.editingDirty, "starts clean")
    expect(vm.editorStatus == .idle, "starts idle")
}

runGroup("enterEditor on missing path opens empty buffer") {
    let (vm, _, _) = editorVM()
    vm.enterEditor(path: "/does-not-exist.md")
    expect(vm.viewMode == .editor("/does-not-exist.md"), "mode=editor")
    expect(vm.editingContent == "", "empty buffer")
    expect(!vm.editingDirty, "not dirty")
}

runGroup("updateEditorContent marks dirty") {
    let (vm, io, _) = editorVM()
    io.files["/a.md"] = "old"
    vm.enterEditor(path: "/a.md")
    vm.updateEditorContent("new stuff")
    expect(vm.editingContent == "new stuff", "buffer updated")
    expect(vm.editingDirty, "dirty=true")
}

runGroup("updateEditorContent noop outside editor mode") {
    let (vm, _, _) = editorVM()
    vm.viewMode = .main
    vm.updateEditorContent("nope")
    expect(vm.editingContent == "", "buffer unchanged")
    expect(!vm.editingDirty, "not dirty")
}

runGroup("saveEditorContent writes dirty buffer and clears flag") {
    let (vm, io, _) = editorVM()
    io.files["/a.md"] = "old"
    vm.enterEditor(path: "/a.md")
    vm.updateEditorContent("fresh body")
    let ok = vm.saveEditorContent()
    expect(ok, "save returned true")
    expect(io.files["/a.md"] == "fresh body", "file contains new body")
    expect(!vm.editingDirty, "clean after save")
    expect(vm.editorStatus == .saved, "status=saved")
}

runGroup("saveEditorContent is noop when not dirty") {
    let (vm, io, _) = editorVM()
    io.files["/a.md"] = "body"
    vm.enterEditor(path: "/a.md")
    io.writeCalls = 0
    _ = vm.saveEditorContent()
    expect(io.writeCalls == 0, "no disk write when clean")
    expect(vm.editorStatus == .saved, "still saved")
}

runGroup("saveEditorContent surfaces errors via status") {
    let (vm, io, _) = editorVM()
    io.files["/a.md"] = "old"
    vm.enterEditor(path: "/a.md")
    vm.updateEditorContent("fresh")
    io.failNextWrite = true
    let ok = vm.saveEditorContent()
    expect(!ok, "save returned false")
    expect(vm.editingDirty, "still dirty on failure")
    if case .error = vm.editorStatus { expect(true, "status=error") }
    else { expect(false, "expected error status") }
}

runGroup("exitEditor flushes dirty buffer then returns to main") {
    let (vm, io, _) = editorVM()
    io.files["/a.md"] = "old"
    vm.enterEditor(path: "/a.md")
    vm.updateEditorContent("last change")
    _ = vm.exitEditor()
    expect(vm.viewMode == .main, "mode=main")
    expect(io.files["/a.md"] == "last change", "flushed on exit")
    expect(vm.editingContent == "", "buffer cleared")
    expect(!vm.editingDirty, "not dirty")
}

runGroup("exitEditor clears query and results for a fresh prompt") {
    // UX: after saving a note, Gyors stays open but drops back to
    // an empty command field - not the stale `note foo` search
    let (vm, io, _) = editorVM()
    io.files["/a.md"] = ""
    vm.query = "note meeting"
    vm.results = [makeCand(id: "note::x", title: "X")]
    vm.selectedIndex = 0
    vm.enterEditor(path: "/a.md")
    vm.updateEditorContent("hello")
    _ = vm.exitEditor()
    expect(vm.query == "", "query cleared")
    expect(vm.results.isEmpty, "results cleared")
    expect(vm.selectedIndex == 0, "selection reset")
}

}
