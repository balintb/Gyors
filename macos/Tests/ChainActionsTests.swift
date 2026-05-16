import Foundation

func runChainActionsTests() {

let multiActions = [
    CandidateAction(id: "default", label: "Open"),
    CandidateAction(id: "reveal", label: "Show in Finder"),
]

runGroup("enterActionsMode activates for multi-action candidate") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [makeCand(id: "a", title: "A", actions: multiActions)]
    vm.selectedIndex = 0
    let ok = vm.enterActionsMode()
    expect(ok, "returned true")
    if case .actions(let c) = vm.viewMode {
        expect(c.id == "a", "viewMode holds selected candidate")
    } else {
        expect(false, "expected .actions viewMode")
    }
    expect(vm.actionIndex == 0, "actionIndex reset to 0")
}

runGroup("enterActionsMode noop with no selection") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = []
    let ok = vm.enterActionsMode()
    expect(!ok, "returned false")
    expect(vm.viewMode == .main, "viewMode unchanged")
}

runGroup("enterActionsMode noop with zero-actions candidate") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [makeCand(id: "a", title: "A", actions: [])]
    vm.selectedIndex = 0
    let ok = vm.enterActionsMode()
    expect(!ok, "returned false")
}

runGroup("exitActionsMode returns to main") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.viewMode = .actions(makeCand(id: "a", title: "A"))
    vm.actionIndex = 1
    let ok = vm.exitActionsMode()
    expect(ok, "returned true")
    expect(vm.viewMode == .main, "back to main")
    expect(vm.actionIndex == 0, "actionIndex reset")
}

runGroup("exitActionsMode noop in main") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    let ok = vm.exitActionsMode()
    expect(!ok, "returned false")
}

runGroup("moveActionSelection clamps to actions bounds") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.viewMode = .actions(makeCand(id: "a", title: "A", actions: multiActions))
    vm.actionIndex = 0
    vm.moveActionSelection(by: -1)
    expect(vm.actionIndex == 0, "clamps at 0")
    vm.moveActionSelection(by: 10)
    expect(vm.actionIndex == 1, "clamps at last")
}

runGroup("activateAction dispatches correct action id") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.viewMode = .actions(makeCand(id: "a", title: "A", actions: multiActions))
    vm.actionIndex = 1
    let ok = vm.activateAction()
    expect(ok, "returned true")
    expect(mock.activations.last?.action == "reveal", "used correct action id")
    expect(mock.activations.last?.id == "a", "used correct candidate id")
    expect(vm.viewMode == .main, "returned to main after activation")
}

runGroup("activateAction noop in main mode") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    let ok = vm.activateAction()
    expect(!ok, "returned false")
    expect(mock.activations.isEmpty, "no activation")
}

runGroup("activateSelected uses default action") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [makeCand(id: "x", title: "X")]
    vm.selectedIndex = 0
    let ok = vm.activateSelected()
    expect(ok, "returned true")
    expect(mock.activations.last?.action == "default", "default action")
    expect(mock.activations.last?.id == "x", "correct id")
}

runGroup("handleUp/Down routes to action selection in actions mode") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.viewMode = .actions(makeCand(id: "a", title: "A", actions: multiActions))
    vm.handleDownKey()
    expect(vm.actionIndex == 1, "moved action down")
    vm.handleUpKey()
    expect(vm.actionIndex == 0, "moved action up")
}

runGroup("handleUp/Down routes to result selection in main") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [
        makeCand(id: "a", title: "A"),
        makeCand(id: "b", title: "B"),
    ]
    vm.handleDownKey()
    expect(vm.selectedIndex == 1, "moved result down")
}

runGroup("handleEnter in actions mode activates action") {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    vm.viewMode = .actions(makeCand(id: "a", title: "A", actions: multiActions))
    vm.actionIndex = 1
    let dismiss = vm.handleEnterKey()
    expect(dismiss, "signals dismiss")
    expect(mock.activations.last?.action == "reveal", "activated reveal")
}

}
