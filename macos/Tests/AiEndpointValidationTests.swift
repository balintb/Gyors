#if AI
import Foundation

/// Coverage. `AiClient.validateEndpoint` must accept the
/// legitimate provider shapes (https everywhere, plus localhost
/// http for Ollama) and reject the foot-guns (RFC-1918, link-local
/// metadata endpoints, loopback aliases, IPv6 link-local)
func runAiEndpointValidationTests() {

runGroup("validateEndpoint - https accepted for any provider") {
    expect(AiClient.validateEndpoint("https://api.openai.com", provider: "openai") != nil,
           "https openai allowed")
    expect(AiClient.validateEndpoint("https://api.anthropic.com", provider: "anthropic") != nil,
           "https anthropic allowed")
    expect(AiClient.validateEndpoint("https://example.com:11434", provider: "ollama") != nil,
           "https ollama on public host allowed")
}

runGroup("validateEndpoint - http rejected for non-ollama") {
    expect(AiClient.validateEndpoint("http://api.openai.com", provider: "openai") == nil,
           "http openai rejected")
    expect(AiClient.validateEndpoint("http://localhost:11434", provider: "openai") == nil,
           "http localhost still rejected for openai (only ollama gets the exemption)")
}

runGroup("validateEndpoint - localhost http allowed for ollama only") {
    expect(AiClient.validateEndpoint("http://localhost:11434", provider: "ollama") != nil,
           "ollama on localhost http allowed")
    expect(AiClient.validateEndpoint("http://127.0.0.1:11434", provider: "ollama") != nil,
           "ollama on 127.0.0.1 http allowed")
    expect(AiClient.validateEndpoint("http://[::1]:11434", provider: "ollama") != nil,
           "ollama on ::1 http allowed")
}

runGroup("validateEndpoint - loopback aliases blocked") {
    expect(AiClient.validateEndpoint("http://127.0.0.2:11434", provider: "ollama") == nil,
           "127.0.0.2 blocked (not the literal localhost)")
    expect(AiClient.validateEndpoint("http://127.1.2.3:11434", provider: "ollama") == nil,
           "127.x.x.x other than .1 blocked")
}

runGroup("validateEndpoint - AWS metadata endpoint blocked") {
    expect(AiClient.validateEndpoint("http://169.254.169.254/", provider: "ollama") == nil,
           "AWS metadata via http blocked")
    expect(AiClient.validateEndpoint("https://169.254.169.254/", provider: "openai") == nil,
           "AWS metadata via https still blocked")
}

runGroup("validateEndpoint - RFC-1918 blocked even via https") {
    expect(AiClient.validateEndpoint("http://192.168.1.1/", provider: "ollama") == nil,
           "192.168/16 blocked")
    expect(AiClient.validateEndpoint("http://10.0.0.5/", provider: "ollama") == nil,
           "10/8 blocked")
    expect(AiClient.validateEndpoint("http://172.16.0.1/", provider: "ollama") == nil,
           "172.16/12 blocked (low end)")
    expect(AiClient.validateEndpoint("http://172.31.255.254/", provider: "ollama") == nil,
           "172.16/12 blocked (high end)")
    expect(AiClient.validateEndpoint("https://192.168.1.1/", provider: "openai") == nil,
           "https into 192.168/16 still blocked")
}

runGroup("validateEndpoint - 172.x boundary not over-blocked") {
    expect(AiClient.validateEndpoint("https://172.15.0.1/", provider: "openai") != nil,
           "172.15.x is outside 172.16/12 - allowed")
    expect(AiClient.validateEndpoint("https://172.32.0.1/", provider: "openai") != nil,
           "172.32.x is outside 172.16/12 - allowed")
}

runGroup("validateEndpoint - IPv6 link-local blocked") {
    expect(AiClient.validateEndpoint("https://[fe80::1]/", provider: "openai") == nil,
           "fe80::/10 blocked")
}

runGroup("validateEndpoint - unparseable input returns nil") {
    expect(AiClient.validateEndpoint("not a url at all", provider: "openai") == nil,
           "garbage rejected")
    expect(AiClient.validateEndpoint("", provider: "openai") == nil,
           "empty rejected")
    expect(AiClient.validateEndpoint("ftp://example.com", provider: "openai") == nil,
           "non-http scheme rejected")
}

}
#else
func runAiEndpointValidationTests() {}
#endif
