import Foundation

func runNewNoteShortcutTests() {

runGroup("newNoteShortcut from empty query primes newnote prefix") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    _ = vm.newNoteShortcut()
    expect(vm.query == "newnote ", "got \(vm.query)")
}

runGroup("newNoteShortcut strips `note ` prefix and reuses filter as title") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.query = "note meeting agenda"
    _ = vm.newNoteShortcut()
    expect(vm.query == "newnote meeting agenda", "got \(vm.query)")
}

runGroup("newNoteShortcut strips bare `n` alias") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.query = "n"
    _ = vm.newNoteShortcut()
    expect(vm.query == "newnote ", "got \(vm.query)")
}

runGroup("newNoteShortcut preserves free text as title") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.query = "grocery list"
    _ = vm.newNoteShortcut()
    expect(vm.query == "newnote grocery list", "got \(vm.query)")
}

runGroup("newNoteShortcut refuses outside main mode") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.viewMode = .editor("/tmp/x.md")
    let ok = vm.newNoteShortcut()
    expect(!ok, "should not fire in editor mode")
}

}
