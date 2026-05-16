import Foundation

func runChainStateParseTests() {
// ChainState: display-only parse helper (kept for subtitles)

runGroup("ChainState.parse empty → empty state") {
    let s = ChainState.parse("")
    expect(s.commits.isEmpty, "commits empty")
    expect(s.active == "", "active empty")
}

runGroup("ChainState.parse space-padded pipe splits") {
    // Used only for debug/display; actual pill state lives
    // on the VM. Matches Rust parser's ` | ` rule
    let s = ChainState.parse("note hello | copy")
    expect(s.commits == ["note hello"], "one pill")
    expect(s.active == "copy", "active is copy")
}

runGroup("ChainState.parse bare `|` is literal") {
    let s = ChainState.parse("re cat|dog")
    expect(s.commits.isEmpty, "no pill - pipe not space-padded")
    expect(s.active == "re cat|dog", "literal text preserved")
}

}
