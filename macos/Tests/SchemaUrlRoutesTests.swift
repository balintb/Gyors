import Foundation

func runSchemaUrlRoutesTests() {

runGroup("schema-driven bool toggle via FIELDS") {
    // Install a fixture that mirrors what gyors_config_fields_json
    // would return for a new bool key. No Rust link needed
    ConfigSchema.installFromJson("""
        [{"key":"preview_markdown","slug":"preview-markdown",
          "type":"bool","default":"true",
          "description":"Render markdown in previews","enum_values":null}]
        """)
    let u = URL(string: "gyors://toggle/preview-markdown")!
    if case .toggleBool(let jsonKey, let displayName) = GyorsUrlHandler.parse(u) {
        expect(jsonKey == "preview_markdown", "key: \(jsonKey)")
        expect(displayName == "Render markdown in previews", "label: \(displayName)")
    } else {
        expect(false, "expected toggleBool, got \(GyorsUrlHandler.parse(u))")
    }
}

runGroup("schema-driven bool set via FIELDS") {
    ConfigSchema.installFromJson("""
        [{"key":"preview_syntax","slug":"preview-syntax",
          "type":"bool","default":"true",
          "description":"Colourise code","enum_values":null}]
        """)
    let u = URL(string: "gyors://set/preview-syntax?value=false")!
    if case .setBool(let jsonKey, _, let value) = GyorsUrlHandler.parse(u) {
        expect(jsonKey == "preview_syntax", "key: \(jsonKey)")
        expect(value == false, "value: \(value)")
    } else {
        expect(false, "expected setBool, got \(GyorsUrlHandler.parse(u))")
    }
}

runGroup("schema-driven toggle rejects non-bool FIELDS entry") {
    ConfigSchema.installFromJson("""
        [{"key":"ai.model","slug":"ai-model",
          "type":"text","default":"llama3",
          "description":"AI model id","enum_values":null}]
        """)
    let u = URL(string: "gyors://toggle/ai-model")!
    if case .invalid = GyorsUrlHandler.parse(u) { expect(true, "invalid") }
    else { expect(false, "expected invalid for non-bool schema toggle") }
    // Reset to empty so later tests dont see this fixture
    ConfigSchema.install([])
}

}
