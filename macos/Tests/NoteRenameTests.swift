import Foundation

func runNoteRenameTests() {

func renameVM(rootName: String) -> (GyorsViewModel, MockBridge, MockNoteIO, URL) {
    // A scratch directory so rename actually moves files on disk
    let temp = URL(fileURLWithPath: NSTemporaryDirectory())
        .appendingPathComponent("gyors-rename-\(rootName)-\(UUID().uuidString)")
    try? FileManager.default.createDirectory(at: temp, withIntermediateDirectories: true)
    let mock = MockBridge()
    mock.stubNotesFolder = temp.path
    let vm = GyorsViewModel(bridge: mock)
    let io = MockNoteIO()
    vm.noteIO = io
    return (vm, mock, io, temp)
}

runGroup("renameCurrentNote refuses outside editor mode") {
    let (vm, _, _, _) = renameVM(rootName: "noedit")
    let result = vm.renameCurrentNote(to: "foo.md")
    expect(result == .notEditing, "not editing guard")
}

runGroup("renameCurrentNote moves file and updates viewMode") {
    let (vm, _, _, temp) = renameVM(rootName: "move")
    let src = temp.appendingPathComponent("a.md")
    try? "# A".write(to: src, atomically: true, encoding: .utf8)
    vm.viewMode = .editor(src.path)
    vm.editingContent = "# A"
    vm.editingDirty = false

    let result = vm.renameCurrentNote(to: "renamed.md")
    switch result {
    case .ok(let p):
        expect(p == temp.appendingPathComponent("renamed.md").path, "new path ok")
        expect(FileManager.default.fileExists(atPath: p), "file on disk")
        expect(!FileManager.default.fileExists(atPath: src.path), "old removed")
        if case .editor(let m) = vm.viewMode {
            expect(m == p, "viewMode switched to new path")
        } else {
            expect(false, "viewMode not editor")
        }
    default:
        expect(false, "expected .ok, got \(result)")
    }
}

runGroup("renameCurrentNote appends .md when missing") {
    let (vm, _, _, temp) = renameVM(rootName: "ext")
    let src = temp.appendingPathComponent("a.md")
    try? "x".write(to: src, atomically: true, encoding: .utf8)
    vm.viewMode = .editor(src.path)
    let result = vm.renameCurrentNote(to: "bareword")
    if case .ok(let p) = result {
        expect(p.hasSuffix("/bareword.md"), "suffix appended: \(p)")
    } else {
        expect(false, "expected ok, got \(result)")
    }
}

runGroup("renameCurrentNote creates nested folders") {
    let (vm, _, _, temp) = renameVM(rootName: "nest")
    let src = temp.appendingPathComponent("flat.md")
    try? "x".write(to: src, atomically: true, encoding: .utf8)
    vm.viewMode = .editor(src.path)
    let result = vm.renameCurrentNote(to: "work/deep/note.md")
    if case .ok(let p) = result {
        expect(FileManager.default.fileExists(atPath: p), "file created")
        expect(p.contains("/work/deep/"), "nested path preserved")
    } else {
        expect(false, "expected ok, got \(result)")
    }
}

runGroup("renameCurrentNote rejects conflict with existing file") {
    let (vm, _, _, temp) = renameVM(rootName: "conflict")
    let src = temp.appendingPathComponent("a.md")
    let other = temp.appendingPathComponent("b.md")
    try? "x".write(to: src, atomically: true, encoding: .utf8)
    try? "y".write(to: other, atomically: true, encoding: .utf8)
    vm.viewMode = .editor(src.path)
    let result = vm.renameCurrentNote(to: "b.md")
    if case .conflict = result { expect(true, "conflict") }
    else { expect(false, "expected conflict, got \(result)") }
    // Source must still exist (nothing moved)
    expect(FileManager.default.fileExists(atPath: src.path), "source intact")
}

runGroup("renameCurrentNote rejects dotdot escape") {
    let (vm, _, _, temp) = renameVM(rootName: "escape")
    let src = temp.appendingPathComponent("a.md")
    try? "x".write(to: src, atomically: true, encoding: .utf8)
    vm.viewMode = .editor(src.path)
    let result = vm.renameCurrentNote(to: "../escape.md")
    if case .invalidPath = result { expect(true, "rejected") }
    else { expect(false, "expected invalid, got \(result)") }
    expect(FileManager.default.fileExists(atPath: src.path), "source intact")
}

runGroup("renameCurrentNote rejects empty path") {
    let (vm, _, _, temp) = renameVM(rootName: "empty")
    let src = temp.appendingPathComponent("a.md")
    try? "x".write(to: src, atomically: true, encoding: .utf8)
    vm.viewMode = .editor(src.path)
    let result = vm.renameCurrentNote(to: "   ")
    if case .invalidPath = result { expect(true, "rejected") }
    else { expect(false, "expected invalid, got \(result)") }
}

runGroup("renameCurrentNote no-ops when target equals source") {
    let (vm, _, _, temp) = renameVM(rootName: "same")
    let src = temp.appendingPathComponent("a.md")
    try? "x".write(to: src, atomically: true, encoding: .utf8)
    vm.viewMode = .editor(src.path)
    let result = vm.renameCurrentNote(to: "a.md")
    if case .ok(let p) = result {
        expect(p == src.path, "same path returned")
        expect(FileManager.default.fileExists(atPath: src.path), "source intact")
    } else {
        expect(false, "expected ok, got \(result)")
    }
}

runGroup("renameCurrentNote flushes dirty buffer before moving") {
    let (vm, _, io, temp) = renameVM(rootName: "flush")
    let src = temp.appendingPathComponent("a.md")
    try? "old".write(to: src, atomically: true, encoding: .utf8)
    io.files[src.path] = "old"
    vm.viewMode = .editor(src.path)
    vm.editingContent = "NEW"
    vm.editingDirty = true
    _ = vm.renameCurrentNote(to: "renamed.md")
    // The in-memory MockNoteIO is what the VM used to save; the
    // physical move happens through FileManager against the real
    // `old` contents. The important invariant: VM flushed state
    // (mock received the write) before issuing the move
    expect(io.files[src.path] == "NEW", "buffer flushed before move")
}

}
