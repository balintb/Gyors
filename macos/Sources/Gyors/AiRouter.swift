// AI-only - collapses to nothing when WITH_AI=0
#if AI
import Foundation

/// Natural-language -> keyword translator
///
/// Sends user's free-form input to configured AI backend along with
/// a tool catalog, gets back either a structured tool pick or `nil`
/// (model declined / nothing fits / network failed). Caller is
/// responsible for what to do with answer - typically: render
/// picked tool back into a keyword query and feed it through
/// `Effect::SetInput` so existing orchestrator dispatches normally
///
/// v0 ships with Ollama backend only. OpenAI / Anthropic / Apple FM
/// share same shape (each provider has native tool calling) and are
/// queued for follow-ups
enum AiRouter {
    /// Route a user query through configured AI backend
    ///
    /// Returns `nil` (rather than throwing) when:
    /// - backend declined to pick a tool
    /// - call failed for any reason (network, timeout, bad JSON)
    /// - backend isn't supported by this v0 (anything but Ollama)
    ///
    /// Routing is best-effort UX sugar; a failure here just means AI
    /// palette row stays as default cmd+return free-form ask. We
    /// intentionally swallow errors rather than propagate them -
    /// fast-path quality must not depend on router being up
    static func route(question: String, tools: [ToolSpec]) async -> ToolCall? {
        let config = Config.load().ai ?? AiConfig()
        // Use EXISTING `ai.provider` resolution so router honours
        // whatever user already configured for free-form asks. No
        // separate `router.provider` knob - fewer surprises
        var provider = AiClient.effectiveProvider(config: config)
        // Same auto-fallback rule as `AiClient.dispatch`: when we
        // DEFAULTED to Apple FM (no user-set provider) but it isn't
        // ready, silently route through Ollama instead. User-explicit
        // `apple` is honoured - no fallback there, so user sees
        // framework's actual error in diagnostic surface
        let isAutoApple = provider == "apple" && (config.provider ?? "").isEmpty
        if provider == "apple", isAutoApple {
            if case .available = currentFoundationModelsAvailability() {
                // Proceed
            } else {
                provider = "ollama"
            }
        }
        switch provider {
        case "ollama":
            return await routeViaOllama(question: question, tools: tools, config: config)
        case "openai":
            return await routeViaOpenAI(question: question, tools: tools, config: config)
        case "anthropic":
            return await routeViaAnthropic(question: question, tools: tools, config: config)
        case "apple":
            return await routeViaApple(question: question, tools: tools)
        default:
            // Unknown / unsupported provider - return nil so palette
            // row stays put. Free-form cmd+return still routes
            // through user's configured provider (or surfaces
            // appropriate error from AiClient.dispatch)
            return nil
        }
    }

    /// Ollama's `/api/chat` accepts a `tools` array in same shape
    /// OpenAI uses (Ollama implemented Function Calling on
    /// OpenAI-compatible side). Models that actually do this well
    /// in practice: `llama3.1`, `mistral-nemo`, `qwen2.5` >= 7B.
    /// Smaller / non-tool-tuned models tend to hallucinate calls;
    /// schema validation in `parseToolCall` rejects malformed picks
    /// rather than passing them through.
    /// Internal (not private) so test harness can drive it against a
    /// live local Ollama when one is reachable
    static func routeViaOllama(
        question: String,
        tools: [ToolSpec],
        config: AiConfig
    ) async -> ToolCall? {
        let endpoint = config.endpoint ?? "http://localhost:11434"
        let model = config.model ?? "llama3.2"
        // Validate endpoint before composing URL. A bad /
        // private-range value yields `nil` (router declines and
        // caller falls through to free-form Ask AI)
        guard AiClient.validateEndpoint(endpoint, provider: "ollama") != nil,
              let url = URL(string: "\(endpoint)/api/chat")
        else { return nil }

        let body: [String: Any] = [
            "model": model,
            "messages": [
                ["role": "system", "content": systemPrompt],
                ["role": "user",   "content": question],
            ],
            "tools": tools.map(asOllamaToolJSON),
            "stream": false,
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: body) else {
            return nil
        }

        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.httpBody = data
        // Tight timeout - router runs on typing-hot-path. A user who
        // types at 80wpm produces a fresh keystroke every ~150ms; if
        // Ollama hasn't answered in 4s we should give up and let
        // next keystroke retry rather than block UI
        req.timeoutInterval = 4.0

        do {
            let (raw, resp) = try await URLSession.shared.data(for: req)
            guard let http = resp as? HTTPURLResponse, http.statusCode == 200 else {
                return nil
            }
            return parseOllamaToolCall(data: raw)
        } catch {
            return nil
        }
    }

    /// Tool-list payload as OpenAI/Ollama shape:
    /// `{type: "function", function: {name, description, parameters}}`
    static func asOllamaToolJSON(_ tool: ToolSpec) -> [String: Any] {
        var properties: [String: [String: String]] = [:]
        for (key, param) in tool.parameters.properties {
            properties[key] = [
                "type": param.kind.rawValue,
                "description": param.description,
            ]
        }
        return [
            "type": "function",
            "function": [
                "name": tool.name,
                "description": tool.description,
                "parameters": [
                    "type": "object",
                    "properties": properties,
                    "required": tool.parameters.required,
                ],
            ],
        ]
    }

    /// Decode Ollama's `/api/chat` response and pull out first tool
    /// call. Returns `nil` if model returned text instead (no tool
    /// was picked) or if call references an unknown tool
    /// (hallucination guard)
    static func parseOllamaToolCall(data: Data) -> ToolCall? {
        guard let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let message = json["message"] as? [String: Any]
        else { return nil }

        // Ollama returns `tool_calls` as an array. Take first.
        // Models occasionally return multiple - for our routing UX
        // we only ever surface one preview row, so multi-call is an
        // accidental over-pick
        guard let toolCalls = message["tool_calls"] as? [[String: Any]],
              let first = toolCalls.first,
              let function = first["function"] as? [String: Any],
              let name = function["name"] as? String
        else { return nil }
        // Only accept calls that name a registered tool. Anything
        // else is a hallucination and we silently drop it - user
        // will see AI palette row stay put, indicating "no routing
        // happened, cmd+return is still your option."
        guard AiRouterTools.find(named: name) != nil else { return nil }

        // `arguments` can come back as a JSON object (newer Ollama
        // / models that respect schema) OR as a JSON-encoded string
        // that needs a second parse (older / quantised models). We
        // accept both shapes
        let rawArgs = function["arguments"]
        var argDict: [String: Any] = [:]
        if let dict = rawArgs as? [String: Any] {
            argDict = dict
        } else if let str = rawArgs as? String,
                  let parsed = try? JSONSerialization.jsonObject(with: Data(str.utf8))
                    as? [String: Any]
        {
            argDict = parsed
        }

        // Coerce every argument to a String. Eventual rendering
        // pastes values into a keyword template - preserving native
        // numeric precision past LLM's own JSON serialisation would
        // be more careful than use case warrants
        var stringArgs: [String: String] = [:]
        for (key, value) in argDict {
            stringArgs[key] = stringify(value)
        }
        return ToolCall(toolName: name, arguments: stringArgs)
    }

    /// Render any JSON-Numbery / String / Bool value as a string
    /// suitable for keyword template substitution. Numbers render
    /// without trailing `.0` so `100.0` becomes `"100"` (keyword
    /// form for currency / units expects bare integers when value
    /// is integral)
    static func stringify(_ value: Any) -> String {
        if let s = value as? String { return s }
        if let n = value as? NSNumber {
            // NSNumber boxing in JSONSerialization makes integer/double
            // distinction lossy; treat anything whose double form
            // equals its rounded form as an integer
            let d = n.doubleValue
            if d.rounded() == d && abs(d) < 1e15 {
                return String(Int64(d))
            }
            return String(d)
        }
        return String(describing: value)
    }

    /// System prompt. Moat - bad prompt -> hallucinated tools.
    /// Iterate via router eval corpus once we have one running
    static let systemPrompt = """
        You are Gyors's command router. Your job: pick exactly the right \
        tool for the user's request, or decline. Never invent a tool. \
        Never explain. Just call the tool with concrete arguments \
        extracted from the user's text, or return text if no tool fits.

        Rules:
        - Extract concrete values from the user's text. Don't guess values \
          they didn't supply.
        - If multiple tools fit, pick the most specific one.
        - If no tool fits cleanly, return a brief plain-text reply. The \
          user will see your text in a fallback panel.
        - Never call a tool just to do something. When in doubt, decline.

        The user's input is verbatim what they typed in a launcher. They \
        want a result, not a conversation.
        """


    /// OpenAI Chat Completions with `tools`. Same wire shape as
    /// Ollama's `/api/chat` (Ollama implemented function calling on
    /// OpenAI-compatible side), with two differences worth
    /// flagging: response wraps everything in `choices[0]` instead
    /// of a top-level `message`, and `arguments` is ALWAYS a
    /// JSON-encoded string rather than an object
    private static func routeViaOpenAI(
        question: String,
        tools: [ToolSpec],
        config: AiConfig
    ) async -> ToolCall? {
        guard let key = config.effectiveApiKey else { return nil }
        let endpoint = config.endpoint ?? "https://api.openai.com"
        let model = config.model ?? "gpt-4o-mini"
        // Validate the endpoint before composing the URL
        guard AiClient.validateEndpoint(endpoint, provider: "openai") != nil,
              let url = URL(string: "\(endpoint)/v1/chat/completions")
        else { return nil }

        let body: [String: Any] = [
            "model": model,
            "messages": [
                ["role": "system", "content": systemPrompt],
                ["role": "user",   "content": question],
            ],
            "tools": tools.map(asOllamaToolJSON),
            "tool_choice": "auto",
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: body) else {
            return nil
        }

        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.setValue("Bearer \(key)", forHTTPHeaderField: "Authorization")
        req.httpBody = data
        req.timeoutInterval = 4.0

        do {
            let (raw, resp) = try await URLSession.shared.data(for: req)
            guard let http = resp as? HTTPURLResponse, http.statusCode == 200 else {
                return nil
            }
            return parseOpenAiToolCall(data: raw)
        } catch {
            return nil
        }
    }

    /// Decode OpenAI's chat-completions response and pull out first
    /// tool call. Like Ollama, multi-call responses are trimmed to
    /// one - router's UX surfaces a single preview row, not a wall
    /// of options
    static func parseOpenAiToolCall(data: Data) -> ToolCall? {
        guard let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let choices = json["choices"] as? [[String: Any]],
              let choice = choices.first,
              let message = choice["message"] as? [String: Any],
              let toolCalls = message["tool_calls"] as? [[String: Any]],
              let first = toolCalls.first,
              let function = first["function"] as? [String: Any],
              let name = function["name"] as? String
        else { return nil }
        guard AiRouterTools.find(named: name) != nil else { return nil }

        // OpenAI's contract: `arguments` is ALWAYS a JSON-encoded
        // string. No object form to handle. Robustness against
        // model returning malformed JSON: silently drop call (yields
        // a stuck palette row, not a crash)
        guard let argsString = function["arguments"] as? String,
              let argsData = argsString.data(using: .utf8),
              let argsDict = try? JSONSerialization.jsonObject(with: argsData)
                as? [String: Any]
        else { return nil }

        var stringArgs: [String: String] = [:]
        for (key, value) in argsDict {
            stringArgs[key] = stringify(value)
        }
        return ToolCall(toolName: name, arguments: stringArgs)
    }


    /// Anthropic Messages API with `tools`. Two shape differences
    /// from OpenAI/Ollama:
    /// - Tool spec uses `input_schema` (not `parameters`)
    /// - Tool calls come back as `tool_use` content blocks at top
    ///   of `content`, alongside any text model also produced. We
    ///   pull first `tool_use` block
    private static func routeViaAnthropic(
        question: String,
        tools: [ToolSpec],
        config: AiConfig
    ) async -> ToolCall? {
        guard let key = config.effectiveApiKey else { return nil }
        let endpoint = config.endpoint ?? "https://api.anthropic.com"
        let model = config.model ?? "claude-haiku-4-5"
        // Validate the endpoint before composing the URL
        guard AiClient.validateEndpoint(endpoint, provider: "anthropic") != nil,
              let url = URL(string: "\(endpoint)/v1/messages")
        else { return nil }

        let body: [String: Any] = [
            "model": model,
            "max_tokens": 1024,
            "system": systemPrompt,
            "messages": [
                ["role": "user", "content": question],
            ],
            "tools": tools.map(asAnthropicToolJSON),
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: body) else {
            return nil
        }

        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.setValue(key, forHTTPHeaderField: "x-api-key")
        req.setValue("2023-06-01", forHTTPHeaderField: "anthropic-version")
        req.httpBody = data
        req.timeoutInterval = 4.0

        do {
            let (raw, resp) = try await URLSession.shared.data(for: req)
            guard let http = resp as? HTTPURLResponse, http.statusCode == 200 else {
                return nil
            }
            return parseAnthropicToolCall(data: raw)
        } catch {
            return nil
        }
    }

    /// Anthropic's tool spec shape - same idea as OpenAI's but with
    /// flatter wrapping (no `function:` envelope) and a renamed
    /// `input_schema` field
    static func asAnthropicToolJSON(_ tool: ToolSpec) -> [String: Any] {
        var properties: [String: [String: String]] = [:]
        for (key, param) in tool.parameters.properties {
            properties[key] = [
                "type": param.kind.rawValue,
                "description": param.description,
            ]
        }
        return [
            "name": tool.name,
            "description": tool.description,
            "input_schema": [
                "type": "object",
                "properties": properties,
                "required": tool.parameters.required,
            ],
        ]
    }

    /// Decode Anthropic's Messages response and pull out first
    /// `tool_use` content block. Anthropic returns `input` as an
    /// object directly (no double-encoded JSON string), which is
    /// why this parser is simpler than OpenAI's
    static func parseAnthropicToolCall(data: Data) -> ToolCall? {
        guard let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let content = json["content"] as? [[String: Any]]
        else { return nil }

        // Find first content block whose type is `tool_use`.
        // Anthropic interleaves text + tool_use blocks; we ignore
        // text and pick first tool call. Same single-call UX
        // contract as other backends
        guard let block = content.first(where: { ($0["type"] as? String) == "tool_use" }),
              let name = block["name"] as? String,
              let input = block["input"] as? [String: Any]
        else { return nil }
        guard AiRouterTools.find(named: name) != nil else { return nil }

        var stringArgs: [String: String] = [:]
        for (key, value) in input {
            stringArgs[key] = stringify(value)
        }
        return ToolCall(toolName: name, arguments: stringArgs)
    }


    /// On-device routing via Apple Foundation Models (macOS 26+).
    /// Framework dispatches typed `Tool` structs autonomously; we
    /// dont actually want our `call()` to do anything (keyword-form
    /// rendering happens in `AiRouterTools.render`), so each tool's
    /// `call()` is a captured-args sentinel. After `respond(to:)`
    /// returns, we read captured args from session-side state
    private static func routeViaApple(
        question: String,
        tools: [ToolSpec]
    ) async -> ToolCall? {
        #if canImport(FoundationModels)
        if #available(macOS 26.0, *) {
            return await runAppleFmRouter(question: question, tools: tools)
        }
        return nil
        #else
        return nil
        #endif
    }
}

#endif // AI
