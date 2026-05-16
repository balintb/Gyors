import Foundation

func runAiRouterToolsCatalogTests() {
//
// Regression: the router used to expose a `regex__test` tool that
// hallucinated `re ^...$ :: ...` calls on free-form questions
// (LLMs love "test a regex" because it pattern-matches any text).
// Pin catalog shape so it can't quietly grow back

runGroup("AiRouterTools.all does NOT include regex tool") {
    let names = AiRouterTools.all.map { $0.name }
    expect(!names.contains("regex__test"),
        "regex__test must not be in router catalog (hallucinates on non-regex questions)")
    expect(!AiRouterTools.all.contains { $0.template.contains("re {") },
        "no router tool may render to a `re ...` keyword form")
}

runGroup("AiRouterTools.all has exactly the read-only tools we ship") {
    let names = Set(AiRouterTools.all.map { $0.name })
    expect(names == Set(["currency__convert", "units__convert", "calc__evaluate"]),
        "router catalog should be the three read-only tools only - got \(names.sorted())")
}

runGroup("AiRouterTools.find returns nil for removed regex tool") {
    expect(AiRouterTools.find(named: "regex__test") == nil,
        "find(`regex__test`) must return nil after removal")
}

runGroup("AiRouterTools.render rejects regex tool calls") {
    // Even if a model hallucinates the tool name, render must
    // surface UnknownTool rather than producing `re ... :: ...`
    let bogus = ToolCall(toolName: "regex__test",
                         arguments: ["pattern": "^x$", "text": "x"])
    switch AiRouterTools.render(bogus) {
    case .success(let s):
        expect(false, "regex__test rendered to \(s) but tool was removed")
    case .failure(let err):
        expect(err == .unknownTool("regex__test"),
            "expected .unknownTool(regex__test), got \(err)")
    }
}

runGroup("AiRouterTools.render currency happy path still works") {
    let call = ToolCall(toolName: "currency__convert",
                        arguments: ["amount": "100", "from": "USD", "to": "EUR"])
    switch AiRouterTools.render(call) {
    case .success(let s):
        expect(s == "100 USD in EUR", "got \(s)")
    case .failure:
        expect(false, "currency render should succeed")
    }
}

runGroup("AiRouterTools.render units happy path still works") {
    let call = ToolCall(toolName: "units__convert",
                        arguments: ["amount": "5", "from": "km", "to": "miles"])
    switch AiRouterTools.render(call) {
    case .success(let s): expect(s == "5 km in miles", "got \(s)")
    case .failure:        expect(false, "units render should succeed")
    }
}

runGroup("AiRouterTools.render calc happy path still works") {
    let call = ToolCall(toolName: "calc__evaluate",
                        arguments: ["expression": "12 * 7"])
    switch AiRouterTools.render(call) {
    case .success(let s): expect(s == "12 * 7", "got \(s)")
    case .failure:        expect(false, "calc render should succeed")
    }
}

runGroup("AiRouterTools.render reports missing required args") {
    let call = ToolCall(toolName: "currency__convert",
                        arguments: ["amount": "100", "from": "USD"])  // missing `to`
    switch AiRouterTools.render(call) {
    case .success: expect(false, "should have failed on missing `to`")
    case .failure(let err):
        expect(err == .missingArgument("to"), "expected missing `to`, got \(err)")
    }
}

runGroup("AiRouterTools tools have unique names") {
    let names = AiRouterTools.all.map { $0.name }
    expect(names.count == Set(names).count, "duplicate tool name in catalog")
}

runGroup("AiRouterTools tools never render a `re ...` keyword") {
    // Even if someone adds a future tool whose
    // template happens to start with `re`, fail loudly. Keeps
    // this regression buried even if catalog grows
    for tool in AiRouterTools.all {
        let trimmed = tool.template.trimmingCharacters(in: .whitespaces)
        expect(!trimmed.hasPrefix("re "),
            "tool \(tool.name) template starts with `re ` - would re-introduce regex hallucination")
    }
}

}
