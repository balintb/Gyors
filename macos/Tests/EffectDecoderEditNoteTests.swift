import Foundation

func runEffectDecoderEditNoteTests() {

runGroup("Effect decoder parses EditNote JSON") {
    let data = #"{"EditNote":"/tmp/note.md"}"#.data(using: .utf8)!
    let eff = try? JSONDecoder().decode(Effect.self, from: data)
    if case .editNote(let p) = eff {
        expect(p == "/tmp/note.md", "path decoded")
    } else {
        expect(false, "decoded wrong variant: \(String(describing: eff))")
    }
}

runGroup("Effect decoder parses AiTransform JSON") {
    let data = #"{"AiTransform":{"text":"one two","instruction":"Summarize"}}"#
        .data(using: .utf8)!
    let eff = try? JSONDecoder().decode(Effect.self, from: data)
    if case .aiTransform(let t, let i) = eff {
        expect(t == "one two", "text decoded")
        expect(i == "Summarize", "instruction decoded")
    } else {
        expect(false, "decoded wrong variant: \(String(describing: eff))")
    }
}

runGroup("AiTransform effect routes to aiThinking") {
    // The dispatch path should flip into aiThinking immediately
    // so user gets a spinner even before the network call
    let mock = MockBridge()
    mock.nextEffect = .aiTransform(text: "hi", instruction: "Translate to French")
    let vm = GyorsViewModel(bridge: mock)
    vm.results = [makeCand(id: "ait::translate::abc", title: "Translate")]
    vm.selectedIndex = 0
    let dismiss = vm.activateSelected()
    expect(!dismiss, "panel stays open while AI runs")
    if case .aiThinking(let q) = vm.viewMode {
        expect(q == "Translate to French", "header shows instruction, not raw text")
    } else {
        expect(false, "expected aiThinking, got \(vm.viewMode)")
    }
}

runGroup("Effect decoder parses TrashFile JSON") {
    let data = #"{"TrashFile":"/tmp/note.md"}"#.data(using: .utf8)!
    let eff = try? JSONDecoder().decode(Effect.self, from: data)
    if case .trashFile(let p) = eff {
        expect(p == "/tmp/note.md", "path decoded")
    } else {
        expect(false, "decoded wrong variant: \(String(describing: eff))")
    }
}

}
