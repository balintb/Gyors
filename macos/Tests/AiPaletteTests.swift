import Foundation

/// AI palette - cmd+return dispatch behaviour on the ViewModel side
func runAiPaletteTests() {
runGroup("AI PALETTE: cmd+return in main with non-empty buffer dispatches askAi") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.viewMode = .main
    vm.setActiveBuffer("what is love")
    expect(vm.askAiPalette(),
        "askAiPalette returns true when buffer has content")
    // Successful dispatch flips view-mode to aiThinking with
    // original question as the header
    if case .aiThinking(let q) = vm.viewMode {
        expect(q == "what is love", "thinking header carries question, got `\(q)`")
    } else {
        expect(false, "expected .aiThinking, got \(vm.viewMode)")
    }
}

runGroup("AI PALETTE: cmd+return on empty buffer is a no-op") {
    // The handler returns false on empty input so the
    // KeyCatcher falls through. Result: an empty cmd+return stays
    // a normal Enter - no AI request fires with nothing to
    // ask
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.viewMode = .main
    vm.setActiveBuffer("")
    expect(!vm.askAiPalette(),
        "askAiPalette returns false on empty input")
    expect(vm.viewMode == .main, "viewMode unchanged")
}

runGroup("AI PALETTE: cmd+return on whitespace-only buffer is a no-op") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.viewMode = .main
    vm.setActiveBuffer("    ")
    expect(!vm.askAiPalette(), "trimmed-empty input rejected")
    expect(vm.viewMode == .main, "viewMode unchanged")
}

runGroup("AI PALETTE: cmd+return outside main mode is a no-op") {
    // Editor / preview / aiThinking modes own their own
    // keyboard semantics - the palette must not steal cmd+return
    // from them
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.setActiveBuffer("anything")

    vm.viewMode = .editor("/tmp/x.md")
    expect(!vm.askAiPalette(), "editor mode: no-op")

    vm.viewMode = .imagePreview(base64: "x", label: "QR")
    expect(!vm.askAiPalette(), "image preview: no-op")

    vm.viewMode = .aiThinking("ongoing")
    expect(!vm.askAiPalette(), "aiThinking: no-op (already running)")
}

runGroup("AI PALETTE: askAiPalette uses query (chain stages + active buffer)") {
    // The current `query` reflects committed pills + active
    // text combined - exercising that the palette routes the
    // FULL composition rather than just the last segment
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.viewMode = .main
    vm.setActiveBuffer("foo")
    vm.commitActiveAsPill()
    vm.setActiveBuffer("bar")
    // After commit + new buffer, query is "foo | bar"
    expect(vm.askAiPalette(), "dispatched")
    if case .aiThinking(let q) = vm.viewMode {
        expect(q.contains("foo"), "first stage in question")
        expect(q.contains("bar"), "active buffer in question")
    } else {
        expect(false, "expected aiThinking, got \(vm.viewMode)")
    }
}
}
