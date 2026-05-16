import Foundation

/// AI router - catalog shape + general render contract
func runAiRouterTests() {

runGroup("ROUTER CATALOG: every tool has non-empty contract fields") {
    // The model sees these strings - empty values would
    // produce silent routing failures or worse, hallucinated
    // calls. Validate up front so a future contributor who
    // forgets a description can't ship the regression
    for tool in AiRouterTools.all {
        expect(!tool.name.isEmpty, "tool has name")
        expect(!tool.description.isEmpty,
            "\(tool.name) has description")
        expect(tool.description.count >= 10,
            "\(tool.name) description is meaningful")
        expect(!tool.template.isEmpty,
            "\(tool.name) has template")
        expect(!tool.parameters.properties.isEmpty,
            "\(tool.name) declares at least one property")
    }
}

runGroup("ROUTER CATALOG: tool names follow provider__verb shape") {
    // Show-separated for JSON-schema friendliness;
    // dots would confuse callers that flatten dotted keys.
    // Hyphens reserved for keyword form on orchestrator
    // side (e.g. `sha3-512`). Keep tool names tidy
    for tool in AiRouterTools.all {
        expect(tool.name.contains("__"),
            "\(tool.name) follows <provider>__<verb> shape")
        expect(!tool.name.contains("."),
            "\(tool.name) has no dots")
        expect(tool.name.lowercased() == tool.name,
            "\(tool.name) is lowercase")
    }
}

runGroup("ROUTER CATALOG: every required arg appears in template") {
    // Template substitution is the whole dispatch - a
    // required parameter the model fills in must actually
    // make it into keyword query, otherwise we silently
    // drop user-supplied data
    for tool in AiRouterTools.all {
        for required in tool.parameters.required {
            let placeholder = "{\(required)}"
            expect(tool.template.contains(placeholder),
                "\(tool.name): required `\(required)` missing from template")
        }
    }
}

runGroup("ROUTER CATALOG: every tool can render with all required args") {
    // Smoke test: feed each tool a dummy filled-in arg set
    // covering every required field, ensure render succeeds.
    // Catches both missing-required-arg detection and the
    // happy-path template substitution in one shot
    for tool in AiRouterTools.all {
        var args: [String: String] = [:]
        for (key, _) in tool.parameters.properties {
            args[key] = "dummy"
        }
        let call = ToolCall(toolName: tool.name, arguments: args)
        switch AiRouterTools.render(call) {
        case .success: break
        case .failure(let e):
            expect(false, "\(tool.name) render failed: \(e)")
        }
    }
}

runGroup("ROUTER: render survives args with template-like braces") {
    // Adversarial: the model returns argument values that
    // happen to contain `{placeholder}`-shaped substrings.
    // Naive `.replacingOccurrences` would expand those on a
    // second pass and corrupt keyword form. Verify the
    // current implementation handles this correctly (it
    // doesn't loop on placeholders, so braces in values
    // pass through unchanged)
    let call = ToolCall(
        toolName: "calc__evaluate",
        arguments: ["expression": "{noop}*2"]
    )
    switch AiRouterTools.render(call) {
    case .success(let s):
        expect(s == "{noop}*2",
            "args with braces pass through unchanged, got `\(s)`")
    case .failure(let e):
        expect(false, "render failed: \(e)")
    }
}

runGroup("ROUTER: render ignores extra unused arguments") {
    // Some models return more arguments than declared in
    // schema. We render with the declared ones only -
    // extras are silently dropped, the model's overreach
    // doesn't break dispatch
    let call = ToolCall(
        toolName: "calc__evaluate",
        arguments: [
            "expression": "12 * 7",
            "stray_extra": "ignored",
            "another_one": "also ignored",
        ]
    )
    switch AiRouterTools.render(call) {
    case .success(let s):
        expect(s == "12 * 7",
            "extras dropped, got `\(s)`")
    case .failure(let e):
        expect(false, "render failed: \(e)")
    }
}

runGroup("ROUTER: stringify handles boolean, negative, edge numbers") {
    // Edge cases that would have broken naive Double
    // detection. We dont currently have a tool that takes
    // a Bool, but parser callers might receive any JSON
    // primitive - must produce SOMETHING printable
    expect(AiRouter.stringify(NSNumber(value: -42)) == "-42",
        "negative integer collapses without `.0`")
    expect(AiRouter.stringify(NSNumber(value: -3.14)) == "-3.14",
        "negative double preserves precision")
    expect(AiRouter.stringify(NSNumber(value: 0)) == "0", "zero")
    expect(AiRouter.stringify(NSNumber(value: 1.5e10)) == "15000000000",
        "1.5e10 in integer range")
    // Bool is bridged to NSNumber as 0/1 - that's actually
    // the right behaviour for our use case (template
    // substitutes "1" or "0")
    expect(AiRouter.stringify(NSNumber(value: true)) == "1",
        "true → 1")
    expect(AiRouter.stringify(NSNumber(value: false)) == "0",
        "false → 0")
}

runGroup("ROUTER: parseOllamaToolCall on empty tool_calls is nil") {
    // Defensive - model returned an empty array instead of
    // omitting field entirely. Treat as "no tool picked."
    let json = """
        {"message": {"tool_calls": []}}
        """.data(using: .utf8)!
    expect(AiRouter.parseOllamaToolCall(data: json) == nil,
        "empty tool_calls → nil")
}

runGroup("ROUTER: parseOllamaToolCall on missing function name is nil") {
    let json = """
        {"message": {"tool_calls": [{"function": {}}]}}
        """.data(using: .utf8)!
    expect(AiRouter.parseOllamaToolCall(data: json) == nil,
        "missing function.name → nil")
}

runGroup("ROUTER: parseOpenAiToolCall on empty choices is nil") {
    let json = """
        {"choices": []}
        """.data(using: .utf8)!
    expect(AiRouter.parseOpenAiToolCall(data: json) == nil,
        "empty choices → nil")
}

runGroup("ROUTER: parseAnthropicToolCall on empty content is nil") {
    let json = """
        {"content": []}
        """.data(using: .utf8)!
    expect(AiRouter.parseAnthropicToolCall(data: json) == nil,
        "empty content → nil")
}

runGroup("ROUTER: parsers reject malformed JSON gracefully") {
    // Garbage bytes shouldn't crash - return nil and let the
    // palette row stay put. Runs all three parsers against
    // intentionally invalid JSON
    let garbage = "not valid json at all".data(using: .utf8)!
    expect(AiRouter.parseOllamaToolCall(data: garbage) == nil,
        "ollama: garbage → nil")
    expect(AiRouter.parseOpenAiToolCall(data: garbage) == nil,
        "openai: garbage → nil")
    expect(AiRouter.parseAnthropicToolCall(data: garbage) == nil,
        "anthropic: garbage → nil")
}

runGroup("ROUTER: asOllamaToolJSON marks all required fields") {
    // For each tool, the emitted JSON's `required` list
    // must equal catalog's `required`. A drift here
    // would let the model omit fields keyword path
    // depends on
    for tool in AiRouterTools.all {
        let j = AiRouter.asOllamaToolJSON(tool)
        let fn = j["function"] as? [String: Any]
        let params = fn?["parameters"] as? [String: Any]
        let required = params?["required"] as? [String] ?? []
        expect(Set(required) == Set(tool.parameters.required),
            "\(tool.name): required mismatch - got \(required)")
    }
}

runGroup("ROUTER: asAnthropicToolJSON omits OpenAI-specific fields") {
    // Anthropic doesn't recognise the `function:` envelope
    // OR the `parameters` field name. Verify our adapter
    // strips both and emits the flat `name + input_schema`
    // shape Anthropic expects
    for tool in AiRouterTools.all {
        let j = AiRouter.asAnthropicToolJSON(tool)
        expect(j["function"] == nil,
            "\(tool.name): no `function` envelope")
        expect(j["parameters"] == nil,
            "\(tool.name): no `parameters` (should be input_schema)")
        expect(j["input_schema"] != nil,
            "\(tool.name): input_schema present")
    }
}
}
