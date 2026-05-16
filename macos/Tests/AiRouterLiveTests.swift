import Foundation

final class ResultBox<T> {
    var value: T?
}

/// AI router - live Ollama integration tests (skip when no local Ollama)
func runAiRouterLiveTests() {
//
// These tests hit a real local Ollama if one's running on
// default port. They probe reachability synchronously
// before doing any work; if Ollama isn't up, test
// emits a skip line (silent green) instead of failing -
// most contributors dont have Ollama installed and we
// dont want to make the suite require it

runGroup("OLLAMA LIVE: reachability probe is fast") {
    // Make sure the probe itself never hangs the suite,
    // even when Ollama isn't installed and the kernel has
    // to wait the full TCP timeout
    let start = Date()
    _ = ollamaIsReachable()
    let elapsed = Date().timeIntervalSince(start)
    expect(elapsed < 3.0,
        "probe returned in <3s, took \(elapsed)s")
}

runGroup("OLLAMA LIVE: router routes natural-language to a tool") {
    guard ollamaIsReachable() else {
        expect(true, "skipped: Ollama not running on localhost:11434")
        return
    }
    // Pick whatever model user has installed first;
    // most likely something tool-capable since that's the
    // user's reason to have Ollama at all. If none of the
    // known good tool-callers are installed, fall back to
    // the first available - the assertion is permissive
    // (any catalog tool, OR nil decline) so weaker models
    // dont fail test, only verify integration
    // doesn't crash
    let installed = ollamaInstalledModels()
    guard let model = pickToolCapableModel(from: installed) else {
        expect(true, "skipped: Ollama is up but no models installed")
        return
    }

    let cfg = AiConfig(provider: "ollama", model: model)
    let semaphore = DispatchSemaphore(value: 0)
    let box = ResultBox<ToolCall>()
    Task {
        box.value = await AiRouter.routeViaOllama(
            question: "convert 100 US dollars to euros",
            tools: AiRouterTools.all,
            config: cfg
        )
        semaphore.signal()
    }
    // Local LLM call - generous ceiling for cold loads
    let outcome = semaphore.wait(timeout: .now() + 60)
    expect(outcome == .success,
        "completed within 60s using model \(model)")

    // Permissive shape check: either nil (model declined
    // or doesn't tool-call) or a valid catalog tool.
    // Hallucinated tool names MUST NOT pass the parser
    // (it rejects unknown names), so any non-nil result
    // is by definition a real tool
    if let r = box.value {
        let knownNames = AiRouterTools.all.map { $0.name }
        expect(knownNames.contains(r.toolName),
            "if non-nil, tool is in catalog (got `\(r.toolName)` from \(model))")
    }
}

runGroup("OLLAMA LIVE: parser tolerates real-world Ollama responses") {
    guard ollamaIsReachable() else {
        expect(true, "skipped: Ollama not running")
        return
    }
    let installed = ollamaInstalledModels()
    guard let model = pickToolCapableModel(from: installed) else {
        expect(true, "skipped: no models installed")
        return
    }

    // Send several queries shaped for different tools.
    // Point isn't that every query routes correctly
    // (small models miss frequently) - it's that the
    // parser handles whatever shape Ollama sends back
    // without crashing or returning bogus rows
    let prompts = [
        "convert 50 dollars to pounds",
        "5 km in miles",
        "what is 12 times seven",
        "regex \\d+ against abc123",
    ]
    let cfg = AiConfig(provider: "ollama", model: model)
    for prompt in prompts {
        let semaphore = DispatchSemaphore(value: 0)
        let box = ResultBox<ToolCall>()
        Task {
            box.value = await AiRouter.routeViaOllama(
                question: prompt, tools: AiRouterTools.all, config: cfg
            )
            semaphore.signal()
        }
        _ = semaphore.wait(timeout: .now() + 60)
        if let r = box.value {
            // If model picked something, it must be
            // a real tool. Schema validation drops
            // hallucinations; we mirror that here as a
            // belt-and-braces
            let knownNames = AiRouterTools.all.map { $0.name }
            expect(knownNames.contains(r.toolName),
                "`\(prompt)` → `\(r.toolName)` is a real tool")
            // Args are typed strings, never empty for
            // required keys - verify if the tool was a
            // hit on something we know required-args for
            if let spec = AiRouterTools.find(named: r.toolName) {
                for required in spec.parameters.required {
                    // The render step would catch missing
                    // required args, but parser shouldn't
                    // hand back a ToolCall with completely
                    // missing values either. Empty string
                    // IS allowed (renders as blank)
                    expect(r.arguments[required] != nil,
                        "required `\(required)` present for `\(prompt)`")
                }
            }
        }
        // Nil result is acceptable - model declined
    }
}
}
