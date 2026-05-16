import Foundation

func runActivateAtIndexTests() {

runGroup("activateAtIndex picks the nth result and runs its default action") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [
        makeCand(id: "a", title: "A"),
        makeCand(id: "b", title: "B"),
        makeCand(id: "c", title: "C"),
    ]
    _ = vm.activateAtIndex(1)
    expect(vm.selectedIndex == 1, "selection moved to index 1")
    expect(mock.activations.count == 1, "activated once")
    expect(mock.activations.first?.id == "b", "activated B")
    expect(mock.activations.first?.action == "default", "default action")
}

runGroup("activateAtIndex returns false for out-of-range index") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [makeCand(id: "a", title: "A")]
    let ok = vm.activateAtIndex(5)
    expect(!ok, "out-of-range returns false")
    expect(mock.activations.isEmpty, "nothing activated")
}

runGroup("activateAtIndex refuses when not in main mode") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [makeCand(id: "a", title: "A")]
    vm.viewMode = .editor("/tmp/x.md")
    let ok = vm.activateAtIndex(0)
    expect(!ok, "refused outside main mode")
}

runGroup("activateAtIndex honours dismiss signal from bridge") {
    let mock = MockBridge()
    mock.nextEffect = .openPath("/whatever")
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [makeCand(id: "a", title: "A")]
    let dismiss = vm.activateAtIndex(0)
    expect(dismiss, "openPath triggers dismiss")
}

runGroup("activateAtIndex with SetInput keeps panel open") {
    let mock = MockBridge()
    mock.nextEffect = .setInput("prefill ")
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [makeCand(id: "hint::x", title: "prefill")]
    let dismiss = vm.activateAtIndex(0)
    expect(!dismiss, "SetInput shouldn't dismiss")
    expect(vm.query == "prefill ", "query updated")
}

}
