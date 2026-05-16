// AI-only - collapses to nothing when WITH_AI=0
#if AI
import Foundation

/// Catalog entry AI router exposes to model. Each tool is a thin
/// description of one of Gyors's existing keyword providers, shaped
/// for LLM's tool-calling API
///
/// Router NEVER produces a candidate row directly - it picks a
/// tool, fills in arguments, and resulting `ToolCall` is rendered
/// back into keyword form (`"100 usd in eur"`) that existing
/// orchestrator already knows how to dispatch. Router is purely a
/// natural-language -> keyword translator. All precision tiers,
/// frecency, action chains, and previews are unchanged
struct ToolSpec: Equatable {
    /// Stable tool name. Convention: `<provider>__<verb>`.
    /// Shows rather than dots so JSON-schema emitters dont
    /// have to escape
    let name: String
    /// One-line, model-facing description. Plain English, no jargon
    let description: String
    /// JSON-schema-shaped argument definition
    let parameters: ToolParameters
    /// Substitution template - `"{amount} {from} in {to}"` ->
    /// `"100 usd in eur"`. Placeholders match
    /// `parameters.properties` keys exactly. Rendered string is
    /// what gets fed back into orchestrator via `Effect::SetInput`
    let template: String
}

struct ToolParameters: Equatable {
    /// Ordered (deterministic for tests) list of typed parameters
    let properties: [(String, ToolParam)]
    /// Names that MUST be supplied by model. Anything missing from
    /// a `ToolCall` fails validation; we dont try to fill in
    /// guesses on model's behalf
    let required: [String]

    static func == (lhs: ToolParameters, rhs: ToolParameters) -> Bool {
        guard lhs.required == rhs.required else { return false }
        guard lhs.properties.count == rhs.properties.count else { return false }
        for (a, b) in zip(lhs.properties, rhs.properties) {
            if a.0 != b.0 || a.1 != b.1 { return false }
        }
        return true
    }
}

struct ToolParam: Equatable {
    enum Kind: String, Equatable {
        case string, number, integer
    }
    let kind: Kind
    let description: String
}

/// Successful router pick: a tool name + concrete argument values.
/// Arguments stored as strings because eventual rendering goes
/// straight into a keyword query - type fidelity beyond "what the
/// user typed" is unnecessary, and forcing every tool to
/// round-trip numbers through JSON-Number's float quirks is more
/// work than it's worth
struct ToolCall: Equatable {
    let toolName: String
    let arguments: [String: String]
}

enum ToolRenderError: Error, Equatable {
    case missingArgument(String)
    case unknownTool(String)
}

/// Hardcoded starter catalog. Read-only tools only - mutating
/// actions (notes, system prefs, window management) want a
/// confirmation row first, which isn't wired yet
enum AiRouterTools {
    static let all: [ToolSpec] = [
        currencyConvert,
        unitConvert,
        calcEvaluate,
    ]

    static func find(named name: String) -> ToolSpec? {
        all.first { $0.name == name }
    }

    /// Render a tool call into keyword query that fires existing
    /// provider. Returns templated string (e.g. `"100 usd in eur"`)
    /// or an error when model omitted a required argument
    static func render(_ call: ToolCall) -> Result<String, ToolRenderError> {
        guard let tool = find(named: call.toolName) else {
            return .failure(.unknownTool(call.toolName))
        }
        var rendered = tool.template
        for (key, _) in tool.parameters.properties {
            let placeholder = "{\(key)}"
            if let value = call.arguments[key] {
                rendered = rendered.replacingOccurrences(of: placeholder, with: value)
            } else if tool.parameters.required.contains(key) {
                return .failure(.missingArgument(key))
            } else {
                // Optional placeholder with no value - strip plus
                // any surrounding whitespace so keyword form
                // doesn't end up with `"100 usd"  -> eur"`
                rendered = rendered.replacingOccurrences(of: " \(placeholder)", with: "")
                rendered = rendered.replacingOccurrences(of: "\(placeholder) ", with: "")
                rendered = rendered.replacingOccurrences(of: placeholder, with: "")
            }
        }
        return .success(rendered.trimmingCharacters(in: .whitespaces))
    }


    private static let currencyConvert = ToolSpec(
        name: "currency__convert",
        description: "Convert an amount of money between two currencies.",
        parameters: ToolParameters(
            properties: [
                ("amount", ToolParam(kind: .number, description: "Numeric amount to convert.")),
                ("from",   ToolParam(kind: .string, description: "Source currency. ISO 4217 code (USD, EUR, …) or common name.")),
                ("to",     ToolParam(kind: .string, description: "Target currency. ISO 4217 code (USD, EUR, …) or common name.")),
            ],
            required: ["amount", "from", "to"]
        ),
        template: "{amount} {from} in {to}"
    )

    private static let unitConvert = ToolSpec(
        name: "units__convert",
        description: "Convert a quantity between two units of measure (length, mass, temperature, time, energy, etc).",
        parameters: ToolParameters(
            properties: [
                ("amount", ToolParam(kind: .number, description: "Numeric amount to convert.")),
                ("from",   ToolParam(kind: .string, description: "Source unit, e.g. km, miles, kg, fahrenheit.")),
                ("to",     ToolParam(kind: .string, description: "Target unit, e.g. mi, km, lb, celsius.")),
            ],
            required: ["amount", "from", "to"]
        ),
        template: "{amount} {from} in {to}"
    )

    private static let calcEvaluate = ToolSpec(
        name: "calc__evaluate",
        description: "Evaluate a maths expression. Supports +, -, *, /, %, parentheses, sqrt, sin/cos/tan, log, etc.",
        parameters: ToolParameters(
            properties: [
                ("expression", ToolParam(kind: .string, description: "The maths expression to evaluate, e.g. '12 * 7 + 3' or 'sqrt(2)'."))
            ],
            required: ["expression"]
        ),
        template: "{expression}"
    )

}

#endif // AI
