import Foundation

func runConfigTests() {

runGroup("Config.load from missing file") {
    let tmp = URL(fileURLWithPath: NSTemporaryDirectory())
        .appendingPathComponent("gyors-test-\(UUID().uuidString).json")
    let cfg = Config.load(from: tmp)
    expect(cfg.hotkey == nil, "nil hotkey falls back")
    expect(cfg.hotkeyBinding.keyCode == 49, "default is space")
}

runGroup("Config.load from valid file") {
    let tmp = URL(fileURLWithPath: NSTemporaryDirectory())
        .appendingPathComponent("gyors-test-\(UUID().uuidString).json")
    try? """
    { "hotkey": "cmd+k" }
    """.write(to: tmp, atomically: true, encoding: .utf8)
    defer { try? FileManager.default.removeItem(at: tmp) }
    let cfg = Config.load(from: tmp)
    expect(cfg.hotkey == "cmd+k", "hotkey read")
    expect(cfg.hotkeyBinding.keyCode == 40, "k keyCode")
}

runGroup("Config.load from malformed file") {
    let tmp = URL(fileURLWithPath: NSTemporaryDirectory())
        .appendingPathComponent("gyors-test-\(UUID().uuidString).json")
    try? "not json".write(to: tmp, atomically: true, encoding: .utf8)
    defer { try? FileManager.default.removeItem(at: tmp) }
    let cfg = Config.load(from: tmp)
    expect(cfg.hotkey == nil, "malformed → nil hotkey")
    expect(cfg.hotkeyBinding.keyCode == 49, "falls back to default")
}

}
