import Foundation

/// AI router - provider response parsers + tool-spec JSON shapes
func runAiRouterParsersTests() {
runGroup("ROUTER: tool catalog has unique names") {
    // Catalog stability - duplicate names would let one tool's
    // template overwrite another at lookup, silently breaking
    // routing for whichever tool registered later
    var seen: Set<String> = []
    for tool in AiRouterTools.all {
        expect(seen.insert(tool.name).inserted,
            "duplicate tool name: \(tool.name)")
    }
}

runGroup("ROUTER: render fills template from arguments") {
    let call = ToolCall(
        toolName: "currency__convert",
        arguments: ["amount": "100", "from": "USD", "to": "EUR"]
    )
    switch AiRouterTools.render(call) {
    case .success(let s):
        expect(s == "100 USD in EUR",
            "rendered keyword form, got `\(s)`")
    case .failure(let e):
        expect(false, "render failed: \(e)")
    }
}

runGroup("ROUTER: render fails on missing required arg") {
    // Hallucination guard - the model omitted a required
    // parameter. We refuse rather than render a half-formed
    // query that would fire the wrong provider
    let call = ToolCall(
        toolName: "currency__convert",
        arguments: ["amount": "100", "from": "USD"] // missing `to`
    )
    switch AiRouterTools.render(call) {
    case .success(let s):
        expect(false, "expected failure, got `\(s)`")
    case .failure(let e):
        expect(e == .missingArgument("to"),
            "expected missingArgument(to), got \(e)")
    }
}

runGroup("ROUTER: render rejects unknown tool name") {
    let call = ToolCall(
        toolName: "imaginary__action",
        arguments: ["x": "y"]
    )
    switch AiRouterTools.render(call) {
    case .success: expect(false, "should not succeed")
    case .failure(let e):
        expect(e == .unknownTool("imaginary__action"),
            "expected unknownTool, got \(e)")
    }
}

// Regex tool's compound-template test (`re \d+ :: abc`)
// used to live here. Removed alongside the tool itself - the
// router was hallucinating regex calls on free-form questions.
// The new "AiRouterTools.* regex tool" group below pins the
// catalog so it can't quietly grow back.

runGroup("ROUTER: parseOllamaToolCall on valid response") {
    let json = """
        {
          "message": {
            "tool_calls": [{
              "function": {
                "name": "currency__convert",
                "arguments": {"amount": 100, "from": "USD", "to": "EUR"}
              }
            }]
          }
        }
        """.data(using: .utf8)!
    guard let call = AiRouter.parseOllamaToolCall(data: json) else {
        expect(false, "expected ToolCall, got nil"); return
    }
    expect(call.toolName == "currency__convert", "tool name")
    expect(call.arguments["amount"] == "100", "amount stringified")
    expect(call.arguments["from"] == "USD", "from")
    expect(call.arguments["to"] == "EUR", "to")
}

runGroup("ROUTER: parseOllamaToolCall accepts JSON-string args") {
    // Some quantised / older Ollama models return `arguments`
    // as a JSON-encoded STRING rather than an object. We accept
    // both forms - the parser does a second JSON decode for
    // the string variant
    let json = """
        {
          "message": {
            "tool_calls": [{
              "function": {
                "name": "calc__evaluate",
                "arguments": "{\\"expression\\": \\"12 * 7\\"}"
              }
            }]
          }
        }
        """.data(using: .utf8)!
    guard let call = AiRouter.parseOllamaToolCall(data: json) else {
        expect(false, "expected ToolCall, got nil"); return
    }
    expect(call.toolName == "calc__evaluate", "tool")
    expect(call.arguments["expression"] == "12 * 7", "expression decoded")
}

runGroup("ROUTER: parseOllamaToolCall rejects unknown tool") {
    // Model hallucinates a non-existent tool. We silently
    // drop it rather than passing a bogus pick downstream
    let json = """
        {
          "message": {
            "tool_calls": [{
              "function": {
                "name": "delete_everything",
                "arguments": {}
              }
            }]
          }
        }
        """.data(using: .utf8)!
    expect(AiRouter.parseOllamaToolCall(data: json) == nil,
        "unknown tool dropped")
}

runGroup("ROUTER: parseOpenAiToolCall on valid response") {
    // OpenAI's contract: arguments come back as a JSON-encoded
    // string, not an object. The parser does second decode
    let json = """
        {
          "choices": [{
            "message": {
              "tool_calls": [{
                "function": {
                  "name": "currency__convert",
                  "arguments": "{\\"amount\\": 100, \\"from\\": \\"USD\\", \\"to\\": \\"EUR\\"}"
                }
              }]
            }
          }]
        }
        """.data(using: .utf8)!
    guard let call = AiRouter.parseOpenAiToolCall(data: json) else {
        expect(false, "expected ToolCall, got nil"); return
    }
    expect(call.toolName == "currency__convert", "tool name")
    expect(call.arguments["amount"] == "100", "amount stringified")
    expect(call.arguments["from"] == "USD", "from")
    expect(call.arguments["to"] == "EUR", "to")
}

runGroup("ROUTER: parseOpenAiToolCall on text-only response is nil") {
    // No tool_calls array - model returned plain text (declined)
    let json = """
        {"choices": [{"message": {"content": "I don't think any tool fits."}}]}
        """.data(using: .utf8)!
    expect(AiRouter.parseOpenAiToolCall(data: json) == nil,
        "text-only response yields nil")
}

runGroup("ROUTER: parseOpenAiToolCall rejects unknown tool") {
    let json = """
        {
          "choices": [{
            "message": {
              "tool_calls": [{
                "function": {
                  "name": "delete_everything",
                  "arguments": "{}"
                }
              }]
            }
          }]
        }
        """.data(using: .utf8)!
    expect(AiRouter.parseOpenAiToolCall(data: json) == nil,
        "unknown tool dropped")
}

runGroup("ROUTER: parseOpenAiToolCall handles malformed args string") {
    // OpenAI mostly returns valid JSON, but defensively:
    // Unparseable arguments string -> drop call rather
    // than render with garbage data
    let json = """
        {
          "choices": [{
            "message": {
              "tool_calls": [{
                "function": {
                  "name": "calc__evaluate",
                  "arguments": "not even json"
                }
              }]
            }
          }]
        }
        """.data(using: .utf8)!
    expect(AiRouter.parseOpenAiToolCall(data: json) == nil,
        "malformed args yield nil")
}

runGroup("ROUTER: parseAnthropicToolCall on valid response") {
    // Anthropic's tool_use block embeds `input` as an object
    // directly (no double-encoded string). Parser just walks
    // the content array for the first tool_use
    let json = """
        {
          "content": [
            {"type": "text", "text": "Let me convert that."},
            {
              "type": "tool_use",
              "id": "toolu_01abc",
              "name": "currency__convert",
              "input": {"amount": 100, "from": "USD", "to": "EUR"}
            }
          ]
        }
        """.data(using: .utf8)!
    guard let call = AiRouter.parseAnthropicToolCall(data: json) else {
        expect(false, "expected ToolCall, got nil"); return
    }
    expect(call.toolName == "currency__convert", "tool name")
    expect(call.arguments["amount"] == "100", "amount stringified")
    expect(call.arguments["from"] == "USD", "from")
    expect(call.arguments["to"] == "EUR", "to")
}

runGroup("ROUTER: parseAnthropicToolCall on text-only response is nil") {
    let json = """
        {
          "content": [
            {"type": "text", "text": "Nothing matches that."}
          ]
        }
        """.data(using: .utf8)!
    expect(AiRouter.parseAnthropicToolCall(data: json) == nil,
        "text-only response yields nil")
}

runGroup("ROUTER: parseAnthropicToolCall rejects unknown tool") {
    let json = """
        {
          "content": [
            {"type": "tool_use", "name": "imaginary", "input": {}}
          ]
        }
        """.data(using: .utf8)!
    expect(AiRouter.parseAnthropicToolCall(data: json) == nil,
        "unknown tool dropped")
}

runGroup("ROUTER: asAnthropicToolJSON uses input_schema not parameters") {
    // Anthropic spec quirk - verify we emit `input_schema`
    // (their wire name) rather than OpenAI's `parameters`
    let tool = AiRouterTools.all.first { $0.name == "calc__evaluate" }!
    let j = AiRouter.asAnthropicToolJSON(tool)
    expect(j["name"] as? String == "calc__evaluate", "name at top level")
    expect(j["input_schema"] != nil, "input_schema present")
    expect(j["parameters"] == nil, "no `parameters` field")
    let schema = j["input_schema"] as? [String: Any]
    expect((schema?["type"] as? String) == "object", "schema type=object")
    expect((schema?["required"] as? [String])?.contains("expression") == true,
        "required list includes expression")
}

runGroup("ROUTER: parseOllamaToolCall on text-only response is nil") {
    // Model declined to pick a tool - no `tool_calls` array
    let json = """
        {
          "message": {
            "content": "I don't think any tool fits."
          }
        }
        """.data(using: .utf8)!
    expect(AiRouter.parseOllamaToolCall(data: json) == nil,
        "text-only response yields nil")
}

runGroup("ROUTER: stringify renders integral doubles without .0") {
    // Currency / units expect `100` not `100.0` when the
    // amount is integral. JSON-Number boxing in Swift gives
    // us a Double; collapse the trailing zero
    expect(AiRouter.stringify(NSNumber(value: 100.0)) == "100",
        "100.0 → 100")
    expect(AiRouter.stringify(NSNumber(value: 99.5)) == "99.5",
        "99.5 → 99.5")
    expect(AiRouter.stringify("hello") == "hello", "string passes through")
}

runGroup("ROUTER: asOllamaToolJSON shape matches OpenAI tools spec") {
    let tool = AiRouterTools.all.first { $0.name == "calc__evaluate" }!
    let j = AiRouter.asOllamaToolJSON(tool)
    expect((j["type"] as? String) == "function", "type=function")
    let fn = j["function"] as? [String: Any]
    expect(fn?["name"] as? String == "calc__evaluate", "name")
    let params = fn?["parameters"] as? [String: Any]
    expect((params?["type"] as? String) == "object", "params type")
    expect((params?["required"] as? [String])?.contains("expression") == true,
        "expression required")
    let props = params?["properties"] as? [String: Any]
    expect(props?["expression"] != nil, "expression property")
}

runGroup("ROUTER: config off by default, on when explicitly enabled") {
    // Lives under the `ai.*` namespace so it shares JSON
    // ground with `ai.provider`, `ai.model` etc. - same
    // routing behaviour, same config block
    let off = Config(
        hotkey: nil, ai: nil, theme: nil, notesFolder: nil,
        previewMarkdown: nil, previewSyntax: nil,
        terminalApp: nil,
        panelOpacity: nil, panelBlur: nil
    )
    expect(!off.effectiveRouterEnabled,
        "router default OFF when ai block missing entirely")

    let aiNoRouter = AiConfig(routerEnabled: nil)
    let offWithAi = Config(
        hotkey: nil, ai: aiNoRouter, theme: nil, notesFolder: nil,
        previewMarkdown: nil, previewSyntax: nil,
        terminalApp: nil,
        panelOpacity: nil, panelBlur: nil
    )
    expect(!offWithAi.effectiveRouterEnabled,
        "router default OFF when ai.router_enabled is unset")

    let aiOn = AiConfig(routerEnabled: LooseBool(true))
    let on = Config(
        hotkey: nil, ai: aiOn, theme: nil, notesFolder: nil,
        previewMarkdown: nil, previewSyntax: nil,
        terminalApp: nil,
        panelOpacity: nil, panelBlur: nil
    )
    expect(on.effectiveRouterEnabled, "explicit true → on")

    let aiOff = AiConfig(routerEnabled: LooseBool(false))
    let off2 = Config(
        hotkey: nil, ai: aiOff, theme: nil, notesFolder: nil,
        previewMarkdown: nil, previewSyntax: nil,
        terminalApp: nil,
        panelOpacity: nil, panelBlur: nil
    )
    expect(!off2.effectiveRouterEnabled, "explicit false → off")
}
}
