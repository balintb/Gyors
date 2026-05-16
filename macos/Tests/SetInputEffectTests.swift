import Foundation

func runSetInputEffectTests() {

runGroup("SetInput prefills the query and keeps panel open") {
    let mock = MockBridge()
    mock.nextEffect = .setInput("md5 ")
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [makeCand(id: "hint::md5", title: "md5 <text>")]
    vm.selectedIndex = 0
    let dismiss = vm.activateSelected()
    expect(!dismiss, "does not dismiss")
    expect(vm.query == "md5 ", "query prefilled")
}

runGroup("Non-SetInput effect dismisses the panel") {
    let mock = MockBridge()
    mock.nextEffect = .copyToClipboard("hello")
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [makeCand(id: "encode::x", title: "X")]
    vm.selectedIndex = 0
    let dismiss = vm.activateSelected()
    expect(dismiss, "dismisses on non-setInput")
}

runGroup("activateAction with SetInput keeps panel open") {
    let mock = MockBridge()
    mock.nextEffect = .setInput("sha256 ")
    let vm = GyorsViewModel(bridge: mock)
    vm.viewMode = .actions(makeCand(id: "hint::sha256", title: "sha256 <text>"))
    vm.actionIndex = 0
    let dismiss = vm.activateAction()
    expect(!dismiss, "does not dismiss")
    expect(vm.query == "sha256 ", "query updated")
}

}
