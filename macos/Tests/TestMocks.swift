import Foundation

// Shared mocks + Live-Ollama probe helpers. Pulled out of the original
// monolithic test file so each per-topic suite can `let mock =
// MockBridge()` without dragging in entire test runner state

/// Build a minimal `(ViewModel, MockNoteIO, MockBridge)` triple for
/// editor-mode tests - editor needs a `noteIO` for read/write, and
/// every test inevitably wants the `MockBridge` too. Used by
/// `EditorTests`, `MarkdownListContinuerTests`, `AiRouterLiveTests`
func editorVM() -> (GyorsViewModel, MockNoteIO, MockBridge) {
    let mock = MockBridge()
    let vm = GyorsViewModel(bridge: mock)
    let io = MockNoteIO()
    vm.noteIO = io
    return (vm, io, mock)
}

/// Build a minimal Candidate for tests. Defaults match the most common
/// shape (single "Open" action, app-style icon) so each call site only
/// has to override what it actually cares about
func makeCand(
    id: String,
    title: String,
    subtitle: String = "",
    kind: UInt8 = 0,
    actions: [CandidateAction] = [CandidateAction(id: "default", label: "Open")]
) -> Candidate {
    Candidate(
        id: id, title: title, subtitle: subtitle,
        iconKind: 1, iconValue: "app", kind: kind, score: 0,
        actions: actions
    )
}

final class MockNoteIO: NoteIO {
    var files: [String: String] = [:]
    var writeCalls: Int = 0
    var failNextWrite: Bool = false

    enum FakeError: Error { case fail }

    func read(path: String) throws -> String {
        if let v = files[path] { return v }
        throw CocoaError(.fileReadNoSuchFile)
    }
    func write(path: String, content: String) throws {
        writeCalls += 1
        if failNextWrite {
            failNextWrite = false
            throw FakeError.fail
        }
        files[path] = content
    }
}

//
// Used by the OLLAMA LIVE: tests above to skip silently when no
// local Ollama is reachable. All calls run synchronously with a
// short timeout so a missing Ollama doesn't stretch the suite

/// Quick TCP-level reachability probe. Returns true when GET
/// `localhost:11434/api/tags` answers 200 within ~2 seconds.
/// Anything else (connection refused, timeout, non-200 status) is
/// false -> the live tests skip
func ollamaIsReachable() -> Bool {
    guard let url = URL(string: "http://localhost:11434/api/tags") else {
        return false
    }
    var req = URLRequest(url: url)
    req.httpMethod = "GET"
    req.timeoutInterval = 2.0

    let semaphore = DispatchSemaphore(value: 0)
    var ok = false
    let task = URLSession.shared.dataTask(with: req) { _, response, _ in
        if let http = response as? HTTPURLResponse, http.statusCode == 200 {
            ok = true
        }
        semaphore.signal()
    }
    task.resume()
    _ = semaphore.wait(timeout: .now() + 2.5)
    return ok
}

/// List of model names returned by Ollama's `/api/tags`. Used to
/// pick a tool-capable model for the live router test. Empty list
/// when Ollama isn't running, malformed response, or the user
/// hasn't pulled any models yet
func ollamaInstalledModels() -> [String] {
    guard let url = URL(string: "http://localhost:11434/api/tags") else {
        return []
    }
    var req = URLRequest(url: url)
    req.httpMethod = "GET"
    req.timeoutInterval = 2.0

    let semaphore = DispatchSemaphore(value: 0)
    var models: [String] = []
    let task = URLSession.shared.dataTask(with: req) { data, _, _ in
        defer { semaphore.signal() }
        guard let data = data,
              let json = try? JSONSerialization.jsonObject(with: data)
                as? [String: Any],
              let list = json["models"] as? [[String: Any]]
        else { return }
        models = list.compactMap { $0["name"] as? String }
    }
    task.resume()
    _ = semaphore.wait(timeout: .now() + 2.5)
    return models
}

/// Pick the first installed model that's likely to handle tool
/// calling well. Tool-capable family priority list, then fall back
/// to whatever user has if none of the preferred ones are
/// installed (the test's assertions are permissive enough that
/// even a bad model won't false-fail)
func pickToolCapableModel(from installed: [String]) -> String? {
    let preferred = [
        "llama3.1", "llama3.2", "qwen2.5", "mistral-nemo",
        "qwen3", "llama3", "mistral",
    ]
    for needle in preferred {
        if let hit = installed.first(where: { $0.contains(needle) }) {
            return hit
        }
    }
    return installed.first
}

final class MockBridge: QueryBridge {
    var queryResponse: [Candidate] = []
    var lastPattern: String = ""
    var recordedClips: [String] = []
    var activations: [(id: String, action: String)] = []
    var clearedClipboard: Int = 0
    /// Effect returned from next `activate` call
    var nextEffect: Effect?
    /// Stub notes-folder path. Tests that exercise rename fill this in
    var stubNotesFolder: String = ""
    /// Ordered list of queries the VM pushed into `recordQuery`.
    /// Tests assert on order + contents to verify recall semantics
    var recordedQueries: [String] = []
    /// What `recentQueries` returns - tests seed this to control
    /// what the up-arrow walker sees
    var stubRecentQueries: [String] = []

    func query(_ pattern: String) -> [Candidate] {
        lastPattern = pattern
        return queryResponse
    }
    func activate(_ id: String, action: String) -> Effect? {
        activations.append((id: id, action: action))
        return nextEffect
    }
    var appCount: UInt64 { 0 }
    var clipboardCount: UInt64 { UInt64(recordedClips.count) }
    func recordClipboard(_ content: String) {
        recordedClips.append(content)
    }
    func clearClipboardHistory() {
        clearedClipboard += 1
        recordedClips.removeAll()
    }
    var notesFolder: String { stubNotesFolder }

    func recordQuery(_ pattern: String) {
        recordedQueries.append(pattern)
    }
    func recentQueries(limit: Int) -> [String] {
        Array(stubRecentQueries.prefix(limit))
    }
    func lastQuery() -> String {
        stubRecentQueries.first ?? ""
    }
}
