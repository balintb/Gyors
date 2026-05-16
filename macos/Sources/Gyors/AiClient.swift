// Built only when WITH_AI=1 (default). Collapsed to nothing in an
// AI-stripped build so launcher ships with no AI client, no
// OpenAI/Anthropic/Ollama wiring, and no API key handling
#if AI
import Foundation

enum AiError: Error, LocalizedError {
    case missingApiKey(provider: String)
    case unknownProvider(String)
    case badUrl(String)
    case httpError(Int, String)
    case decodeError(String)
    case ollamaUnreachable
    case appleUnavailable(reason: String)

    var errorDescription: String? {
        switch self {
        case .missingApiKey(let p):
            return "Missing API key for provider '\(p)'. Add `ai.api_key` to config.json."
        case .unknownProvider(let p):
            return "Unknown AI provider '\(p)'. Use 'ollama', 'openai', 'anthropic', or 'apple'."
        case .badUrl(let u):
            return "Invalid URL: \(u)"
        case .httpError(let code, let body):
            let snippet = body.prefix(400)
            return "HTTP \(code): \(snippet)"
        case .decodeError(let msg):
            return "Couldn't parse response: \(msg)"
        case .ollamaUnreachable:
            return "Can't reach Ollama at the configured endpoint. Is `ollama serve` running?"
        case .appleUnavailable(let reason):
            return "Apple Foundation Models not available - \(reason). Set `ai.provider` to ollama / openai / anthropic in config.json."
        }
    }
}

/// Handles requests to user's configured AI provider
///
/// Default: Ollama on `http://localhost:11434` with model `llama3.2`.
/// Configure via `config.json`'s `ai` block:
///
/// ```json
/// {
///   "ai": {
///     "provider": "ollama" | "openai" | "anthropic",
///     "model":    "...",
///     "endpoint": "..."     (optional, per-provider default)
///     "api_key":  "..."     (required for openai/anthropic)
///   }
/// }
/// ```
enum AiClient {
    static func ask(_ question: String) async throws -> String {
        try await dispatch(userContent: question, system: nil)
    }

    /// Transform `text` according to `instruction`. Instruction
    /// becomes system prompt for chat-capable backends; for Ollama
    /// (which /api/generate doesn't split), we merge into a single
    /// prompt so small local models still behave
    static func transform(text: String, instruction: String) async throws -> String {
        try await dispatch(userContent: text, system: instruction)
    }

    private static func dispatch(userContent: String, system: String?) async throws -> String {
        let config = Config.load().ai ?? AiConfig()
        let provider = effectiveProvider(config: config)
        // When we DEFAULTED to Apple (no `ai.provider` set, OS supports
        // it) but assets aren't ready / Apple Intelligence isn't
        // enabled, silently fall back to Ollama. Default-to-Apple
        // promise was "works out of the box on a recent Mac"; a Mac
        // that can't actually run Apple FM should still get a working
        // experience via Ollama if it's reachable, rather than a hard
        // failure. User-explicit `ai.provider = "apple"` does NOT fall
        // back - that choice is honoured even when broken, and error
        // message tells them how to fix it
        let isAutoApple = provider == "apple" && (config.provider ?? "").isEmpty
        if provider == "apple" {
            let avail = currentFoundationModelsAvailability()
            if case .available = avail {
                return try await askApple(userContent: userContent, system: system)
            }
            if isAutoApple {
                return try await askOllama(
                    userContent: userContent, system: system, config: config
                )
            }
            throw AiError.appleUnavailable(reason: avail.userMessage)
        }
        switch provider {
        case "ollama":
            return try await askOllama(userContent: userContent, system: system, config: config)
        case "openai":
            return try await askOpenAI(userContent: userContent, system: system, config: config)
        case "anthropic":
            return try await askAnthropic(userContent: userContent, system: system, config: config)
        default: throw AiError.unknownProvider(provider)
        }
    }

    /// What provider dispatch should hit when nothing is set in
    /// `config.ai.provider`. Default-to-Apple on macOS 26+ falls back
    /// to Ollama on older systems - matches AI command palette UX
    /// promise of "works out of the box on a recent Mac, falls back
    /// to whatever the user already has running"
    static func effectiveProvider(config: AiConfig) -> String {
        if let explicit = config.provider, !explicit.isEmpty {
            return explicit.lowercased()
        }
        if #available(macOS 26.0, *) {
            return "apple"
        }
        return "ollama"
    }

    /// Human-facing name for provider that will actually run. Mirrors
    /// `dispatch`'s auto-fallback logic: if we'd default to Apple FM
    /// but assets aren't ready, dispatcher silently uses Ollama
    /// instead - so subtitle should say "Ollama" rather than
    /// promising "Apple Intelligence" and delivering something else.
    /// Pure UX text - never used for routing
    static func effectiveProviderLabel() -> String {
        let config = Config.load().ai ?? AiConfig()
        let provider = effectiveProvider(config: config)
        let isAutoApple = provider == "apple" && (config.provider ?? "").isEmpty
        if provider == "apple", isAutoApple {
            if case .available = currentFoundationModelsAvailability() {
                return "Apple Intelligence"
            }
            return "Ollama"
        }
        switch provider {
        case "apple": return "Apple Intelligence"
        case "ollama": return "Ollama"
        case "openai": return "OpenAI"
        case "anthropic": return "Anthropic"
        case let other: return other.capitalized
        }
    }

    // Routes prompt through Apple's `FoundationModels` framework.
    // Free, offline, private - but only available on Apple
    // Intelligence-supported Macs running macOS 15.2+ (framework
    // shipped with Sequoia's Apple Intelligence wave). On older OSes
    // or non-AI Macs call surfaces a clear error so user knows to
    // flip back to another provider
    //
    // Compile-time guard via `#if canImport` lets us build on SDKs
    // that predate framework. Runtime guard via `#available` covers
    // case where SDK has it but user's OS doesn't. Together they
    // make this branch a no-op on every platform that can't honour
    // it
    private static func askApple(userContent: String, system: String?) async throws -> String {
        #if canImport(FoundationModels)
        if #available(macOS 26.0, *) {
            return try await runFoundationModels(userContent: userContent, system: system)
        }
        throw AiError.appleUnavailable(reason: "macOS 26.0+ required")
        #else
        throw AiError.appleUnavailable(reason: "FoundationModels framework not present in this build")
        #endif
    }

    #if canImport(FoundationModels)
    @available(macOS 26.0, *)
    private static func runFoundationModels(
        userContent: String,
        system: String?
    ) async throws -> String {
        // Imported lazily so file still compiles on SDKs that dont
        // ship framework - `import` at function scope is fine for
        // top-level symbol lookups
        let session = FoundationModelsSession(system: system)
        return try await session.respond(to: userContent)
    }
    #endif


    private static func askOllama(
        userContent: String,
        system: String?,
        config: AiConfig
    ) async throws -> String {
        let endpoint = config.endpoint ?? "http://localhost:11434"
        let model = config.model ?? "llama3.2"
        // Validate scheme + host before composing URL
        guard validateEndpoint(endpoint, provider: "ollama") != nil,
              let url = URL(string: "\(endpoint)/api/generate")
        else {
            throw AiError.badUrl(endpoint)
        }
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.timeoutInterval = 120
        // /api/generate doesn't split system/user. For small local
        // models a merged prompt beats using /api/chat (which some
        // Ollama builds behave differently on). Keep instruction up
        // top so attention to it is strongest
        let prompt: String
        if let system = system {
            prompt = "\(system)\n\n---\n\n\(userContent)"
        } else {
            prompt = userContent
        }
        let body: [String: Any] = [
            "model": model,
            "prompt": prompt,
            "stream": false,
        ]
        req.httpBody = try JSONSerialization.data(withJSONObject: body)

        do {
            let (data, resp) = try await URLSession.shared.data(for: req)
            try throwIfBadStatus(resp, data: data)
            let decoded = try JSONDecoder().decode(OllamaResponse.self, from: data)
            return decoded.response
        } catch let e as URLError where e.code == .cannotConnectToHost || e.code == .cannotFindHost {
            throw AiError.ollamaUnreachable
        }
    }


    private static func askOpenAI(
        userContent: String,
        system: String?,
        config: AiConfig
    ) async throws -> String {
        guard let key = config.effectiveApiKey else { throw AiError.missingApiKey(provider: "openai") }
        let endpoint = config.endpoint ?? "https://api.openai.com"
        let model = config.model ?? "gpt-4o-mini"
        // Validate scheme + host before composing URL
        guard validateEndpoint(endpoint, provider: "openai") != nil,
              let url = URL(string: "\(endpoint)/v1/chat/completions")
        else {
            throw AiError.badUrl(endpoint)
        }
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.setValue("Bearer \(key)", forHTTPHeaderField: "Authorization")
        req.timeoutInterval = 120
        var messages: [[String: String]] = []
        if let system = system {
            messages.append(["role": "system", "content": system])
        }
        messages.append(["role": "user", "content": userContent])
        let body: [String: Any] = [
            "model": model,
            "messages": messages,
        ]
        req.httpBody = try JSONSerialization.data(withJSONObject: body)

        let (data, resp) = try await URLSession.shared.data(for: req)
        try throwIfBadStatus(resp, data: data)
        let decoded = try JSONDecoder().decode(OpenAIResponse.self, from: data)
        return decoded.choices.first?.message.content ?? ""
    }


    private static func askAnthropic(
        userContent: String,
        system: String?,
        config: AiConfig
    ) async throws -> String {
        guard let key = config.effectiveApiKey else { throw AiError.missingApiKey(provider: "anthropic") }
        let endpoint = config.endpoint ?? "https://api.anthropic.com"
        let model = config.model ?? "claude-3-5-sonnet-latest"
        // Validate scheme + host before composing URL
        guard validateEndpoint(endpoint, provider: "anthropic") != nil,
              let url = URL(string: "\(endpoint)/v1/messages")
        else {
            throw AiError.badUrl(endpoint)
        }
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.setValue(key, forHTTPHeaderField: "x-api-key")
        req.setValue("2023-06-01", forHTTPHeaderField: "anthropic-version")
        req.timeoutInterval = 120
        var body: [String: Any] = [
            "model": model,
            "max_tokens": 4096,
            "messages": [["role": "user", "content": userContent]],
        ]
        if let system = system {
            // Anthropic puts system prompt in a top-level field, not
            // messages array - easy to get wrong
            body["system"] = system
        }
        req.httpBody = try JSONSerialization.data(withJSONObject: body)

        let (data, resp) = try await URLSession.shared.data(for: req)
        try throwIfBadStatus(resp, data: data)
        let decoded = try JSONDecoder().decode(AnthropicResponse.self, from: data)
        return decoded.content.first?.text ?? ""
    }

    private static func throwIfBadStatus(_ resp: URLResponse, data: Data) throws {
        guard let http = resp as? HTTPURLResponse else { return }
        if !(200..<300).contains(http.statusCode) {
            let body = String(data: data, encoding: .utf8) ?? "<binary>"
            throw AiError.httpError(http.statusCode, body)
        }
    }

    /// Defend AI dispatch from misconfigured / hostile
    /// `ai.endpoint` values
    ///
    /// Returns a parsed `URL` only when endpoint is safe to hit:
    /// - Scheme must be `https`. Single exception is Ollama bound
    ///   to a localhost host (`localhost` / `127.0.0.1` / `::1`),
    ///   canonical local-Ollama dev setup.
    /// - Loopback aliases other than three above (e.g. `127.0.0.2`)
    ///   are rejected even for Ollama.
    /// - RFC-1918 (`10/8`, `172.16/12`, `192.168/16`) and link-local
    ///   (`169.254/16` for v4, `fe80::/10` for v6) are rejected
    ///   regardless of scheme - blocks SSRF against user's intranet
    ///   even when user pasted an `https://` internal endpoint.
    /// - Returns `nil` for unparseable strings
    ///
    /// Known limitation: DNS names resolving to private IPs are not
    /// caught here - that needs a resolver step at request time
    /// (`URLSession`'s connection delegate). Out of scope for v0;
    /// this catches common "paste 169.254.169.254 by mistake" shape
    static func validateEndpoint(_ endpoint: String, provider: String) -> URL? {
        guard let url = URL(string: endpoint),
              let scheme = url.scheme?.lowercased(),
              let rawHost = url.host
        else { return nil }
        // `URL` keeps literal brackets off `.host`; an IPv6 host
        // arrives as `fe80::1` rather than `[fe80::1]`
        let host = rawHost.lowercased()
        let isLocalhost = (host == "localhost" || host == "127.0.0.1" || host == "::1")
        if scheme == "https" {
            // Https never gets a free pass on private ranges - a
            // certificate doesn't make `192.168.1.1` safer to call
        } else if scheme == "http" {
            // Plain http is only allowed for local-Ollama pattern.
            // Everything else gets rejected outright
            let providerLower = provider.lowercased()
            guard providerLower == "ollama" && isLocalhost else { return nil }
            return url
        } else {
            // Reject anything that isn't http/https outright (no
            // file://, ws://, ftp://, custom schemes)
            return nil
        }
        // Past this point: scheme is https. Still reject internal
        // hosts even though TLS is on the wire
        if isHostInRejectedRange(host) { return nil }
        return url
    }

    /// True when host string falls in a private / link-local range
    /// we dont want AI dispatch to talk to. Matches on literal IP
    /// textual form - validator's job is to catch common foot-gun
    /// ("paste an internal address"), not to be a full SSRF defence
    /// (DNS rebinding bypasses any string check; that needs
    /// network-layer filtering)
    static func isHostInRejectedRange(_ host: String) -> Bool {
        // IPv6 link-local: `fe80::/10`. Matches `fe80:`, `fe81:`,
        // ... `febf:`. Case-insensitive prefix check; higher nibble
        // after `fe8`/`fe9`/`fea`/`feb` doesn't change /10 membership
        if host.contains(":") {
            let h = host
            if h.hasPrefix("fe8") || h.hasPrefix("fe9")
                || h.hasPrefix("fea") || h.hasPrefix("feb")
            {
                return true
            }
            // Loopback non-`::1` IPv6 is essentially nonexistent
            // in practice; let `::1` pass via explicit localhost
            // check above
            return false
        }
        // IPv4: split on `.`. Anything that isn't four numeric
        // octets isn't an IP literal (it's a hostname, which we
        // leave to DNS - documented limitation)
        let parts = host.split(separator: ".")
        guard parts.count == 4 else { return false }
        let octets = parts.compactMap { Int($0) }
        guard octets.count == 4,
              octets.allSatisfy({ (0...255).contains($0) })
        else { return false }
        let a = octets[0]
        let b = octets[1]
        // 10.0.0.0/8
        if a == 10 { return true }
        // 172.16.0.0/12  (172.16.* .. 172.31.*)
        if a == 172 && (16...31).contains(b) { return true }
        // 192.168.0.0/16
        if a == 192 && b == 168 { return true }
        // 169.254.0.0/16 link-local
        if a == 169 && b == 254 { return true }
        // 127.0.0.0/8 loopback BUT NOT 127.0.0.1 (explicit localhost
        // we accept earlier). Anything else in 127/8 is worth
        // flagging - aliases like `127.0.0.2` are a common way to
        // dodge a naive substring check
        if a == 127 && host != "127.0.0.1" { return true }
        return false
    }
}


private struct OllamaResponse: Decodable {
    let response: String
}

private struct OpenAIResponse: Decodable {
    let choices: [Choice]
    struct Choice: Decodable { let message: Message }
    struct Message: Decodable { let content: String }
}

private struct AnthropicResponse: Decodable {
    let content: [Block]
    struct Block: Decodable { let type: String?; let text: String }
}

#endif // AI
