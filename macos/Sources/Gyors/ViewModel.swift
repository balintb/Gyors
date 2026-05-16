import Foundation
import Combine

/// Raw value mirror of Rust `CandidateKind` enum. Must match
/// encoding in `gyors_ipc::to_ui`
enum CandidateKindCode: UInt8 {
    case app = 0
    case file = 1
    case calculation = 2
    case snippet = 3
    case clipboard = 4
    case web = 5
    case action = 6
    case custom = 7
}

enum ViewMode: Equatable {
    case main
    case actions(Candidate)
    case aiThinking(String)
    case aiResult(String, String)
    case aiError(String, String)
    /// Inline markdown editor. Payload is absolute file path being
    /// edited; content lives in `GyorsViewModel.editingContent` so
    /// it can be bound to `TextEditor` without shoving a binding
    /// through an enum
    case editor(String)
    /// Inline image preview (e.g. QR -> "Show QR"). Keeps user
    /// inside launcher - no new window to dismiss
    case imagePreview(base64: String, label: String)
    /// Inline text preview for conversions whose output is too
    /// large to fit in a subtitle (JSON <-> YAML <-> TOML,
    /// pretty-printed JSON, ...). Enter still copies on source row;
    /// -> opens this view. Optional `language` hint drives
    /// rendering - `"markdown"` formats headings/lists/code,
    /// `"json"`/`"yaml"`/`"toml"` syntax-highlight, anything else
    /// falls back to plain monospace
    ///
    /// `editablePath` is populated for previews whose source is an
    /// editable file on disk (currently: notes). When set, Enter in
    /// preview opens that file in inline editor instead of copying
    /// - intuitive "preview -> edit" flow
    case textPreview(text: String, label: String, language: String?, editablePath: String?)
}

final class GyorsViewModel: ObservableObject {
    let bridge: QueryBridge

    @Published var query: String = ""
    @Published var results: [Candidate] = []
    @Published var selectedIndex: Int = 0
    @Published var isSearching: Bool = false
    @Published var viewMode: ViewMode = .main
    @Published var actionIndex: Int = 0
    /// Bumped each time panel opens; ContentView watches this to refocus text field
    @Published var focusTick: Int = 0
    /// Greyed-out tail shown inline after user's query (ghost
    /// text). Populated from candidates with id prefix `autoclose::`
    /// whose title extends current query; those candidates are
    /// removed from `results` so completion only lives in field
    /// itself
    @Published var autocompleteSuffix: String = ""

    /// Bound to `TextEditor`. Reset to empty when leaving editor mode
    @Published var editingContent: String = ""
    /// True iff content has changed since last disk write
    @Published var editingDirty: Bool = false
    /// "idle" / "saving" / "saved" - surfaced at bottom of editor
    /// so user can confirm writes landed
    @Published var editorStatus: EditorStatus = .idle
    /// True when in-editor markdown preview pane is shown
    /// (cmd+shift+P). Lifted to VM so `handleEscape` can distinguish
    /// "ESC while previewing" (go back to editor) from "ESC while
    /// editing" (save & exit)
    @Published var editorPreviewOpen: Bool = false

    static let filesDebounceMs: Int = 150
    /// Debounce for autosaving while user types. 500 ms feels
    /// immediate without thrashing disk per keystroke
    static let autosaveDebounceMs: Int = 500

    private var pending: DispatchWorkItem?
    private var autosave: DispatchWorkItem?
    /// Abstract file I/O so unit tests can inject a fake without
    /// touching real filesystem
    var noteIO: NoteIO = FileSystemNoteIO()

    init(bridge: QueryBridge) {
        self.bridge = bridge
    }

    func reset() {
        pending?.cancel()
        pending = nil
        autosave?.cancel()
        autosave = nil
        query = ""
        results = []
        selectedIndex = 0
        isSearching = false
        viewMode = .main
        actionIndex = 0
        editingContent = ""
        editingDirty = false
        editorStatus = .idle
        editorPreviewOpen = false
        autocompleteSuffix = ""
        // Wipe pill state so a panel re-open starts from a clean
        // input - without this, a user who committed pills last
        // session would see them still there after ESC -> reopen,
        // and their next typed text would flow into
        // `<pill> | <text>` form even though TextField looks empty
        resetChainState()
        // Refresh history snapshot every panel reopen so Up walks
        // through queries persisted between sessions too, not just
        // ones typed in this launch
        refreshHistorySnapshot()
        historyRecallIndex = nil
        historyRecallStash = nil
        // Fetch empty-state result set (curated discovery rows
        // when enabled, [] when user has dismissed them) so panel
        // never opens on a blank list. Bridge call is cheap -
        // orchestrator short-circuits empty input straight to
        // discovery provider, no fan-out
        update(query: "")
    }

    /// Pull a fresh copy of recent queries from backend. Called on
    /// reset; scoped to 50 entries so walking list stays snappy and
    /// stable without user scrolling into ancient noise
    func refreshHistorySnapshot() {
        historySnapshot = bridge.recentQueries(limit: 50)
    }

    func update(query newQuery: String) {
        query = newQuery
        pending?.cancel()

        let trimmed = newQuery.trimmingCharacters(in: .whitespaces)
        // Empty input no longer short-circuits to `results = []` -
        // backend now answers with curated discovery rows so panel
        // always has *something* to show. Fast-path call is cheap
        // (single provider, no fan-out) and lets user-facing
        // empty-state come from same code path that serves real
        // searches
        if trimmed.hasPrefix("'") {
            updateFilesPath(newQuery: newQuery, trimmed: trimmed)
        } else {
            updateFastPath(newQuery: newQuery)
        }
    }

    private func updateFastPath(newQuery: String) {
        isSearching = false
        let raw = bridge.query(newQuery)
        let split = Self.splitGhost(results: raw, query: newQuery)
        results = split.filtered
        autocompleteSuffix = split.suffix
        // `!!` recall preview: when buffer is exactly `!!`, show
        // last query inline as ghost text so user sees what will
        // run before they press Enter. We synthesise suffix
        // ourselves because backend wouldn't know about it - last
        // query is launcher-local state. Marker prefix (` -> `)
        // signals "this isn't a normal autocomplete, it's a recall
        // preview" both visually and to Tab/Enter handlers below,
        // which branch on `bangBangPreview` rather than generic
        // ghost-commit path
        if Self.isBangBang(newQuery) {
            let last = bridge.lastQuery()
            if !last.isEmpty {
                autocompleteSuffix = "\(Self.bangBangMarker)\(last)"
            }
        }
        selectedIndex = 0
        maybeRunRouter(for: newQuery)
    }

    /// Recognises a lone `!!` (with optional surrounding whitespace,
    /// including newlines so a pasted "!!\n" still triggers recall).
    /// Exposed as a static so Tab + Enter handlers can ask same
    /// question suffix-builder asked, without re-deriving
    static func isBangBang(_ raw: String) -> Bool {
        raw.trimmingCharacters(in: .whitespacesAndNewlines) == "!!"
    }

    /// Sentinel that prefixes ghost suffix for a `!!` recall. Two
    /// arrows-with-spaces is a deliberately illegal autocomplete
    /// payload (no real provider id starts with this), so Tab /
    /// Enter handlers can recognise recall path by suffix prefix
    /// without a separate state flag. Visible in UI: ghost renders
    /// as ` -> <last query>` after user's `!!`
    static let bangBangMarker = " → "

    /// Pull last-query payload out of a recall-shaped ghost suffix.
    /// Returns `nil` when suffix isn't from `!!` recall
    static func bangBangPayload(_ suffix: String) -> String? {
        guard suffix.hasPrefix(bangBangMarker) else { return nil }
        return String(suffix.dropFirst(bangBangMarker.count))
    }

    /// Background task running AI router for current query.
    /// Cancelled on every keystroke so we never have two competing
    /// router calls in flight; in-flight task verifies its snapshot
    /// still matches `self.query` before mutating results
    private var routerTask: Task<Void, Never>?

    /// Decide whether router should run for this query, and if so
    /// kick it off (debounced + cancellable)
    ///
    /// Trigger conditions:
    /// - `ai.router_enabled` config flag is on
    /// - query has at least 4 chars, so we dont blast LLM on
    ///   every initial keystroke
    /// - no NON-PALETTE bypass-rank candidate is present, i.e. no
    ///   provider had a precise direct hit (currency, calc, b64,
    ///   ...). Fuzzy app matches on stray words DONT count - they
    ///   used to defeat trigger ("how much is 100 euros in
    ///   forints?" might fuzzy-match an app starting with H), and
    ///   router is exactly meant for "I typed natural English and
    ///   no keyword parser caught it" case
    private func maybeRunRouter(for newQuery: String) {
        routerTask?.cancel()
        routerTask = nil

        #if AI
        let cfg = Config.load()
        guard cfg.effectiveRouterEnabled else { return }
        let trimmed = newQuery.trimmingCharacters(in: .whitespaces)
        guard trimmed.count >= 4 else { return }
        guard !hasStrongNonPaletteMatch() else { return }

        let snapshot = newQuery
        routerTask = Task { [weak self] in
            // 300ms debounce - typing at 80wpm produces a fresh
            // keystroke every ~150ms, so this lets a fast typist
            // settle before any LLM call goes out. Next keystroke
            // `cancel()`s this task before sleep returns
            try? await Task.sleep(nanoseconds: 300_000_000)
            if Task.isCancelled { return }
            guard let call = await AiRouter.route(
                question: snapshot,
                tools: AiRouterTools.all
            ) else { return }
            if Task.isCancelled { return }

            let keyword: String
            switch AiRouterTools.render(call) {
            case .success(let s): keyword = s
            case .failure: return
            }
            await MainActor.run { [weak self] in
                guard let self,
                      self.query == snapshot,
                      !self.hasStrongNonPaletteMatch()
                else { return }
                self.injectRouterPreview(keyword: keyword, toolName: call.toolName)
            }
        }
        #else
        // No-AI build: router silently does nothing. Rust side
        // doesn't emit `ai_router::...` ids either, so input path
        // stays unchanged
        _ = newQuery
        #endif
    }

    /// `BYPASS_RANK_SCORE` from Rust orchestrator - direct hits
    /// (calc result, currency conversion, hint match, ...) ride at
    /// this score so ranker can't dethrone them with a fuzzy app
    /// match. Mirrored here so we can detect "is there a real
    /// provider hit?" client-side without re-querying
    private static let bypassRankScore: Int64 = 1_000_000

    /// True when at least one row was produced by a provider as a
    /// PRECISE hit (bypass-rank score), excluding synthetic AI
    /// palette row. Drives router's trigger gate: when this is true
    /// user already has a real answer ranked, so router speculation
    /// would just add noise
    func hasStrongNonPaletteMatch() -> Bool {
        results.contains { c in
            c.score >= Self.bypassRankScore && !c.id.hasPrefix("ai_palette::")
        }
    }

    /// Legacy: kept for tests already pinned to it. Router's gate
    /// now uses `hasStrongNonPaletteMatch` (more permissive), but
    /// this predicate still describes a legitimate state worth
    /// asserting in tests of orchestrator's empty-state path
    func resultsAreEmptyExceptPalette() -> Bool {
        guard results.count == 1 else { return false }
        return results[0].id.hasPrefix("ai_palette::")
    }

    /// Replace AI palette row with a router-rendered preview row.
    /// On activate, row's `Effect::SetInput(keyword_form)` cycles
    /// keyword string back through orchestrator - user sees
    /// natural-language input become precise keyword query, and
    /// matching provider's real result appears. Internal so
    /// regression tests can verify row shape directly
    func injectRouterPreview(keyword: String, toolName: String) {
        let pretty = Self.prettyToolName(toolName)
        let row = Candidate(
            id: "ai_router::\(keyword)",
            title: keyword,
            subtitle: "via \(pretty) · ↵ to use",
            iconKind: 1, // SF Symbol
            iconValue: "wand.and.stars",
            kind: 0, // Action
            score: 0,
            actions: [CandidateAction(id: "default", label: "Use")]
        )
        results = [row]
        selectedIndex = 0
    }

    /// Strip `<provider>__<verb>` shape into something readable.
    /// `currency__convert` -> `currency`. Verb is implicit in row's
    /// title (rendered keyword form), so subtitle only needs
    /// provider name. Internal for direct testing
    static func prettyToolName(_ raw: String) -> String {
        if let underscore = raw.range(of: "__") {
            return String(raw[..<underscore.lowerBound])
        }
        return raw
    }

    /// Pull any `autoclose::...` candidate out of `results` and
    /// convert it into a ghost-text suffix (part of its id that
    /// *extends* current query). Returning filtered list keeps
    /// completion out of results stack - it lives only in text
    /// field, which is what "inline autocomplete" means to user
    ///
    /// We key off id rather than title because title is flattened +
    /// ellipsized at 80 chars for display, which would break a
    /// prefix-based extraction on long inputs. Id carries full
    /// balanced form verbatim. Multiple autoclose rows (which
    /// shouldn't happen) collapse to first usable one; any others
    /// are dropped so they never leak into list
    static func splitGhost(
        results: [Candidate],
        query: String
    ) -> (suffix: String, filtered: [Candidate]) {
        let prefix = "autoclose::"
        var suffix = ""
        var filtered: [Candidate] = []
        filtered.reserveCapacity(results.count)
        for c in results {
            guard c.id.hasPrefix(prefix) else {
                filtered.append(c)
                continue
            }
            if suffix.isEmpty {
                let full = String(c.id.dropFirst(prefix.count))
                if full.hasPrefix(query), full.count > query.count {
                    suffix = String(full.dropFirst(query.count))
                }
            }
            // Autoclose candidates never appear in list regardless
            // of whether we extracted a ghost - "inline or nothing"
        }
        return (suffix, filtered)
    }

    private func updateFilesPath(newQuery: String, trimmed: String) {
        let effectivePattern = String(trimmed.dropFirst())
            .trimmingCharacters(in: .whitespaces)

        results = results.filter { c in
            c.kind != CandidateKindCode.calculation.rawValue
                || c.subtitle == effectivePattern
        }

        // File search walks disk and can take 100s of ms on a
        // large notes folder or a populated `~/Downloads` tree.
        // Surface two layers of feedback:
        //   1. `isSearching` -> inline ProgressView next to input
        //      + "Searching files..." placeholder row in dropdown
        //      (rendered by ContentView when results is empty AND
        //      isSearching is true).
        //   2. `BusyTracker` -> menu-bar icon animation so user
        //      knows work is happening even if panel is partially
        //      obscured.
        // Empty pattern -> no search to feedback, skip both
        isSearching = !effectivePattern.isEmpty

        let snapshot = newQuery
        let task = DispatchWorkItem { [weak self] in
            guard let self else { return }
            // Begin/end straddle actual disk hit. `defer` fires on
            // every exit path - cancelled `self`, finished query,
            // or thrown panic - so busy state never leaks
            BusyTracker.shared.begin()
            DispatchQueue.global(qos: .userInitiated).async { [weak self] in
                defer { BusyTracker.shared.end() }
                guard let self else { return }
                let r = self.bridge.query(snapshot)
                DispatchQueue.main.async { [weak self] in
                    guard let self, self.query == snapshot else { return }
                    let split = Self.splitGhost(results: r, query: snapshot)
                    self.results = split.filtered
                    self.autocompleteSuffix = split.suffix
                    self.selectedIndex = 0
                    self.isSearching = false
                }
            }
        }
        pending = task
        DispatchQueue.main.asyncAfter(
            deadline: .now() + .milliseconds(Self.filesDebounceMs),
            execute: task
        )
    }


    /// Committed chain stages rendered as pills. Authoritative
    /// state on VM - NOT derived from `query`. Pills are minted
    /// when user types a ` | ` (space-pipe-space) separator in
    /// active field, unless base starts with a pipe-using keyword
    /// (regex, YAML conversions, AI transforms, `ai`/`ask`) in
    /// which case `|` stays literal so those providers see their
    /// input verbatim
    ///
    /// Users can also force-commit by typing opt| which bypasses
    /// bail list - useful for chaining something like
    /// `ai explain | copy` where default would refuse
    @Published var chainCommits: [String] = []

    /// Mirror of TextField's current text. `setActiveBuffer` is
    /// only write path from UI binding - it handles auto-commit
    /// parsing (detecting ` | ` and promoting into a pill)
    @Published var activeBuffer: String = ""

    /// Snapshot of `bridge.recentQueries(...)` taken whenever panel
    /// reopens. Captured once so walking with Up / Down shows a
    /// stable list even if a new query gets recorded mid-navigation
    private var historySnapshot: [String] = []
    /// Current position within `historySnapshot`: `nil` means "not
    /// walking", `0` means "showing most recent", etc
    private var historyRecallIndex: Int? = nil
    /// What input buffer held at moment user first triggered
    /// recall. Restored when Down walks back past top, so user can
    /// peek into history and bail back to what they were typing
    /// without losing it
    private var historyRecallStash: String? = nil

    /// Providers whose input legitimately contains `|`. Matched
    /// against first whitespace-delimited token of effective base.
    /// Mirrors Rust-side list in `crates/gyors-ipc/src/lib.rs`
    private static let pipeUsingKeywords: Set<String> = [
        "re", "regex",
        "json2yaml", "yaml2json",
        "json2toml", "toml2json",
        "yaml2toml", "toml2yaml",
        "summarize", "tldr", "explain", "rewrite",
        "fix", "shorten", "expand", "translate",
        "ai", "ask",
    ]

    /// Recompute and push flat query to backend. Call after
    /// `activeBuffer` or `chainCommits` changes. Canonical ` | `
    /// joiner is what Rust's chain parser recognises
    private func syncQuery() {
        let next: String
        if chainCommits.isEmpty {
            next = activeBuffer
        } else {
            next = chainCommits.joined(separator: " | ") + " | " + activeBuffer
        }
        update(query: next)
    }

    /// TextField -> VM bridge. Parses incoming string for a chain
    /// separator pipe (unless base uses literal pipe), auto-commits
    /// everything before it as a pill, and stores tail in
    /// `activeBuffer`
    ///
    /// Separator rule: a `|` whose neighbours include at least one
    /// whitespace character. Covers all natural typing rhythms:
    /// `foo | bar` (spaced), `foo| bar` (pipe-then-space),
    /// `foo |bar` (space-then-pipe), trailing `foo |` / `foo| `. A
    /// bare `foo|bar` (no surrounding whitespace at all) stays
    /// literal - that's shape regex alternation uses
    ///
    /// Idempotency: defensive strip of any existing flat-commit
    /// prefix in case field layer ever delivers stale text.
    /// NSTextField-backed `MainInputField` force-syncs `stringValue`
    /// so stale delivery shouldn't happen, but belt-and-braces for
    /// future input layers (accessibility tools, paste, etc.)
    func setActiveBuffer(_ s: String) {
        // Typing exits history recall - user has chosen to mutate
        // entry instead of continuing to walk. Recall state lives
        // separate from buffer so clearing it here doesn't touch
        // text that was just recalled into field
        clearHistoryRecall()
        var raw = s
        if !chainCommits.isEmpty {
            let canonical = chainCommits.joined(separator: " | ") + " | "
            let withoutTrailingSpace = String(canonical.dropLast())
            if raw.hasPrefix(canonical) {
                raw = String(raw.dropFirst(canonical.count))
            } else if raw.hasPrefix(withoutTrailingSpace) {
                raw = String(raw.dropFirst(withoutTrailingSpace.count))
            }
        }

        if let pipeIdx = Self.findSeparatorPipe(raw) {
            let before = String(raw[..<pipeIdx]).trimmingCharacters(in: .whitespaces)
            let afterStart = raw.index(after: pipeIdx)
            let after: String = afterStart < raw.endIndex
                ? String(raw[afterStart...]).drop(while: { $0.isWhitespace }).description
                : ""
            if !before.isEmpty
                && !Self.baseUsesLiteralPipe(effectiveBase(committing: before))
            {
                chainCommits.append(before)
                activeBuffer = after
                syncQuery()
                return
            }
        }

        guard activeBuffer != raw else { return }
        activeBuffer = raw
        syncQuery()
    }

    /// Index of first `|` that acts as a separator - i.e. has
    /// whitespace on at least one side, anywhere in string.
    /// Returns nil when every `|` is flanked by non-whitespace
    /// chars (regex-alternation shape, e.g. `cat|dog`)
    ///
    /// Public-to-tests so regression tests can pin rule without
    /// constructing full ViewModels
    static func findSeparatorPipe(_ s: String) -> String.Index? {
        var idx = s.startIndex
        while idx < s.endIndex {
            if s[idx] == "|" {
                let beforeIsWs = idx > s.startIndex
                    && s[s.index(before: idx)].isWhitespace
                let afterIdx = s.index(after: idx)
                let afterIsWs = afterIdx < s.endIndex
                    && s[afterIdx].isWhitespace
                if beforeIsWs || afterIsWs {
                    return idx
                }
            }
            idx = s.index(after: idx)
        }
        return nil
    }

    /// Opt| keyboard shortcut: unconditional commit. Bypasses
    /// pipe-using-keyword bail list so power users can chain from
    /// providers that normally keep `|` literal
    @discardableResult
    func commitActiveAsPill() -> Bool {
        guard viewMode == .main else { return false }
        let trimmed = activeBuffer.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return false }
        chainCommits.append(trimmed)
        activeBuffer = ""
        syncQuery()
        return true
    }

    /// Backspace-at-empty-active handler. Pops most recent pill
    /// back into editable buffer so user can continue editing it.
    /// No-op when active has text (normal backspace deletes chars)
    @discardableResult
    func popLastChainCommit() -> Bool {
        guard viewMode == .main else { return false }
        guard activeBuffer.isEmpty else { return false }
        guard let last = chainCommits.last else { return false }
        chainCommits.removeLast()
        activeBuffer = last
        syncQuery()
        return true
    }

    /// What counts as "the base" for pipe-using-keyword detection:
    /// First committed pill if there is one, else text about to be
    /// committed. Keeping check anchored to position 0 of whole
    /// chain means a user who committed `note hello` can still
    /// chain on from there even if a LATER segment happens to start
    /// with a pipe-using keyword
    private func effectiveBase(committing next: String) -> String {
        chainCommits.first ?? next
    }

    static func baseUsesLiteralPipe(_ base: String) -> Bool {
        let first = base.split(separator: " ").first.map(String.init) ?? ""
        return pipeUsingKeywords.contains(first.lowercased())
    }

    /// Wipe chain state. Called from `reset()` so a panel reopen
    /// starts fresh with no leftover pills
    func resetChainState() {
        chainCommits.removeAll()
        activeBuffer = ""
    }

    /// External query replacement: URL handlers, SetInput effects,
    /// Tab-completion paths, newnote prefilling, etc. Always wipes
    /// committed pills (those would be meaningless carry-over for a
    /// new command) and routes text through active buffer so
    /// TextField picks it up via its @Published binding
    ///
    /// Internal chain mutation (add pill, pop pill, typed keystroke)
    /// goes through `syncQuery()` directly - it preserves chainCommits
    func setInput(_ s: String) {
        chainCommits.removeAll()
        activeBuffer = s
        syncQuery()
    }


    func moveSelection(by delta: Int) {
        guard !results.isEmpty else { return }
        selectedIndex = max(0, min(results.count - 1, selectedIndex + delta))
    }

    func activateSelected() -> Bool {
        guard results.indices.contains(selectedIndex) else { return false }
        let cand = results[selectedIndex]
        // Record canonical query user actually committed to.
        // Only real activations land here - autocomplete / preview /
        // SetInput paths are handled separately so recall stays
        // free of half-typed noise
        bridge.recordQuery(query)
        let effect = bridge.activate(cand.id, action: "default")
        return dispatchEffect(effect)
    }

    /// Cmd1..cmd9 - jump to nth candidate (0-indexed) and activate
    /// it. Only meaningful in main mode. No-op on out-of-range
    /// index or while editor / actions view is up (those use
    /// dedicated shortcuts)
    @discardableResult
    func activateAtIndex(_ index: Int) -> Bool {
        guard viewMode == .main else { return false }
        guard results.indices.contains(index) else { return false }
        selectedIndex = index
        return activateSelected()
    }


    /// Enter actions-mode for currently-selected candidate. Does
    /// nothing if selection is empty or candidate has no actions
    ///
    /// Preview short-circuit: when candidate has exactly
    /// `[primary, preview]` as its two actions, -> jumps straight
    /// to preview instead of opening menu. That's single-keystroke
    /// UX QR / JSON / format-converter rows rely on - "-> shows me
    /// the thing"
    ///
    /// For candidates with 3+ actions (notes, multi-action clipboard
    /// rows), -> opens actions menu. Menu still contains Preview
    /// item; users can pick it explicitly, or use one of other
    /// actions they came for
    @discardableResult
    func enterActionsMode() -> Bool {
        guard results.indices.contains(selectedIndex) else { return false }
        let cand = results[selectedIndex]
        guard !cand.actions.isEmpty else { return false }
        let onlyPrimaryAndPreview =
            cand.actions.count == 2
            && cand.actions.first?.id == "default"
            && cand.actions.last?.id == "preview"
        if onlyPrimaryAndPreview {
            let effect = bridge.activate(cand.id, action: "preview")
            _ = dispatchEffect(effect)
            return true
        }
        viewMode = .actions(cand)
        actionIndex = 0
        return true
    }

    @discardableResult
    func exitActionsMode() -> Bool {
        guard case .actions = viewMode else { return false }
        viewMode = .main
        actionIndex = 0
        return true
    }

    /// <- handler - semantic "go back". Leaves actions list or
    /// inline preview (two modes that logically have a parent),
    /// no-ops in main / editor / AI modes
    @discardableResult
    func handleLeftKey() -> Bool {
        switch viewMode {
        case .actions:
            return exitActionsMode()
        case .imagePreview, .textPreview:
            viewMode = .main
            return true
        default:
            return false
        }
    }

    func moveActionSelection(by delta: Int) {
        guard case .actions(let cand) = viewMode, !cand.actions.isEmpty else { return }
        actionIndex = max(0, min(cand.actions.count - 1, actionIndex + delta))
    }

    func activateAction() -> Bool {
        guard case .actions(let cand) = viewMode else { return false }
        guard cand.actions.indices.contains(actionIndex) else { return false }
        let action = cand.actions[actionIndex]
        // Actions menu activations count as a commit too - same
        // rationale as `activateSelected`
        bridge.recordQuery(query)
        let effect = bridge.activate(cand.id, action: action.id)
        let dismiss = dispatchEffect(effect)
        // Only clear actions view if dispatchEffect didn't
        // transition us somewhere else (editor / image preview /
        // ...). Previously we unconditionally reset -> `.main`,
        // clobbering inline preview for QR's "Show QR" action
        if case .actions = viewMode {
            viewMode = .main
            actionIndex = 0
        }
        return dismiss
    }

    /// Cmd1..cmd9 jumps to nth action within actions menu and fires
    /// it. Mirrors main-mode quick-pick shortcut (cmd+N on result
    /// row n) so users never have to context-switch between
    /// gestures: cmd + digit always means "fire position N in
    /// whatever list is open"
    @discardableResult
    func activateActionAtIndex(_ index: Int) -> Bool {
        guard case .actions(let cand) = viewMode else { return false }
        guard cand.actions.indices.contains(index) else { return false }
        actionIndex = index
        return activateAction()
    }

    /// Runs effect via `EffectRunner`, except `.setInput`,
    /// `.askAi`, and `.editNote` - these keep panel open and are
    /// handled inline. Returns `true` when panel should dismiss
    /// after action
    private func dispatchEffect(_ effect: Effect?) -> Bool {
        guard let effect = effect else { return true }
        switch effect {
        case .setInput(let s):
            setInput(s)
            return false
        case .askAi(let question):
            // Route through `askAiSmart` so this path honours
            // `ai.router_enabled` same way cmd+return does. Without
            // this, pressing Enter on AI palette row went straight
            // to free-form Ask AI even with router turned on - user
            // would only see routing if they remembered cmd+return,
            // which is not discoverable
            askAiSmart(question, source: "activate-palette")
            return false
        case .aiTransform(let text, let instruction):
            startAiTransform(text: text, instruction: instruction)
            return false
        case .askAiThenPipe(let prompt, let instruction, let stages):
            // Pipe-from-AI: run AI call, then re-dispatch answer
            // through existing `pipeline::` activation handler.
            // Keeps panel-side flow unchanged - once answer lands,
            // resulting Effect (CopyToClipboard, ShowText, etc.)
            // runs through normal dispatch chain
            startAskAiThenPipe(prompt: prompt, instruction: instruction, stages: stages)
            return false
        case .editNote(let path):
            enterEditor(path: path)
            return false
        case .showImagePng(let b64):
            // Keep user in keyboard flow - render inline rather
            // than popping a window. ESC returns to main results
            viewMode = .imagePreview(base64: b64, label: "Preview")
            return false
        case .showText(let text, let label, let language, let editablePath):
            viewMode = .textPreview(
                text: text,
                label: label,
                language: language,
                editablePath: editablePath
            )
            return false
        default:
            EffectRunner.run(effect)
            return true
        }
    }


    /// Open editor on `path`. If file can't be read, enter editor
    /// mode anyway with an empty buffer so user can type from
    /// scratch - saving later will create file
    func enterEditor(path: String) {
        autosave?.cancel()
        editingContent = (try? noteIO.read(path: path)) ?? ""
        editingDirty = false
        editorStatus = .idle
        editorPreviewOpen = false
        viewMode = .editor(path)
    }

    /// User typed; update buffer, mark dirty, schedule autosave
    func updateEditorContent(_ newContent: String) {
        guard case .editor = viewMode else { return }
        editingContent = newContent
        editingDirty = true
        editorStatus = .idle
        scheduleAutoSave()
    }

    private func scheduleAutoSave() {
        autosave?.cancel()
        let task = DispatchWorkItem { [weak self] in
            self?.saveEditorContent()
        }
        autosave = task
        DispatchQueue.main.asyncAfter(
            deadline: .now() + .milliseconds(Self.autosaveDebounceMs),
            execute: task
        )
    }

    /// Flush buffer to disk immediately. No-op outside editor mode
    /// or when nothing has changed since last save
    @discardableResult
    func saveEditorContent() -> Bool {
        guard case .editor(let path) = viewMode else { return false }
        guard editingDirty else {
            editorStatus = .saved
            return true
        }
        editorStatus = .saving
        do {
            try noteIO.write(path: path, content: editingContent)
            editingDirty = false
            editorStatus = .saved
            return true
        } catch {
            editorStatus = .error(error.localizedDescription)
            return false
        }
    }

    /// Leave editor mode. Always flushes pending edits so user
    /// never loses data from an accidental ESC. Also clears query
    /// so user lands in a fresh "type your next command" state -
    /// requested UX: editor close should feel like starting over,
    /// not like dropping back into stale search results
    @discardableResult
    func exitEditor() -> Bool {
        guard case .editor = viewMode else { return false }
        if editingDirty { _ = saveEditorContent() }
        autosave?.cancel()
        autosave = nil
        viewMode = .main
        editingContent = ""
        editingDirty = false
        editorStatus = .idle
        editorPreviewOpen = false
        query = ""
        results = []
        selectedIndex = 0
        // Re-arm TextField focus: while editor was showing, query
        // TextField was absent from view hierarchy, so its
        // `@FocusState` binding dropped. Bumping focusTick re-fires
        // `onChange` listener in ContentView after viewMode flips
        // to `.main` and TextField is re-mounted. Without this user
        // has to click field to start typing after ESC / "save &
        // new command"
        focusTick &+= 1
        return true
    }

    /// Result of attempting to move/rename current editor note.
    /// Exposed so UI layer can surface errors with a helpful alert
    /// instead of silently failing
    enum RenameResult: Equatable {
        case ok(newPath: String)
        case notEditing
        case invalidPath(String)
        case conflict(String)
        case ioError(String)
    }

    /// Rename / relocate currently-edited note. `newRelative` is
    /// user-typed path relative to notes folder - we handle `.md`
    /// suffixing, parent directory creation, and escape protection
    ///
    /// On success returns `.ok(newPath)` and updates `viewMode` so
    /// editor keeps editing same buffer at its new location
    @discardableResult
    func renameCurrentNote(to newRelative: String) -> RenameResult {
        guard case .editor(let currentPath) = viewMode else { return .notEditing }
        if editingDirty { _ = saveEditorContent() }

        let root = bridge.notesFolder
        guard !root.isEmpty else { return .invalidPath("notes folder not configured") }

        let trimmed = newRelative.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return .invalidPath("empty path") }

        // Strip any leading slashes - paths are relative to notes root
        let stripped = trimmed.drop { $0 == "/" }
        var normalized = String(stripped)
        if !normalized.lowercased().hasSuffix(".md") {
            normalized += ".md"
        }

        // Reject `..` segments up front so a typo can't escape root
        let parts = normalized.split(separator: "/", omittingEmptySubsequences: true)
        if parts.contains(where: { $0 == ".." || $0 == "." }) {
            return .invalidPath("path cannot contain .. or . segments")
        }

        let rootURL = URL(fileURLWithPath: root)
        let destURL = rootURL.appendingPathComponent(normalized).standardizedFileURL
        // After standardization path must still
        // sit inside notes root
        if !destURL.path.hasPrefix(rootURL.standardizedFileURL.path) {
            return .invalidPath("path escapes notes folder")
        }

        // Same path -> treat as a no-op success so user's Enter
        // key doesn't feel broken
        if destURL.path == currentPath {
            return .ok(newPath: currentPath)
        }
        // Destination must not already exist - never clobber someone
        // else's note by accident
        if FileManager.default.fileExists(atPath: destURL.path) {
            return .conflict("a note already exists at \(normalized)")
        }

        do {
            try FileManager.default.createDirectory(
                at: destURL.deletingLastPathComponent(),
                withIntermediateDirectories: true
            )
            try FileManager.default.moveItem(atPath: currentPath, toPath: destURL.path)
        } catch {
            return .ioError(error.localizedDescription)
        }

        autosave?.cancel()
        autosave = nil
        viewMode = .editor(destURL.path)
        editingDirty = false
        editorStatus = .saved
        return .ok(newPath: destURL.path)
    }

    /// Cmd+return inside editor - commit buffer and signal caller
    /// to dismiss whole panel. Returns `true` when a dismiss should
    /// follow; `false` if we weren't in editor mode
    @discardableResult
    func saveEditorAndRequestClose() -> Bool {
        guard case .editor = viewMode else { return false }
        if editingDirty { _ = saveEditorContent() }
        autosave?.cancel()
        autosave = nil
        viewMode = .main
        editingContent = ""
        editingDirty = false
        editorStatus = .idle
        return true
    }

    /// Cmd+return entry point for AI command palette. Returns false
    /// on empty / outside main mode so KeyCatcher passes keystroke
    /// through
    @discardableResult
    func askAiPalette() -> Bool {
        guard viewMode == .main else { return false }
        let question = query.trimmingCharacters(in: .whitespaces)
        guard !question.isEmpty else { return false }
        askAiSmart(question, source: "cmd-enter")
        return true
    }

    /// Shared "do the smart thing with this AI question" entry
    /// point. Used by BOTH:
    /// - cmd+return via `askAiPalette` above
    /// - Enter on AI palette row via `dispatchEffect(.askAi)`
    ///
    /// When `ai.router_enabled = true`: switch to aiThinking view
    /// immediately (so panel reacts), kick off router, and on a
    /// successful tool pick rewind to keyword form via `setInput`
    /// so matching provider dispatches naturally. On router decline
    /// / timeout, fall through to free-form Ask AI with original
    /// text
    ///
    /// When router is off: free-form Ask AI immediately
    func askAiSmart(_ question: String, source: String = "activate") {
        #if !AI
        // No-AI build: nothing to do. Rust side won't emit AskAi
        // effects either (palette row is gated out), but function
        // still has to exist so dispatch switch compiles
        _ = (question, source)
        return
        #else
        let cfg = Config.load()
        if cfg.effectiveRouterEnabled {
            viewMode = .aiThinking(question)
            // Keep `self` weak across whole closure - unwrapping
            // pattern lives inside each `MainActor.run` block, so a
            // long-running router call that finishes after user
            // dismissed panel never resurrects a deallocated VM
            _ = source
            Task { [weak self] in
                // Refcount this entire task as "busy" so menu-bar
                // icon animates while router runs. `defer` fires on
                // every exit path - both early-return on a
                // successful tool pick and fallthrough into
                // `startAiQuery`, which spins up its own task with
                // its own begin/end pair
                BusyTracker.shared.begin()
                defer { BusyTracker.shared.end() }
                let call = await AiRouter.route(
                    question: question, tools: AiRouterTools.all
                )
                if let call = call,
                   case .success(let keyword) = AiRouterTools.render(call) {
                    await MainActor.run { [weak self] in
                        guard let self else { return }
                        self.viewMode = .main
                        self.setInput(keyword)
                    }
                    return
                }
                await MainActor.run { [weak self] in
                    guard let self else { return }
                    self.startAiQuery(question)
                }
            }
            return
        }
        startAiQuery(question)
        #endif
    }

    func startAiQuery(_ question: String) {
        #if !AI
        _ = question
        return
        #else
        viewMode = .aiThinking(question)
        // Keep `self` weak through whole closure: `MainActor.run`
        // blocks below each take their own weak unwrap, so a
        // long-running AiClient call that finishes after panel was
        // dismissed doesn't keep VM alive OR mutate stale
        // view-state. Post-await `Task.isCancelled` check lets a
        // panel-reset / new ask bail out instead of paying
        // rendering cost of writing into a state that's about to
        // be overwritten
        Task { [weak self] in
            BusyTracker.shared.begin()
            defer { BusyTracker.shared.end() }
            do {
                let answer = try await AiClient.ask(question)
                if Task.isCancelled { return }
                await MainActor.run { [weak self] in
                    guard let self else { return }
                    if case .aiThinking = self.viewMode {
                        self.viewMode = .aiResult(question, answer)
                    }
                }
            } catch {
                if Task.isCancelled { return }
                let msg = (error as? LocalizedError)?.errorDescription ?? String(describing: error)
                await MainActor.run { [weak self] in
                    guard let self else { return }
                    if case .aiThinking = self.viewMode {
                        self.viewMode = .aiError(question, msg)
                    }
                }
            }
        }
        #endif
    }

    /// Dispatch a preset transform (summarize / explain / translate
    /// / ...). Reuses same `.aiThinking` / `.aiResult` / `.aiError`
    /// view states as free-form asks - only system prompt differs.
    /// "question" stored with result is instruction, so header line
    /// reads "Summarize the following..." rather than echoing
    /// multi-line source text back at user
    func startAiTransform(text: String, instruction: String) {
        #if !AI
        _ = (text, instruction)
        return
        #else
        viewMode = .aiThinking(instruction)
        // Same weak-self + Task.isCancelled pattern as startAiQuery
        Task { [weak self] in
            BusyTracker.shared.begin()
            defer { BusyTracker.shared.end() }
            do {
                let answer = try await AiClient.transform(text: text, instruction: instruction)
                if Task.isCancelled { return }
                await MainActor.run { [weak self] in
                    guard let self else { return }
                    if case .aiThinking = self.viewMode {
                        self.viewMode = .aiResult(instruction, answer)
                    }
                }
            } catch {
                if Task.isCancelled { return }
                let msg = (error as? LocalizedError)?.errorDescription ?? String(describing: error)
                await MainActor.run { [weak self] in
                    guard let self else { return }
                    if case .aiThinking = self.viewMode {
                        self.viewMode = .aiError(instruction, msg)
                    }
                }
            }
        }
        #endif
    }

    /// Encode `data` as URL-safe base64 with no padding, matching
    /// shape Rust's `base64::URL_SAFE_NO_PAD` produces. Used to
    /// mint synthetic `pipeline::<base64>` activation ids on Swift
    /// side so answer from a pipe-from-AI run can re-enter existing
    /// pipeline-execute path without a Rust round-trip
    private static func base64UrlSafeNoPad(_ data: Data) -> String {
        var s = data.base64EncodedString()
        s = s.replacingOccurrences(of: "+", with: "-")
        s = s.replacingOccurrences(of: "/", with: "_")
        s = s.trimmingCharacters(in: CharacterSet(charactersIn: "="))
        return s
    }

    /// Pipe-from-AI driver. Runs AI call (free-form ask when
    /// `instruction == nil`, transform with preset instruction
    /// otherwise), takes answer, and re-dispatches it through
    /// existing `pipeline::` activation so chain's transforms and
    /// sink fire on AI output
    ///
    /// Doesn't switch `viewMode` to `.aiThinking` - pipe-from-AI
    /// rows are explicit user intent ("write a haiku | copy"), so
    /// only meaningful UI is menu-bar busy gradient (driven by
    /// BusyTracker) and whatever effect sink emits. Showing AI
    /// answer panel mid-pipeline would be confusing
    func startAskAiThenPipe(prompt: String, instruction: String?, stages: [String]) {
        #if !AI
        _ = (prompt, instruction, stages)
        return
        #else
        // Hide panel immediately - user committed via Enter,
        // visible feedback is menu-bar shimmer + final sink effect
        // (clipboard strobe, toast, etc.). Without this, launcher
        // would sit open showing stale results while backend AI
        // call ran for several seconds
        let panelDismissedSnapshot = self.viewMode
        Task { [weak self] in
            BusyTracker.shared.begin()
            defer { BusyTracker.shared.end() }
            do {
                let answer: String
                if let instruction = instruction {
                    answer = try await AiClient.transform(text: prompt, instruction: instruction)
                } else {
                    answer = try await AiClient.ask(prompt)
                }
                if Task.isCancelled { return }
                // Build a synthetic `pipeline::<base64>` activation
                // id with AI answer as source. Rust side's
                // `extract_pipeline_text` falls back to embedded
                // text when base id doesn't match a known prefix
                // (notes are only special-case there) - so
                // `aipipe::result` triggers embedded path cleanly
                let flatAnswer = answer.replacingOccurrences(of: "\n", with: " ")
                var payload = "aipipe::result\n"
                payload += flatAnswer
                payload += "\n"
                payload += stages.joined(separator: "\n")
                let id = "pipeline::" + Self.base64UrlSafeNoPad(Data(payload.utf8))
                await MainActor.run { [weak self] in
                    guard let self else { return }
                    let nextEffect = self.bridge.activate(id, action: "default")
                    if let next = nextEffect {
                        // Run via EffectRunner first (handles
                        // CopyToClipboard / ShowImagePng / shell
                        // side effects); side-effect-free /
                        // panel-state effects route through
                        // dispatchEffect
                        EffectRunner.run(next)
                        _ = self.dispatchEffect(next)
                    }
                    // Suppress UI changes if user dismissed panel
                    // meanwhile. Compare against snapshot: if
                    // viewMode hasn't drifted, leave it alone;
                    // dispatchEffect for ShowText/ShowImagePng
                    // changes viewMode itself, so dont override
                    _ = panelDismissedSnapshot
                }
            } catch {
                if Task.isCancelled { return }
                let msg = (error as? LocalizedError)?.errorDescription ?? String(describing: error)
                NSLog("askAiThenPipe failed: %@", msg)
            }
        }
        #endif
    }

    /// Copy current AI answer (if any) to pasteboard and return to main
    @discardableResult
    func copyAiAnswer() -> Bool {
        guard case .aiResult(_, let answer) = viewMode else { return false }
        EffectRunner.run(.copyToClipboard(answer))
        viewMode = .main
        return true
    }

    /// Cmd+N shortcut - drops user straight into note-create flow
    /// without needing to type `newnote` themselves. If they already
    /// have a filter typed it gets preserved as title so e.g.
    /// `note meet` + cmd+N becomes `newnote meet` ready to confirm
    @discardableResult
    func newNoteShortcut() -> Bool {
        guard viewMode == .main else { return false }
        let current = query.trimmingCharacters(in: .whitespaces)
        let title: String = {
            // Strip known note-list keywords so `note meet` -> title "meet"
            for kw in ["note ", "notes ", "n "] {
                if let rest = current.range(of: kw, options: [.anchored, .caseInsensitive]) {
                    return String(current[rest.upperBound...])
                        .trimmingCharacters(in: .whitespaces)
                }
            }
            if current == "note" || current == "notes" || current == "n" {
                return ""
            }
            return current
        }()
        setInput(title.isEmpty ? "newnote " : "newnote \(title)")
        return true
    }

    /// Tab-complete selected result. Guard used to be a `hint::`
    /// id-prefix check; it now keys off returned `Effect` instead.
    /// Any provider can emit `.setInput` to offer a
    /// completion (hints do, auto-close brackets do, future ones
    /// will), and a prefix-based allowlist broke the moment
    /// registry's `::`-prefix dispatch started colliding between
    /// providers. Non-completion effects fall through untouched -
    /// Tab stays a completion key, never a trigger for copy / run /
    /// open actions. Rust-side `activate` skips frecency recording
    /// for `.setInput`, so firing Tab here doesn't pollute visit
    /// cache
    @discardableResult
    func tryAutoComplete() -> Bool {
        guard viewMode == .main else { return false }
        // `!!` recall: Tab REPLACES buffer with last query rather
        // than appending ghost suffix verbatim (which would leave
        // literal " -> " sentinel in input). User typed `!!`, ghost
        // showed ` -> calc 2+2`, Tab -> buffer becomes `calc 2+2`
        // and they can edit before Enter
        if let recalled = Self.bangBangPayload(autocompleteSuffix) {
            activeBuffer = recalled
            autocompleteSuffix = ""
            syncQuery()
            return true
        }
        // Inline ghost takes priority - if theres a greyed tail
        // shown after cursor, Tab commits it by appending to query.
        // No FFI roundtrip needed; suffix was already computed when
        // results arrived
        if !autocompleteSuffix.isEmpty {
            // Commit ghost into active buffer so TextField picks it
            // up via its binding. syncQuery drives vm.query from
            // new active + existing commits
            activeBuffer += autocompleteSuffix
            syncQuery()
            return true
        }
        guard results.indices.contains(selectedIndex) else { return false }
        let cand = results[selectedIndex]
        // Real note rows get a dedicated Tab path: complete to full
        // title under current note keyword. Lets a user type
        // `note hel` -> Tab -> `note Hello World` -> keep editing
        // without losing keyword context. Synthetic rows (folder
        // headers, chain confirms, prompts, guidance rows) are
        // excluded - completing to their title would paste
        // "Hello - Preview" or similar non-filter text
        //
        // Chain context guard: when pills are already committed,
        // note-keyword prefix check below would match BASE segment
        // (a committed pill) and clobber chain by replacing whole
        // flat query with `note <title>`. Skip that path so Tab on
        // chain rows is just a no-op rather than a surprise
        if chainCommits.isEmpty,
           isRealNoteRow(cand),
           let completed = noteTabCompletion(for: cand)
        {
            setInput(completed)
            return true
        }
        // Pipeline / chain suggestion rows: Tab completes last
        // stage to selected row's canonical keyword. Lets a user
        // type `note helo | md` + Tab -> `note helo | md5`, same
        // rhythm as Tab-to-title on note rows. Works for both
        // pipeline:: rows (transforms/sinks) and chain:: rows
        // (classic base actions like Preview)
        if (cand.id.hasPrefix("pipeline::") || cand.id.hasPrefix("chain::")),
           let keyword = Self.chainCompletionKeyword(from: cand.title)
        {
            setActiveBuffer(keyword)
            return true
        }
        let effect = bridge.activate(cand.id, action: "default")
        if case .setInput(let s) = effect {
            setInput(s)
            return true
        }
        return false
    }

    /// Extract last arrow-joined segment of a chain/pipeline row
    /// title. Title format from Rust side is
    /// `"<base> - <stage1> -> <stage2> -> ... -> <last>"` - final
    /// segment is what user's Tab should complete to
    ///
    /// Public-for-tests so regressions on title format are caught
    /// without needing a full ViewModel wiring
    static func chainCompletionKeyword(from title: String) -> String? {
        guard let dot = title.range(of: " · ") else { return nil }
        let stages = title[dot.upperBound...]
        let segments = stages.components(separatedBy: " → ")
        guard let last = segments.last, !last.isEmpty else { return nil }
        return last
    }

    /// A regular note row (i.e. one pointing at a real `.md` path),
    /// not a synthetic helper notes provider emits (create row,
    /// folder header, chain confirm, guidance, etc.). Only real
    /// rows should trigger Tab-to-title autocomplete
    private func isRealNoteRow(_ cand: Candidate) -> Bool {
        guard cand.id.hasPrefix("note::") else { return false }
        for synthetic in [
            "note::chain::",
            "note::folder::",
            "note::openOrCreate::",
            "note::create::",
            "note::findnote::",
        ] {
            if cand.id.hasPrefix(synthetic) { return false }
        }
        return cand.id != "note::list-empty" && cand.id != "note::prompt-new"
    }

    /// Reconstruct query as `<keyword> <title> ` where keyword is
    /// whatever note-list prefix user started with. Returns nil if
    /// current query doesn't look like a note-list invocation (so
    /// Tab stays a no-op rather than doing something surprising in
    /// unrelated contexts)
    ///
    /// Returned string ends with a trailing space so user can
    /// immediately type `|` (or more filter text) without first
    /// having to press space. Small but measurable UX win for
    /// chain-heavy flow (`note helo` -> Tab -> `note helo ` -> `|`
    /// -> pill)
    private func noteTabCompletion(for cand: Candidate) -> String? {
        let lowered = query.lowercased()
        for prefix in ["notes all ", "note all ", "n all ",
                       "notes ", "note ", "n "]
        {
            if lowered.hasPrefix(prefix) {
                // Preserve case user typed for keyword, just
                // replace filter portion
                let keepPrefix = String(query.prefix(prefix.count))
                return "\(keepPrefix)\(cand.title) "
            }
        }
        // Bare `notes` / `note` / `n` (no trailing space) -> add
        // one so users get a ready-to-edit filter
        for kw in ["notes", "note", "n"] {
            if lowered == kw {
                return "\(kw) \(cand.title) "
            }
        }
        return nil
    }

    /// ESC handler. In non-main modes (actions / AI thinking /
    /// result / error / editor / preview) this returns to `.main`
    /// and swallows event. In `.main` it returns false so caller
    /// can dismiss panel
    @discardableResult
    func handleEscape() -> Bool {
        switch viewMode {
        case .actions, .aiThinking, .aiResult, .aiError, .imagePreview, .textPreview:
            viewMode = .main
            actionIndex = 0
            return true
        case .editor:
            // Two-level escape inside editor: if markdown preview
            // pane is visible, step back to editing first; another
            // ESC exits. Mirrors back-out pattern users already
            // expect from previews elsewhere
            if editorPreviewOpen {
                editorPreviewOpen = false
                return true
            }
            exitEditor()
            return true
        case .main:
            return false
        }
    }


    func handleUpKey() {
        if case .actions = viewMode {
            moveActionSelection(by: -1)
            return
        }
        // Main mode: if input is empty / only whitespace, Up
        // recalls past queries (shell-style). Otherwise - and when
        // history has nothing to recall - it keeps moving result
        // selection upward, same as before
        if historyRecallIsEligible() && recallPrev() {
            return
        }
        moveSelection(by: -1)
    }

    func handleDownKey() {
        if case .actions = viewMode {
            moveActionSelection(by: 1)
            return
        }
        // If user is currently walking history (via Up), Down walks
        // forward through it - including "past index 0" which
        // restores stashed original buffer
        if historyRecallIndex != nil {
            _ = recallNext()
            return
        }
        moveSelection(by: 1)
    }

    /// Up triggers recall only from a "nothing typed yet" state
    /// with nothing to walk through in dropdown - empty buffer, no
    /// committed pills, main view, AND `results` is empty
    ///
    /// Dropdown-empty check matters because empty-state discovery
    /// rows count as results: hijacking Up for history recall while
    /// a list is visible would mean user couldn't navigate that
    /// list at all. With discovery enabled, Up/Down walk curated
    /// tips; with discovery dismissed (`results` truly empty), Up
    /// falls through to history recall as before
    ///
    /// Once recall has actually started, keep walking regardless of
    /// snapshot - buffer mirrors whatever history entry we just
    /// dropped into it, so eligibility flips off on next keystroke
    /// without that escape hatch
    private func historyRecallIsEligible() -> Bool {
        guard viewMode == .main else { return false }
        if historyRecallIndex != nil { return true }
        return chainCommits.isEmpty
            && activeBuffer.trimmingCharacters(in: .whitespaces).isEmpty
            && results.isEmpty
    }

    /// Walk one step back through history. Returns true if
    /// something was actually recalled (caller swallows key), false
    /// if history is empty or we've hit oldest entry
    @discardableResult
    func recallPrev() -> Bool {
        guard !historySnapshot.isEmpty else { return false }
        if historyRecallIndex == nil {
            // First step: stash whatever user had typed so Down
            // can bring them back to it
            historyRecallStash = activeBuffer
            historyRecallIndex = 0
        } else if let idx = historyRecallIndex, idx + 1 < historySnapshot.count {
            historyRecallIndex = idx + 1
        } else {
            // Already at oldest entry - no further to walk
            return true
        }
        if let idx = historyRecallIndex {
            setInput(historySnapshot[idx])
        }
        return true
    }

    /// Walk one step forward through history. Past index 0 lands
    /// back at whatever user had originally typed (stashed buffer),
    /// which exits recall. Returns true if keystroke was consumed
    @discardableResult
    func recallNext() -> Bool {
        guard let idx = historyRecallIndex else { return false }
        if idx > 0 {
            let next = idx - 1
            historyRecallIndex = next
            setInput(historySnapshot[next])
            return true
        }
        // Walking off the top: restore stashed buffer and exit recall
        let stashed = historyRecallStash ?? ""
        historyRecallIndex = nil
        historyRecallStash = nil
        setInput(stashed)
        return true
    }

    /// Reset walking state without touching buffer. Call when user
    /// types, presses Enter, or otherwise commits to an input
    /// distinct from recalled entry
    func clearHistoryRecall() {
        historyRecallIndex = nil
        historyRecallStash = nil
    }

    /// If current input is just `!!` (shell-style "last command"),
    /// replace it with most recent recorded query AND run it. "Run"
    /// means: swap input, recompute results synchronously, then
    /// activate top-ranked row. Recall preview ghost-text in
    /// `updateFastPath` already showed user what was about to fire,
    /// so Enter doesn't need a second confirm round-trip
    ///
    /// Returns `.notBangBang` when buffer isn't `!!`, `.empty` when
    /// theres no recorded history yet (Enter falls through to
    /// normal activate), or `.ran(dismissPanel)` after recalled
    /// query's top result was activated. Caller maps bool out of
    /// `.ran` payload to decide window dismissal
    func expandAndRunBangBang() -> BangBangResult {
        guard chainCommits.isEmpty else { return .notBangBang }
        guard Self.isBangBang(activeBuffer) else { return .notBangBang }
        let last = bridge.lastQuery()
        guard !last.isEmpty else { return .empty }
        // Swap input to recalled query. `setInput` triggers same
        // update path a keystroke would, so `results` ends up
        // populated for recalled query (FFI `query` call inside
        // `updateFastPath` is synchronous on fast path)
        setInput(last)
        // With results now reflecting recalled query, run top one -
        // same code path as a normal Enter on input
        return .ran(dismissPanel: activateSelected())
    }

    enum BangBangResult {
        case notBangBang
        case empty
        case ran(dismissPanel: Bool)
    }

    /// Returns true if panel should dismiss after action
    func handleEnterKey() -> Bool {
        switch viewMode {
        case .actions:
            return activateAction()
        case .aiResult:
            return copyAiAnswer()
        case .aiThinking, .aiError:
            return false
        case .editor:
            // Inside editor Enter key is a literal newline -
            // handled by `TextEditor`, not here
            return false
        case .imagePreview:
            // Enter while viewing preview returns to main - gives
            // user a dedicated "done looking" key
            viewMode = .main
            return false
        case .textPreview(let text, _, _, let editablePath):
            // Enter in a preview means "commit primary action".
            // That's *copy* for transient conversions
            // (json/yaml/toml), but *edit* for previews that know
            // about a source file - user glanced, now they want to
            // change something
            if let path = editablePath {
                enterEditor(path: path)
                return false
            }
            EffectRunner.run(.copyToClipboard(text))
            viewMode = .main
            return true
        case .main:
            // Shell-style `!!` expansion: Enter on a lone `!!`
            // swaps buffer with last recorded query AND fires
            // top-ranked activation immediately. Ghost-text preview
            // in `updateFastPath` already showed user what was
            // queued up, so a second Enter to confirm would just be
            // friction. Tab still acts as "expand-but-dont-run"
            // for users who want to edit before launching
            switch expandAndRunBangBang() {
            case .ran(let dismiss):
                return dismiss
            case .empty, .notBangBang:
                break
            }
            return activateSelected()
        }
    }
}


/// Status shown in editor's bottom bar. `saving` is transient;
/// flipping to `saved` after a successful write lets UI show a
/// subtle "Saved" confirmation without blocking typing
enum EditorStatus: Equatable {
    case idle
    case saving
    case saved
    case error(String)
}

/// Narrow file-IO facade so unit tests can intercept read/write
/// without touching real filesystem. Default production
/// implementation wraps `String(contentsOfFile:)` /
/// `write(toFile:atomically:encoding:)`
protocol NoteIO {
    func read(path: String) throws -> String
    func write(path: String, content: String) throws
}

struct FileSystemNoteIO: NoteIO {
    func read(path: String) throws -> String {
        try String(contentsOfFile: path, encoding: .utf8)
    }
    func write(path: String, content: String) throws {
        // Ensure parent directory exists - `note new ...` paths
        // may point into a folder user hasn't created yet
        let url = URL(fileURLWithPath: path)
        let dir = url.deletingLastPathComponent()
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        try content.write(toFile: path, atomically: true, encoding: .utf8)
    }
}
