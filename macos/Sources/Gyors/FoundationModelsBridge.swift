// AI-only - collapses to nothing when WITH_AI=0
#if AI
// Thin bridge to Apple's `FoundationModels` framework - on-device
// LLM that ships with Apple Intelligence
//
// Why a separate file: we want this whole module to compile out
// on SDKs that dont include `FoundationModels` (Xcode 15-era
// macOS 14 SDK, CI runners that pin older toolchains). `#if
// canImport` gate makes symbol disappear when framework
// isn't there; AiClient.swift's call site is gated same way so
// nothing else in bundle has to know
//
// What's exposed here is *only* surface AiClient needs:
//   - construct a session, optionally with a system prompt
//   - send a single prompt, await response text
//
// Streaming, tool calls, and structured-output features are
// available in underlying framework but not surfaced - keeping
// contract narrow lets us swap implementation if Apple renames
// or restructures framework without rippling through AiClient

import Foundation

/// OS-agnostic availability summary for Apple Foundation Models.
/// Surfaced to rest of app even on builds where framework can't be
/// imported, so call sites dont need their own `#if canImport`
/// gates
enum FoundationModelsAvailability {
    case available
    /// SDK didn't ship framework (older Xcode / macOS 14 SDK)
    case sdkMissing
    /// Running OS is below macOS 26
    case osTooOld
    /// Mac doesn't support Apple Intelligence (Intel, M1/M2 base,
    /// ...)
    case deviceNotEligible
    /// User hasn't enabled Apple Intelligence in System Settings
    case appleIntelligenceNotEnabled
    /// Hardware OK, AI enabled, but model assets aren't
    /// downloaded yet - first-launch or post-update transient.
    /// Apple downloads in background; usually clears within
    /// minutes
    case modelNotReady
    /// Catch-all for future framework states we dont have an
    /// enum for. Carries framework's own description so we dont
    /// throw away diagnostic information
    case otherUnavailable(String)

    /// User-facing one-liner. Matches actionable language: tells
    /// user *what to do*, not just what failed
    var userMessage: String {
        switch self {
        case .available:
            return "Apple Intelligence is ready"
        case .sdkMissing:
            return "FoundationModels framework not present in this build - set `ai.provider` to ollama / openai / anthropic"
        case .osTooOld:
            return "macOS 26.0+ required for Apple Intelligence - set `ai.provider` to ollama / openai / anthropic"
        case .deviceNotEligible:
            return "This Mac doesn't support Apple Intelligence - set `ai.provider` to ollama / openai / anthropic"
        case .appleIntelligenceNotEnabled:
            return "Apple Intelligence isn't enabled - turn it on in System Settings → Apple Intelligence & Siri, or set `ai.provider` to ollama / openai / anthropic"
        case .modelNotReady:
            return "Apple Intelligence model assets are still downloading - try again in a few minutes, or set `ai.provider` to ollama / openai / anthropic"
        case .otherUnavailable(let reason):
            return "Apple Intelligence unavailable (\(reason)) - set `ai.provider` to ollama / openai / anthropic"
        }
    }
}

#if canImport(FoundationModels)
import FoundationModels

/// Probe Apple Foundation Models without touching model. Used by
/// `AiClient` to decide whether to dispatch through Apple FM or
/// fall back to another provider when Apple FM is unavailable.
/// Check is cheap (no model load) so we re-probe on every send -
/// `availability` flips to `.available` the moment assets finish
/// downloading, no app restart needed
func currentFoundationModelsAvailability() -> FoundationModelsAvailability {
    if #available(macOS 26.0, *) {
        switch SystemLanguageModel.default.availability {
        case .available:
            return .available
        case .unavailable(let reason):
            switch reason {
            case .deviceNotEligible:
                return .deviceNotEligible
            case .appleIntelligenceNotEnabled:
                return .appleIntelligenceNotEnabled
            case .modelNotReady:
                return .modelNotReady
            @unknown default:
                return .otherUnavailable(String(describing: reason))
            }
        @unknown default:
            return .otherUnavailable(String(describing: SystemLanguageModel.default.availability))
        }
    }
    return .osTooOld
}

@available(macOS 26.0, *)
struct FoundationModelsSession {
    private let session: LanguageModelSession

    init(system: String?) {
        // Framework's `LanguageModelSession` takes optional
        // initial instructions. When caller supplies a system
        // prompt (used by AI transforms - summarize, explain,
        // etc.) we forward it so on-device model has same context
        // chat-capable backends would receive
        if let system = system, !system.isEmpty {
            self.session = LanguageModelSession(instructions: system)
        } else {
            self.session = LanguageModelSession()
        }
    }

    /// Send `prompt` and await textual response. Errors thrown
    /// from framework (Apple Intelligence not enabled, model
    /// unavailable, content filter rejection, ...) propagate
    /// as-is so AiClient surfaces a precise diagnostic
    func respond(to prompt: String) async throws -> String {
        let response = try await session.respond(to: prompt)
        return response.content
    }
}
#else
/// Build with no FoundationModels SDK - framework symbols dont
/// exist. Always reports SDK-missing reason; AiClient falls back
/// to whichever non-Apple provider is configured
func currentFoundationModelsAvailability() -> FoundationModelsAvailability {
    .sdkMissing
}
#endif

#endif // AI
