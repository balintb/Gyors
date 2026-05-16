import Foundation

func runHotkeyBindingTests() {

runGroup("parse default opt+shift+space") {
    let b = HotkeyBinding.parse("opt+shift+space")
    expect(b != nil, "parsed non-nil")
    expect(b?.keyCode == 49, "space keyCode = 49")
    let mods = b?.modifiers ?? []
    expect(mods.contains { if case .option = $0 { return true } else { return false } }, "contains .option")
    expect(mods.contains { if case .shift  = $0 { return true } else { return false } }, "contains .shift")
}

runGroup("parse cmd+space") {
    let b = HotkeyBinding.parse("cmd+space")
    expect(b?.keyCode == 49, "space keyCode = 49")
    expect(b?.modifiers.count == 1, "one modifier")
}

runGroup("parse case-insensitive") {
    let b1 = HotkeyBinding.parse("OPT+SHIFT+SPACE")
    let b2 = HotkeyBinding.parse("Opt+Shift+Space")
    expect(b1?.keyCode == b2?.keyCode, "case-insensitive key")
    expect(b1?.modifiers.count == b2?.modifiers.count, "case-insensitive mods")
}

runGroup("parse accepts dash separator") {
    let b = HotkeyBinding.parse("ctrl-alt-k")
    expect(b != nil, "parsed")
    expect(b?.keyCode == 40, "k keyCode = 40")
    expect(b?.modifiers.count == 2, "two mods")
}

runGroup("parse accepts space separator") {
    let b = HotkeyBinding.parse("opt shift space")
    expect(b != nil, "parsed")
    expect(b?.keyCode == 49, "space keyCode = 49")
    expect(b?.modifiers.count == 2, "two mods")
}

runGroup("parse modifier-only fails") {
    expect(HotkeyBinding.parse("cmd") == nil, "no key → nil")
    expect(HotkeyBinding.parse("cmd+shift") == nil, "modifiers only → nil")
}

runGroup("parse empty/gibberish fails") {
    expect(HotkeyBinding.parse("") == nil, "empty")
    expect(HotkeyBinding.parse("+++") == nil, "separators only")
    expect(HotkeyBinding.parse("zzz") == nil, "unknown token")
    expect(HotkeyBinding.parse("opt+foo") == nil, "unknown non-modifier")
}

runGroup("parse two keys fails") {
    expect(HotkeyBinding.parse("opt+space+tab") == nil, "rejects two keys")
}

runGroup("parse letter keys") {
    expect(HotkeyBinding.parse("cmd+a")?.keyCode == 0, "a = 0")
    expect(HotkeyBinding.parse("cmd+z")?.keyCode == 6, "z = 6")
    expect(HotkeyBinding.parse("cmd+m")?.keyCode == 46, "m = 46")
}

runGroup("parse digit keys") {
    expect(HotkeyBinding.parse("cmd+0")?.keyCode == 29, "0 = 29")
    expect(HotkeyBinding.parse("cmd+9")?.keyCode == 25, "9 = 25")
}

runGroup("parse function keys") {
    expect(HotkeyBinding.parse("f1")?.keyCode == 122, "f1")
    expect(HotkeyBinding.parse("f12")?.keyCode == 111, "f12")
}

runGroup("parse arrow keys") {
    expect(HotkeyBinding.parse("cmd+up")?.keyCode == 126, "up")
    expect(HotkeyBinding.parse("cmd+down")?.keyCode == 125, "down")
    expect(HotkeyBinding.parse("cmd+left")?.keyCode == 123, "left")
    expect(HotkeyBinding.parse("cmd+right")?.keyCode == 124, "right")
}

runGroup("parse special keys") {
    expect(HotkeyBinding.parse("tab")?.keyCode == 48, "tab")
    expect(HotkeyBinding.parse("escape")?.keyCode == 53, "escape")
    expect(HotkeyBinding.parse("esc")?.keyCode == 53, "esc alias")
    expect(HotkeyBinding.parse("return")?.keyCode == 36, "return")
    expect(HotkeyBinding.parse("enter")?.keyCode == 36, "enter alias")
    expect(HotkeyBinding.parse("delete")?.keyCode == 51, "delete")
}

runGroup("parse modifier aliases") {
    expect(HotkeyBinding.parse("command+k")?.modifiers.count == 1, "command alias")
    expect(HotkeyBinding.parse("option+k")?.modifiers.count == 1, "option alias")
    expect(HotkeyBinding.parse("alt+k")?.modifiers.count == 1, "alt alias")
    expect(HotkeyBinding.parse("control+k")?.modifiers.count == 1, "control alias")
}

}
