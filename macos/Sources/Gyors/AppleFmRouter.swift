// AI-only - collapses to nothing when WITH_AI=0
#if AI
// Apple Foundation Models adapter for AI router
//
// No `@Generable` macros - they require `FoundationModelsMacros`
// plugin which only ships with full Xcode (not Command Line
// Tools we build with). Instead we build each tool's argument
// schema at runtime via `DynamicGenerationSchema`, declare
// `Arguments = GeneratedContent` (framework's already-conformant
// untyped payload type), and extract typed values from
// GeneratedContent in `call()` by property name
//
// Trade-offs vs macro path:
// - PRO: builds with Command Line Tools alone, so CI / fresh
//   developer machines dont need an Xcode install.
// - PRO: tool catalog stays data-driven - to add a tool, append
//   to `AiRouterTools.all` and register a switch arm in
//   `appleFmTool(named:)`. No new typed struct per tool.
// - CON: argument access is `arguments.value(T.self, forProperty:)`
//   rather than macro-derived `arguments.amount`. A few extra
//   characters at every read site, in exchange for never running
//   macro plugin
//
// All tool descriptions are copied verbatim from
// `AiRouterTools.swift` so model sees same prompt regardless of
// which backend dispatched call. If those drift, model's
// accuracy on Apple FM will drift too - keep them in sync

import Foundation

#if canImport(FoundationModels)
import FoundationModels

/// Sentinel thrown from a tool's `call()` after the args are
/// captured. Halts generation immediately so we dont waste a
/// final natural-language response we'd discard anyway
struct RouterStop: Error {}

/// Holds first tool call model fires during a router round-trip.
/// Actor for thread safety - framework can dispatch `call()` from
/// its own scheduler and we read captured value back from caller's
/// task
@available(macOS 26.0, *)
actor RouterCapture {
    private(set) var captured: ToolCall?
    func set(_ call: ToolCall) { captured = call }
}

@available(macOS 26.0, *)
func runAppleFmRouter(question: String, tools: [ToolSpec]) async -> ToolCall? {
    let capture = RouterCapture()
    let appleTools: [any Tool] = tools.compactMap { spec in
        appleFmTool(named: spec.name, capture: capture)
    }
    guard !appleTools.isEmpty else {
        return nil
    }

    let session = LanguageModelSession(
        tools: appleTools,
        instructions: AiRouter.systemPrompt
    )
    do {
        _ = try await session.respond(to: question)
    } catch {
        // Apple FM's framework WRAPS our `RouterStop` throw in a
        // `LanguageModelSession.ToolCallError` before propagating
        // it. Wrapping shape isn't part of public API surface in a
        // way we can match across SDK versions, so use
        // capture box as actual success signal: if
        // `capture.captured` is non-nil, our tool's `call()` fired
        // and recorded args. Error itself is incidental
        let probe = await capture.captured
        if probe == nil {
            return nil
        }
    }
    return await capture.captured
}

/// Tool registry. Internal (not private) so tests can ask for a
/// tool by name and verify framework accepts schema - `parameters`
/// lazily calls into `makeArgsSchema`, so just touching it from a
/// test is a real schema-construction smoke test on every tool
@available(macOS 26.0, *)
func appleFmTool(named name: String, capture: RouterCapture) -> (any Tool)? {
    switch name {
    case "currency__convert": return AppleFmCurrencyTool(capture: capture)
    case "units__convert":    return AppleFmUnitsTool(capture: capture)
    case "calc__evaluate":    return AppleFmCalcTool(capture: capture)
    case "regex__test":       return AppleFmRegexTool(capture: capture)
    default:                  return nil
    }
}


/// Build a `GenerationSchema` from a flat list of (name, type,
/// description) tuples. `T` is property's primitive Swift type -
/// `Double.self`, `String.self`, etc. Apple FM's
/// `DynamicGenerationSchema(type:)` accepts any `Generable`, which
/// all primitives we use here already conform to
@available(macOS 26.0, *)
private func makeArgsSchema(name: String, description: String?, properties: [SchemaProperty]) -> GenerationSchema {
    let dyn = DynamicGenerationSchema(
        name: name,
        description: description,
        properties: properties.map { p in
            DynamicGenerationSchema.Property(
                name: p.name,
                description: p.description,
                schema: p.schema,
                isOptional: false
            )
        }
    )
    // `try!` - only documented failure modes for this conversion
    // (`SchemaError.duplicateProperty`, `.undefinedReferences`,
    // `.duplicateType`, `.emptyTypeChoices`) all describe
    // construction-time programmer errors, not runtime conditions.
    // A duplicate property name in our hardcoded catalogue would be
    // a compile-time-style bug; we want it to crash loudly during
    // development rather than silently degrade routing
    return try! GenerationSchema(root: dyn, dependencies: [])
}

@available(macOS 26.0, *)
private struct SchemaProperty {
    let name: String
    let description: String?
    let schema: DynamicGenerationSchema
}

@available(macOS 26.0, *)
private extension SchemaProperty {
    static func string(_ name: String, _ description: String) -> SchemaProperty {
        .init(
            name: name,
            description: description,
            schema: DynamicGenerationSchema(type: String.self)
        )
    }
    static func double(_ name: String, _ description: String) -> SchemaProperty {
        .init(
            name: name,
            description: description,
            schema: DynamicGenerationSchema(type: Double.self)
        )
    }
}


@available(macOS 26.0, *)
private struct AppleFmCurrencyTool: Tool {
    typealias Arguments = GeneratedContent
    typealias Output = String

    let capture: RouterCapture
    var name: String { "currency__convert" }
    var description: String {
        "Convert an amount of money between two currencies."
    }

    var parameters: GenerationSchema {
        makeArgsSchema(name: "CurrencyArgs", description: nil, properties: [
            .double("amount", "Numeric amount to convert."),
            .string("from", "Source currency. ISO 4217 code (USD, EUR, …) or common name."),
            .string("to", "Target currency. ISO 4217 code (USD, EUR, …) or common name."),
        ])
    }

    func call(arguments: GeneratedContent) async throws -> String {
        let amount = try arguments.value(Double.self, forProperty: "amount")
        let from = try arguments.value(String.self, forProperty: "from")
        let to = try arguments.value(String.self, forProperty: "to")
        await capture.set(ToolCall(
            toolName: name,
            arguments: [
                "amount": AiRouter.stringify(amount as NSNumber),
                "from": from,
                "to": to,
            ]
        ))
        throw RouterStop()
    }
}

@available(macOS 26.0, *)
private struct AppleFmUnitsTool: Tool {
    typealias Arguments = GeneratedContent
    typealias Output = String

    let capture: RouterCapture
    var name: String { "units__convert" }
    var description: String {
        "Convert a quantity between two units of measure (length, mass, temperature, time, energy, etc)."
    }

    var parameters: GenerationSchema {
        makeArgsSchema(name: "UnitsArgs", description: nil, properties: [
            .double("amount", "Numeric amount to convert."),
            .string("from", "Source unit, e.g. km, miles, kg, fahrenheit."),
            .string("to", "Target unit, e.g. mi, km, lb, celsius."),
        ])
    }

    func call(arguments: GeneratedContent) async throws -> String {
        let amount = try arguments.value(Double.self, forProperty: "amount")
        let from = try arguments.value(String.self, forProperty: "from")
        let to = try arguments.value(String.self, forProperty: "to")
        await capture.set(ToolCall(
            toolName: name,
            arguments: [
                "amount": AiRouter.stringify(amount as NSNumber),
                "from": from,
                "to": to,
            ]
        ))
        throw RouterStop()
    }
}

@available(macOS 26.0, *)
private struct AppleFmCalcTool: Tool {
    typealias Arguments = GeneratedContent
    typealias Output = String

    let capture: RouterCapture
    var name: String { "calc__evaluate" }
    var description: String {
        "Evaluate a maths expression. Supports +, -, *, /, %, parentheses, sqrt, sin/cos/tan, log, etc."
    }

    var parameters: GenerationSchema {
        makeArgsSchema(name: "CalcArgs", description: nil, properties: [
            .string("expression", "The maths expression to evaluate, e.g. '12 * 7 + 3' or 'sqrt(2)'."),
        ])
    }

    func call(arguments: GeneratedContent) async throws -> String {
        let expression = try arguments.value(String.self, forProperty: "expression")
        await capture.set(ToolCall(
            toolName: name,
            arguments: ["expression": expression]
        ))
        throw RouterStop()
    }
}

@available(macOS 26.0, *)
private struct AppleFmRegexTool: Tool {
    typealias Arguments = GeneratedContent
    typealias Output = String

    let capture: RouterCapture
    var name: String { "regex__test" }
    var description: String {
        "Test a regular expression against sample text. Use when the user wants to validate a pattern or extract matches."
    }

    var parameters: GenerationSchema {
        makeArgsSchema(name: "RegexArgs", description: nil, properties: [
            .string("pattern", "The regular expression to test."),
            .string("text", "Sample text to match against."),
        ])
    }

    func call(arguments: GeneratedContent) async throws -> String {
        let pattern = try arguments.value(String.self, forProperty: "pattern")
        let text = try arguments.value(String.self, forProperty: "text")
        await capture.set(ToolCall(
            toolName: name,
            arguments: [
                "pattern": pattern,
                "text": text,
            ]
        ))
        throw RouterStop()
    }
}

#else

/// Build with no FoundationModels SDK - symbol exists so call
/// sites dont need their own gate, but router falls back to next
/// configured backend automatically
func runAppleFmRouter(question: String, tools: [ToolSpec]) async -> ToolCall? {
    nil
}

#endif

#endif // AI
