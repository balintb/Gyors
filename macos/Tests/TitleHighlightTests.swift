import Foundation

func runTitleHighlightTests() {

runGroup("TitleHighlight.split returns nil for empty query") {
    expect(TitleHighlight.split(title: "b64 <text>", query: "") == nil, "empty query")
    expect(TitleHighlight.split(title: "b64 <text>", query: "   ") == nil, "whitespace-only query")
}

runGroup("TitleHighlight.split returns nil when title doesn't start with query") {
    expect(TitleHighlight.split(title: "Safari", query: "xyz") == nil, "mismatch")
    expect(TitleHighlight.split(title: "Alpha Notes", query: "notes") == nil, "not a prefix")
}

runGroup("TitleHighlight.split returns (typed, rest) on prefix match") {
    let split = TitleHighlight.split(title: "b64 <text>", query: "b6")
    expect(split != nil, "split returned")
    expect(split?.typed == "b6", "typed portion preserves casing")
    expect(split?.rest == "4 <text>", "rest is correct")
}

runGroup("TitleHighlight.split is case-insensitive on match, preserves display case") {
    let split = TitleHighlight.split(title: "Safari", query: "saf")
    expect(split?.typed == "Saf", "kept original title casing")
    expect(split?.rest == "ari", "rest matches original")
}

runGroup("TitleHighlight.split handles multi-byte (emoji / CJK) correctly") {
    let split = TitleHighlight.split(title: "漢字ABC", query: "漢")
    expect(split?.typed == "漢", "single grapheme split")
    expect(split?.rest == "字ABC", "rest after multi-byte char")
}

runGroup("TitleHighlight.split handles full-query match") {
    let split = TitleHighlight.split(title: "command", query: "command")
    expect(split?.typed == "command", "full match is all typed")
    expect(split?.rest == "", "rest is empty")
}

runGroup("TitleHighlight.split trims surrounding whitespace in query") {
    let split = TitleHighlight.split(title: "b64 <text>", query: "  b6  ")
    expect(split?.typed == "b6", "trimmed query still splits")
}

}
