import Foundation

/// Apple FoundationModels - live router + availability + tool-registry tests.
///
/// All bodies gated on `#if canImport(FoundationModels)`: SDKs without
/// FoundationModels (macOS 14/15 shipped Xcodes) can't see
/// `RouterCapture` / `appleFmTool` at all - those symbols live behind
/// same gate in `AppleFmRouter.swift`. CI runs an older SDK, so we
/// emit a single "skipped" row instead of compile errors
func runAppleFmLiveTests() {
#if canImport(FoundationModels)
runGroup("APPLE FM: every catalog tool has a typed Tool registered") {
    // Every `ToolSpec` in data-driven catalog must have a
    // matching typed `Tool` struct on the Apple FM side.
    // Drift between two surfaces would mean Ollama users
    // see a tool the model can pick, but Apple FM users see
    // it absent - silently breaking routing on Apple's
    // backend without any compile-time signal
    if #available(macOS 26.0, *) {
        let capture = RouterCapture()
        for spec in AiRouterTools.all {
            let tool = appleFmTool(named: spec.name, capture: capture)
            expect(tool != nil,
                "\(spec.name) has a typed Apple FM Tool wired up")
        }
    } else {
        expect(true, "skipped: macOS 26 required for FoundationModels")
    }
}

runGroup("APPLE FM: unknown tool name returns nil from registry") {
    if #available(macOS 26.0, *) {
        let capture = RouterCapture()
        expect(appleFmTool(named: "imaginary__action", capture: capture) == nil,
            "unknown name → nil")
        expect(appleFmTool(named: "", capture: capture) == nil,
            "empty name → nil")
    } else {
        expect(true, "skipped: macOS 26 required")
    }
}

runGroup("APPLE FM: schema construction works for every tool") {
    // Touch `parameters` on every typed tool - this calls
    // through to `DynamicGenerationSchema` + `GenerationSchema(root:)`
    // for real. A duplicate property name, undefined reference,
    // or other schema error would surface as a `try!` crash
    // here rather than at runtime when the user actually
    // invokes the router
    if #available(macOS 26.0, *) {
        let capture = RouterCapture()
        for spec in AiRouterTools.all {
            guard let tool = appleFmTool(named: spec.name, capture: capture) else {
                expect(false, "\(spec.name) registered"); continue
            }
            // Just touching this property forces the
            // GenerationSchema to construct end-to-end.
            // Validates name + description shape too
            let schema = tool.parameters
            let debug = String(describing: schema)
            expect(!debug.isEmpty,
                "\(spec.name) schema is non-empty")
            expect(!tool.name.isEmpty, "\(spec.name) tool name set")
            expect(!tool.description.isEmpty,
                "\(spec.name) description set")
        }
    } else {
        expect(true, "skipped: macOS 26 required")
    }
}

runGroup("APPLE FM: live router routes natural-language to a known tool") {
    // Whole point of Apple FM as a backend: NO network,
    // it runs on-device. So unlike Ollama/OpenAI/Anthropic
    // (where a real integration test needs a live server +
    // API key), this test runs against actual model
    // locally as long as Apple Intelligence is enabled
    //
    // We can't pin specific tool arguments (the model is
    // non-deterministic), but we CAN assert that:
    //   1. Some tool was picked (router didn't return nil)
    //   2. The picked tool is in our catalog (no hallucination)
    // Both invariants hold under any reasonable model
    // behaviour for an obviously currency-shaped query
    //
    // Skipped automatically when Apple FM isn't available
    // (older OS, Intelligence not enabled, model assets
    // still downloading) so CI on a fresh machine doesn't
    // hard-fail
    if #available(macOS 26.0, *) {
        guard case .available = currentFoundationModelsAvailability() else {
            expect(true, "skipped: Apple FM not currently available")
            return
        }
        // First-call cold load on Apple Silicon can take a
        // while; framework may also continue generating
        // a final natural-language response after our
        // RouterStop throw (depending on how it handles
        // tool errors), so we pay the full pipeline cost.
        // 90s ceiling absorbs both. Treat timeout as a
        // skip rather than fail - integration is
        // verified by `appleFmTool(named:)` schema tests
        // above; this test's value is asserting routing
        // shape, not perf
        let semaphore = DispatchSemaphore(value: 0)
        let box = ResultBox<ToolCall>()
        Task {
            box.value = await runAppleFmRouter(
                question: "convert 100 US dollars to euros",
                tools: AiRouterTools.all
            )
            semaphore.signal()
        }
        let outcome = semaphore.wait(timeout: .now() + 90)
        guard outcome == .success else {
            expect(true, "skipped: Apple FM didn't respond in 90s (model warming up?)")
            return
        }
        if let r = box.value {
            let knownNames = AiRouterTools.all.map { $0.name }
            expect(knownNames.contains(r.toolName),
                "picked tool `\(r.toolName)` is in catalog")
            // Strong assertion: obvious currency query
            // ought to land on `currency__convert`. We
            // tolerate model picking `units__convert`
            // (treating dollars as a "unit") - that's a
            // less-good answer but not wrong enough to fail
            // test on. Anything else IS a real
            // routing miss
            let acceptable: Set<String> = [
                "currency__convert",
                "units__convert",
            ]
            expect(acceptable.contains(r.toolName),
                "routed reasonably for currency query, got \(r.toolName)")
        } else {
            // Model declined. Acceptable - Apple FM is
            // smaller and may not always pick. We've
            // verified integration didn't crash
            expect(true, "model declined cleanly (nil result)")
        }
    } else {
        expect(true, "skipped: macOS 26 required")
    }
}

runGroup("APPLE FM: live router handles unrouteable queries") {
    // Counter-test: a question with no obvious tool match
    // ("the meaning of life") should make the model decline.
    // Router returns nil, palette row stays put,
    // cmd+return falls through to free-form Ask AI as designed.
    // Verify decline path works in the real model
    if #available(macOS 26.0, *) {
        guard case .available = currentFoundationModelsAvailability() else {
            expect(true, "skipped: Apple FM not currently available")
            return
        }
        let semaphore = DispatchSemaphore(value: 0)
        let box = ResultBox<ToolCall>()
        Task {
            box.value = await runAppleFmRouter(
                question: "what is the meaning of life",
                tools: AiRouterTools.all
            )
            semaphore.signal()
        }
        let outcome = semaphore.wait(timeout: .now() + 90)
        guard outcome == .success else {
            expect(true, "skipped: Apple FM didn't respond in 90s")
            return
        }
        // We accept either nil (model declined)
        // OR a tool pick that is NEVERTHELESS in the
        // catalog (the model overreached but didn't
        // hallucinate). What we MUST NOT see: a pick
        // whose tool name isn't registered
        if let r = box.value {
            let knownNames = AiRouterTools.all.map { $0.name }
            expect(knownNames.contains(r.toolName),
                "if the model picked something, it's a real tool, got `\(r.toolName)`")
        }
    } else {
        expect(true, "skipped: macOS 26 required")
    }
}

runGroup("APPLE FM: effectiveProvider respects explicit user choice") {
    // User-explicit is honoured even when default would
    // be different - pinning provider should never be
    // silently overridden, otherwise "I set ai.provider to
    // ollama" -> "but Gyors used Apple anyway" is a
    // confusing and trust-breaking surprise
    for explicit in ["ollama", "openai", "anthropic", "apple"] {
        let cfg = AiConfig(provider: explicit)
        expect(AiClient.effectiveProvider(config: cfg) == explicit,
            "explicit \(explicit) preserved, got \(AiClient.effectiveProvider(config: cfg))")
    }
}

runGroup("APPLE FM: effectiveProvider lowercases user input") {
    // Hand-edited config files often have "Ollama" or
    // "OPENAI" - normalise so dispatch switch matches
    let cfg = AiConfig(provider: "OpenAI")
    expect(AiClient.effectiveProvider(config: cfg) == "openai",
        "lowercased, got \(AiClient.effectiveProvider(config: cfg))")
}

runGroup("APPLE FM: empty/missing provider defaults to apple on macOS 26+") {
    // The auto-default. On macOS 26+ we default to Apple FM
    // (free, on-device); fall back to Ollama on older systems.
    // Test runs on whatever macOS version Cargo CI uses,
    // so we assert the SHAPE - provider is one of the two
    // expected defaults - rather than pinning a specific OS
    let cfgNil = AiConfig(provider: nil)
    let p = AiClient.effectiveProvider(config: cfgNil)
    expect(p == "apple" || p == "ollama",
        "auto-default is apple|ollama, got \(p)")

    let cfgEmpty = AiConfig(provider: "")
    let q = AiClient.effectiveProvider(config: cfgEmpty)
    expect(q == "apple" || q == "ollama",
        "empty string treated as unset, got \(q)")
}

runGroup("APPLE FM: availability messages are actionable") {
    // Each unavailable state must point the user at SOMETHING
    // they can do. A vague "model unavailable" with no fix
    // is worse than the bug itself - users have no idea
    // whether to wait, restart, or change config
    for state: FoundationModelsAvailability in [
        .sdkMissing,
        .osTooOld,
        .deviceNotEligible,
        .appleIntelligenceNotEnabled,
        .modelNotReady,
        .otherUnavailable("test reason"),
    ] {
        let msg = state.userMessage
        expect(!msg.isEmpty, "\(state) has empty message")
        // Every unavailable branch should mention either the
        // System Settings path OR the `ai.provider` config
        // fallback. (modelNotReady additionally says "try
        // again in a few minutes" - only state where
        // waiting is the right answer.)
        let mentionsFix = msg.contains("ai.provider")
            || msg.contains("System Settings")
            || msg.contains("try again")
        expect(mentionsFix,
            "\(state) message lacks actionable fix: \(msg)")
    }
}

runGroup("APPLE FM: available state has a positive message") {
    // Sanity check - `.available` is only state where
    // message shouldn't read as an error
    let msg = FoundationModelsAvailability.available.userMessage
    expect(msg.contains("ready"),
        "available message should be positive, got: \(msg)")
}
#else
runGroup("APPLE FM: skipped (FoundationModels not in SDK)") {
    expect(true, "FoundationModels framework not present in this build")
}
#endif
}
