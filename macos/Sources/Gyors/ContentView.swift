import SwiftUI
import AppKit

struct ContentView: View {
    @ObservedObject var vm: GyorsViewModel
    let onDismiss: () -> Void
    @ObservedObject private var themeMgr = ThemeManager.shared
    /// True while user is holding Cmd. Drives cmd1..cmd9 badge
    /// overlay on result rows so quick-pick shortcuts are
    /// discoverable without cluttering default view. Tracked via
    /// NSEvent flags-changed monitor in `keyHandler`
    @State private var cmdHeld = false
    /// Whether theres content above/below current viewport in
    /// result list. Drives chevron edge hints so users know when
    /// list is deeper than what they can see
    @State private var scrollEdges = ScrollEdges(hasAbove: false, hasBelow: false)

    /// "Mouse has moved at least once since panel opened." Hover
    /// events only update selection while this is true
    ///
    /// Without this guard, opening launcher with cursor parked over
    /// an old row position causes SwiftUI to fire `.onHover` with
    /// `inside=true` the moment view paints, so cursor's resting
    /// position dictates initial selection - never what user
    /// wanted. Resetting on every `focusTick` (each `show()`) means
    /// a fresh open always starts at keyboard-determined selection,
    /// and very first physical mouse move arms hover handler
    @State private var hoverArmed: Bool = false
    /// Local NSEvent monitor that flips `hoverArmed = true` on
    /// first `mouseMoved` after each show. Stored so we can remove
    /// monitor once armed (no point in burning event taps rest of
    /// session)
    @State private var mouseMoveMonitor: Any? = nil

    private var theme: Theme { themeMgr.current }

    var body: some View {
        VStack(spacing: 0) {
            switch vm.viewMode {
            case .actions(let cand):
                actionsHeader(for: cand)
                Divider().opacity(0.3)
                actionsList(for: cand)
            case .aiThinking(let q):
                aiThinkingView(question: q)
            case .aiResult(let q, let a):
                aiResultView(question: q, answer: a)
            case .aiError(let q, let err):
                aiErrorView(question: q, error: err)
            case .editor(let path):
                NoteEditorView(vm: vm, path: path, onDismiss: onDismiss)
            case .imagePreview(let b64, let label):
                imagePreview(base64: b64, label: label)
            case .textPreview(let text, let label, let language, let editablePath):
                textPreview(
                    text: text,
                    label: label,
                    language: language,
                    editablePath: editablePath
                )
            case .main:
                mainInput
                if !vm.results.isEmpty {
                    Divider().opacity(0.3)
                    resultList
                } else if vm.isSearching {
                    // File search hits filesystem and can take
                    // 100-300 ms on a large notes folder. Without a
                    // placeholder row dropdown is empty + user has
                    // no idea launcher is doing work (inline
                    // ProgressView next to input is too easy to
                    // miss). This row matches result-list rendering
                    // so panel doesn't visibly jump height when
                    // results arrive
                    Divider().opacity(0.3)
                    searchingPlaceholder
                }
            }
        }
        .frame(width: 720)
        .fixedSize(horizontal: false, vertical: true)
        .background(themeBackground)
        .clipShape(RoundedRectangle(cornerRadius: theme.cornerRadius, style: .continuous))
        // Border overlay removed: panel's `NSVisualEffectView`
        // contentView is now rounded at same `cornerRadius` and
        // provides visual edge against desktop. A SwiftUI
        // `strokeBorder` on top of that rendered as a faint
        // double-line at corner radius (one stroke, plus
        // substrate's own rounded silhouette) - looked broken,
        // wasn't doing useful work.
        // Focus is driven through `MainInputField` via `vm.focusTick`.
        // SwiftUI's @FocusState isn't reliable for an NSView-backed
        // control; AppKit path in MainInputField calls
        // `makeFirstResponder` directly whenever focusTick changes
        .onExitCommand { onDismiss() }
        .onAppear { resetHoverArming() }
        .onDisappear { tearDownHoverArming() }
        // Each `show()` bumps focusTick - re-arm mouse-move guard
        // so a still-open NSHostingView (we reuse panel) returns
        // to same fresh-open state as a brand-new instance
        .onChange(of: vm.focusTick) { resetHoverArming() }
        .background(keyHandler)
        // Cmd+N in main mode -> jump straight into `newnote ` (with
        // any active filter carried over as starting title)
        .onKeyPress(.init("n"), phases: .down) { press in
            if press.modifiers.contains(.command) && vm.viewMode == .main {
                return vm.newNoteShortcut() ? .handled : .ignored
            }
            return .ignored
        }
        // Cmd1..cmd9 lives in KeyCatcher's NSEvent monitor now - it fires
        // window-wide so it works in actions mode too (no focus
        // anchor there for SwiftUI's `.onKeyPress`)
    }

    @ViewBuilder
    private var themeBackground: some View {
        // SwiftUI only paints *tint* layer. Blur substrate lives in
        // AppKit chain - `PanelController` sets panel's
        // `contentView` to an `NSVisualEffectView`. That's
        // configuration that reliably renders system blur on macOS
        // Tahoe; SwiftUI-internal `NSViewRepresentable` attempts at
        // sibling-layered blur silently zero out their bounds and
        // user sees no translucency
        //
        // Tint at `effectivePanelOpacity` (user override > theme
        // default) sits on top of that blur. Result row text and
        // glyphs composite at full alpha above tint, so
        // translucency only ever softens background, never
        // readability of what user is reading
        let cfg = Config.load()
        let opacity = cfg.effectivePanelOpacity ?? theme.backgroundOpacity
        let blurEnabled = cfg.effectivePanelBlur ?? theme.usesBlur

        if theme.usesSystemMaterial {
            // System theme: let panel substrate
            // (NSVisualEffectView, .hudWindow / .behindWindow)
            // render entire background. Dont paint anything in
            // SwiftUI - even `.thinMaterial` here would composite
            // on top of substrate and result reads as solid, which
            // is exactly "no translucency" bug we hit
            Color.clear
        } else if blurEnabled {
            // Blur active: tint shows through proportional to
            // `1 - opacity`, revealing desktop blur underneath
            Rectangle().fill(theme.panelTint.opacity(opacity))
        } else {
            // Blur disabled: paint tint fully opaque so blur
            // contentView underneath is hidden, matching "no blur"
            // intent of theme. (We can't remove NSVisualEffectView
            // at runtime without rebuilding panel, so we cover it
            // instead)
            Rectangle().fill(theme.panelTint)
        }
    }


    private var mainInput: some View {
        HStack(spacing: 8) {
            // Committed pills render first, tightly packed, so
            // cursor lives at their right edge - natural "next
            // stage starts here" position for a shell-style pipe
            // flow
            ForEach(Array(vm.chainCommits.enumerated()), id: \.offset) { _, commit in
                chainPill(commit)
                    .transition(
                        .asymmetric(
                            insertion: .scale(scale: 0.85).combined(with: .opacity),
                            removal: .opacity
                        )
                    )
            }

            ZStack(alignment: .leading) {
                // Ghost-text overlay. An invisible copy of ONLY
                // active segment measures its rendered width so
                // grey suffix sits exactly where cursor does.
                // Hit-test-disabled so clicks, selection, cmd+A,
                // opt+<-/-> etc. pass through untouched
                if !vm.autocompleteSuffix.isEmpty && !vm.activeBuffer.isEmpty {
                    HStack(spacing: 0) {
                        Text(vm.activeBuffer)
                            .foregroundStyle(.clear)
                        Text(vm.autocompleteSuffix)
                            .foregroundStyle(theme.tertiaryText)
                        Spacer(minLength: 0)
                    }
                    .font(.system(size: 22, weight: .light))
                    .allowsHitTesting(false)
                }

                // NSTextField-backed input. SwiftUI TextField's
                // binding-read lag would let stale pre-commit text
                // concatenate with fresh keystrokes (user typed
                // `|` -> pill committed, field still showed old
                // text, next char arrived appended: `note helo ||`).
                // MainInputField synchronously syncs stringValue
                // after every change, so a commit that zeroes
                // buffer is reflected BEFORE next keystroke can be
                // typed on top of stale content
                MainInputField(
                    text: Binding(
                        get: { vm.activeBuffer },
                        set: { vm.setActiveBuffer($0) }
                    ),
                    placeholder: vm.chainCommits.isEmpty ? "Type to search…" : "next stage…",
                    fontSize: 22,
                    textColor: NSColor(theme.primaryText),
                    focusTick: vm.focusTick,
                    onSubmit: { if vm.activateSelected() { onDismiss() } }
                )
            }
            if vm.isSearching {
                ProgressView()
                    .controlSize(.small)
                    .scaleEffect(0.8)
                    .transition(.opacity)
            }
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 18)
        .animation(.easeInOut(duration: 0.18), value: vm.chainCommits)
        .animation(.easeInOut(duration: 0.15), value: vm.isSearching)
    }

    /// A committed chain stage rendered as a solid, rounded token -
    /// visually distinct from active TextField so user sees at a
    /// glance what's "locked in" versus what they're still typing
    private func chainPill(_ text: String) -> some View {
        HStack(spacing: 6) {
            Text(text)
                .font(.system(size: 15, weight: .medium))
                .foregroundStyle(theme.primaryText)
                .lineLimit(1)
                .truncationMode(.tail)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
        .background(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(theme.accent.opacity(0.22))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .strokeBorder(theme.accent.opacity(0.35), lineWidth: 0.5)
        )
        // Cap individual pills so a giant paste doesn't blow input
        // layout; truncated pills still round-trip verbatim through
        // `flat`, which is what gets sent to backend
        .frame(maxWidth: 220, alignment: .leading)
        .fixedSize(horizontal: true, vertical: false)
        .help(text) // tooltip reveals full value when truncated
        .accessibilityLabel("chain stage: \(text)")
    }

    /// "Searching files..." row shown in place of result list while
    /// a file-search query is in flight and no rows have landed
    /// yet. Row has same padding + divider shape as real result
    /// rows so panel doesn't visibly jump when results arrive.
    /// Driven by `vm.isSearching` (cleared moment
    /// `updateFilesPath`'s background dispatch posts results back)
    private var searchingPlaceholder: some View {
        HStack(spacing: 12) {
            ProgressView()
                .controlSize(.small)
                .scaleEffect(0.8)
                .frame(width: 28, height: 28)
            VStack(alignment: .leading, spacing: 2) {
                Text("Searching files…")
                    .font(.body)
                    .foregroundColor(.primary)
                Text("Walking the configured folders")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            Spacer()
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        .transition(.opacity)
    }

    private var resultList: some View {
        // Scroll-position tracking for edge fade + chevron hints.
        // LazyVStack keeps row instantiation on-demand, so piping
        // hundreds of candidates in (e.g. clipboard history) stays
        // cheap - only visible rows are built
        //
        // macOS 14 doesn't have `onScrollGeometryChange`, so we
        // snoop content-Y via a 0-height GeometryReader sink on
        // LazyVStack and compare against known viewport height
        let viewportHeight = min(CGFloat(vm.results.count) * 48, 380)
        let contentHeight = CGFloat(vm.results.count) * 48
        return ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    // Enumerate array up front so each row captures
                    // its own static position. Earlier shape -
                    // `ForEach(vm.results)` + per-row
                    // `firstIndex(of: cand) ?? 0` - silently fell
                    // back to index 0 whenever lookup raced a
                    // results-set update or candidate's Equatable
                    // form drifted from what was in array. Hover
                    // handler then set `vm.selectedIndex = 0` and
                    // keyboard selection appeared to "jump back to
                    // top" mid-scroll. Capturing
                    // `(offset, candidate)` pairs takes lookup out
                    // of hot path entirely
                    ForEach(Array(vm.results.enumerated()), id: \.element.id) { index, cand in
                        ResultRow(
                            candidate: cand,
                            selected: index == vm.selectedIndex,
                            highlightQuery: vm.query,
                            // Cmd1..cmd9 hint shown while Cmd is held -
                            // only for first 9 rows (rest aren't
                            // reachable by number)
                            cmdIndex: (cmdHeld && index < 9) ? (index + 1) : nil
                        )
                            .contentShape(Rectangle())
                            .onContinuousHover { phase in
                                // Selection follows mouse so Tab/Enter
                                // always act on whichever row user
                                // is pointing at. `hoverArmed` guard
                                // suppresses very first hover event
                                // after each panel open (when
                                // SwiftUI fires `.active` because
                                // cursor is parked over row, not
                                // because user moved it)
                                guard hoverArmed else { return }
                                if case .active = phase {
                                    vm.selectedIndex = index
                                }
                            }
                            .onTapGesture {
                                vm.selectedIndex = index
                                if vm.activateSelected() { onDismiss() }
                            }
                    }
                }
                .background(
                    GeometryReader { geo in
                        Color.clear.preference(
                            key: ScrollOffsetKey.self,
                            value: -geo.frame(in: .named("resultScroll")).minY
                        )
                    }
                )
            }
            .coordinateSpace(name: "resultScroll")
            .onPreferenceChange(ScrollOffsetKey.self) { offset in
                // 4-pt tolerance swallows sub-pixel float noise so
                // hint doesn't flicker at exact edges
                let hasAbove = offset > 4
                let hasBelow = offset + viewportHeight < contentHeight - 4
                let next = ScrollEdges(hasAbove: hasAbove, hasBelow: hasBelow)
                if next != scrollEdges { scrollEdges = next }
            }
            .frame(height: viewportHeight)
            .overlay(alignment: .top) {
                if scrollEdges.hasAbove {
                    scrollEdgeHint(systemImage: "chevron.up", alignment: .top)
                }
            }
            .overlay(alignment: .bottom) {
                if scrollEdges.hasBelow {
                    scrollEdgeHint(systemImage: "chevron.down", alignment: .bottom)
                }
            }
            .onChange(of: vm.selectedIndex) { _, new in
                guard vm.results.indices.contains(new) else { return }
                let id = vm.results[new].id
                withAnimation(.easeOut(duration: 0.1)) {
                    proxy.scrollTo(id, anchor: .center)
                }
            }
            .onChange(of: vm.results) { _, _ in
                // Any result-set change could put list back at top
                // with new length; reset so a stale "has above"
                // doesn't linger over fresh content
                scrollEdges = ScrollEdges(hasAbove: false, hasBelow: false)
            }
        }
    }

    /// A subtle chevron hint with a gradient fade - non-interactive,
    /// purely a visual cue that more rows exist off-viewport. Placed
    /// at top or bottom of scroll view via .overlay
    private func scrollEdgeHint(systemImage: String, alignment: Alignment) -> some View {
        let gradient = LinearGradient(
            colors: [theme.panelTint.opacity(0.85), theme.panelTint.opacity(0)],
            startPoint: alignment == .top ? .top : .bottom,
            endPoint: alignment == .top ? .bottom : .top
        )
        return ZStack(alignment: alignment) {
            gradient
                .frame(height: 22)
                .allowsHitTesting(false)
            Image(systemName: systemImage)
                .font(.system(size: 10, weight: .bold))
                .foregroundStyle(theme.tertiaryText)
                .padding(alignment == .top ? .top : .bottom, 4)
                .allowsHitTesting(false)
        }
        .frame(maxWidth: .infinity)
        .transition(.opacity.animation(.easeInOut(duration: 0.12)))
    }


    private func actionsHeader(for cand: Candidate) -> some View {
        HStack(spacing: 12) {
            Image(systemName: "chevron.left")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(theme.tertiaryText)
                .frame(width: 20)
            IconView(kind: cand.iconKind, value: cand.iconValue)
                .frame(width: 24, height: 24)
            VStack(alignment: .leading, spacing: 1) {
                Text("Actions")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(theme.secondaryText)
                Text(cand.title)
                    .font(.system(size: 16, weight: .medium))
                    .foregroundStyle(theme.primaryText)
                    .lineLimit(1)
            }
            Spacer()
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
    }

    private func actionsList(for cand: Candidate) -> some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                ForEach(Array(cand.actions.enumerated()), id: \.element.id) { index, action in
                    ActionRow(
                        action: action,
                        selected: index == vm.actionIndex,
                        // Cmd-held hint mirrors main-list behaviour:
                        // Cmd1..cmd9 jumps to + activates matching
                        // action in menu. Discoverability for a
                        // shortcut that would otherwise be invisible
                        cmdIndex: (cmdHeld && index < 9) ? (index + 1) : nil
                    )
                        .contentShape(Rectangle())
                        .onTapGesture {
                            vm.actionIndex = index
                            if vm.activateAction() { onDismiss() }
                        }
                }
            }
        }
        .frame(height: min(CGFloat(cand.actions.count) * 40, 240))
    }

    private var keyHandler: some View {
        // In editor mode we must not swallow arrows, Enter, Tab etc.
        // - `TextEditor` needs them. Only ESC stays captured so user
        // can bail out of editor from anywhere inside it
        KeyCatcher(
            isEditor: { if case .editor = vm.viewMode { return true } else { return false } }(),
            onUp:     { vm.handleUpKey() },
            onDown:   { vm.handleDownKey() },
            onEnter:  { if vm.handleEnterKey() { onDismiss() } },
            onLeft:   { vm.handleLeftKey() },
            onRight:  { vm.enterActionsMode() },
            onEscape: { if !vm.handleEscape() { onDismiss() } },
            onTab:    { vm.tryAutoComplete() },
            onCmdChanged: { held in cmdHeld = held },
            // Cmd1..cmd9 must work in actions mode too, but SwiftUI's
            // `.onKeyPress` only fires when something has focus - and
            // actions mode has no TextField to hold focus. Route
            // digit through KeyCatcher's NSEvent monitor instead, so
            // it triggers regardless of focus state
            onCmdDigit: { digit in
                switch vm.viewMode {
                case .main:
                    if vm.activateAtIndex(digit - 1) { onDismiss() }
                case .actions:
                    if vm.activateActionAtIndex(digit - 1) { onDismiss() }
                default:
                    break
                }
            },
            // Backspace on an empty active segment pops last chain
            // pill back into editable text. Returning true here
            // swallows event; false lets TextField handle keystroke
            // normally (delete one character)
            onBackspace: { vm.popLastChainCommit() },
            // Cmd| commits current active text as a chain pill.
            // Exists as a keyboard gesture rather than a typed
            // character so `|` in legitimate input (regex
            // alternation, YAML block scalars, AI free text, shell
            // pipes, etc.) never accidentally triggers chaining
            onChainCommit: { vm.commitActiveAsPill() },
            // Cmd+Y - Quick Look selected row. Notes (.md) and file
            // results get native preview panel; other candidate
            // kinds have inline previews already or are not
            // previewable (apps, commands, hints)
            onQuickLook: {
                guard vm.viewMode == .main,
                      vm.results.indices.contains(vm.selectedIndex),
                      let url = quickLookURL(for: vm.results[vm.selectedIndex])
                else { return false }
                return QuickLookPresenter.shared.present(url)
            },
            // Cmd+return - AI command palette. Bypass any selected
            // row and fire AskAi with current input. Only applicable
            // in main mode; everywhere else catcher's `isEditor`
            // gate or lack of input makes this a no-op
            onCmdEnter: { vm.askAiPalette() }
        )
    }


    private func textPreview(
        text: String,
        label: String,
        language: String?,
        editablePath: String?
    ) -> some View {
        // Note previews rebind Enter to "open editor" since that's
        // natural next step after reading a note; transient
        // conversion previews keep Enter -> copy
        let hint = editablePath != nil ? "⏎ edit · ⎋ back" : "⏎ copy · ⎋ back"
        return VStack(alignment: .leading, spacing: 10) {
            HStack {
                Image(systemName: editablePath != nil ? "square.and.pencil" : "doc.text")
                    .foregroundStyle(theme.accent)
                Text(label)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(theme.secondaryText)
                Spacer()
                Text(hint)
                    .font(.system(size: 11))
                    .foregroundStyle(theme.tertiaryText)
            }
            ScrollView {
                PreviewRenderer(text: text, language: language, theme: theme)
            }
            // Cap at 60% of usable screen so tall notes scroll
            // instead of pushing panel off bottom edge. Fallback is
            // a sane 600 when theres no screen (headless tests) so
            // nothing blows up
            .frame(maxHeight: previewMaxHeight)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
    }

    private var previewMaxHeight: CGFloat {
        if let screen = NSScreen.main {
            return max(320, screen.visibleFrame.height * 0.6)
        }
        return 600
    }


    private func imagePreview(base64: String, label: String) -> some View {
        let img = Data(base64Encoded: base64).flatMap(NSImage.init(data:))
        return VStack(alignment: .leading, spacing: 10) {
            HStack {
                Image(systemName: "qrcode.viewfinder")
                    .foregroundStyle(theme.accent)
                Text(label)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(theme.secondaryText)
                Spacer()
                Text("⎋ close")
                    .font(.system(size: 11))
                    .foregroundStyle(theme.tertiaryText)
            }
            if let img = img {
                Image(nsImage: img)
                    .interpolation(.none) // crisp QR edges, no blur on scaling
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .frame(maxWidth: .infinity, maxHeight: 420)
                    .background(Color.white) // QR readers prefer stark contrast
                    .cornerRadius(theme.cornerRadius * 0.5)
            } else {
                Text("Couldn't decode image payload")
                    .font(.system(size: 12))
                    .foregroundStyle(.orange)
            }
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
    }


    private func aiThinkingView(question: String) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 10) {
                ProgressView().controlSize(.small).scaleEffect(0.8)
                Text("Thinking…")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(theme.secondaryText)
                Spacer()
                Text("⎋ cancel")
                    .font(.system(size: 11))
                    .foregroundStyle(theme.tertiaryText)
            }
            Text(question)
                .font(.system(size: 14))
                .foregroundStyle(theme.primaryText)
                .textSelection(.enabled)
                .lineLimit(4)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
    }

    private func aiResultView(question: String, answer: String) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Image(systemName: "sparkles")
                    .foregroundStyle(theme.accent)
                Text(question)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(theme.secondaryText)
                    .lineLimit(1)
                Spacer()
                Text("⏎ copy · ⎋ dismiss")
                    .font(.system(size: 11))
                    .foregroundStyle(theme.tertiaryText)
            }
            ScrollView {
                // Route through PreviewRenderer with markdown as
                // declared language: AI answers are almost always
                // markdown-shaped (lists, bold, `code`) and
                // this keeps one renderer for everything that
                // benefits from it. User can flip it off via
                // preview_markdown
                PreviewRenderer(text: answer, language: "markdown", theme: theme)
            }
            .frame(maxHeight: 420)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
    }

    private func aiErrorView(question: String, error: String) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)
                Text("AI error")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(theme.secondaryText)
                Spacer()
                Text("⎋ dismiss")
                    .font(.system(size: 11))
                    .foregroundStyle(theme.tertiaryText)
            }
            Text(question)
                .font(.system(size: 12))
                .foregroundStyle(theme.secondaryText)
                .lineLimit(2)
            ScrollView {
                Text(error)
                    .font(.system(size: 13))
                    .foregroundStyle(theme.primaryText)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(maxHeight: 240)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
    }

    // Suppress hover-driven selection until user physically moves
    // mouse. Without this, opening panel with cursor parked over a
    // row paints that row as selected - never what user typed in
    // for. Re-armed on every panel show via `vm.focusTick`

    /// Reset hover-arming state and install a one-shot mouse-moved
    /// monitor that flips `hoverArmed = true` on first physical
    /// movement. Monitor self-uninstalls once armed, so we dont
    /// keep a system-wide event tap alive for rest of session
    private func resetHoverArming() {
        hoverArmed = false
        // Uninstall any earlier monitor before
        // installing a new one. `onChange(focusTick)` could fire
        // before `onDisappear` runs in a panel-already-visible
        // re-show, leaving two monitors in flight
        tearDownHoverArming()
        mouseMoveMonitor = NSEvent.addLocalMonitorForEvents(matching: .mouseMoved) { event in
            // Defer state mutation to main runloop so we dont
            // reenter SwiftUI's update cycle from inside an event
            // handler. Cheap; one-shot
            DispatchQueue.main.async {
                hoverArmed = true
                tearDownHoverArming()
            }
            return event
        }
    }

    private func tearDownHoverArming() {
        if let monitor = mouseMoveMonitor {
            NSEvent.removeMonitor(monitor)
            mouseMoveMonitor = nil
        }
    }
}

struct ResultRow: View {
    let candidate: Candidate
    let selected: Bool
    /// User's live input - used to visually separate "what they
    /// typed" from "what command will become" in rows whose title
    /// begins with query (autocomplete hints are common case)
    let highlightQuery: String
    /// When user is holding cmd, each of first 9 rows gets a badge
    /// showing quick-pick shortcut (cmd1..cmd9). `nil` hides badge
    /// (default state)
    let cmdIndex: Int?
    @ObservedObject private var themeMgr = ThemeManager.shared

    private var theme: Theme { themeMgr.current }

    /// Whether this row is synthetic "Ask AI" command-palette entry
    /// orchestrator injects when theres a non-empty query. Drives
    /// muted styling + dynamic subtitle that names provider that
    /// will actually run
    private var isAiPalette: Bool {
        candidate.id.hasPrefix("ai_palette::")
    }

    /// Subtitle override for palette row. Replaces static "Apple
    /// Intelligence - cmd+return" baked in by Rust orchestrator
    /// with live provider name (whatever's actually configured -
    /// Ollama, OpenAI, etc - falling back to Apple Intelligence on
    /// macOS 26+)
    private var displaySubtitle: String {
        guard isAiPalette else { return candidate.subtitle }
        #if AI
        return "\(AiClient.effectiveProviderLabel()) · cmd+return"
        #else
        // Rust side already gates palette row out of result lists
        // when AI is off, so this branch is unreachable in
        // practice. Returning raw subtitle keeps function total
        // without referencing absent AiClient
        return candidate.subtitle
        #endif
    }

    var body: some View {
        HStack(spacing: 12) {
            IconView(kind: candidate.iconKind, value: candidate.iconValue)
                .frame(width: 28, height: 28)
                .opacity(isAiPalette && !selected ? 0.65 : 1.0)

            VStack(alignment: .leading, spacing: 2) {
                titleView
                    .lineLimit(1)
                if !displaySubtitle.isEmpty {
                    Text(displaySubtitle)
                        .font(.system(size: 11))
                        .foregroundStyle(theme.secondaryText)
                        .lineLimit(1)
                }
            }
            Spacer()
            if let n = cmdIndex {
                // Flat filled chip - a 0.5 pt overlay stroke
                // rendered inconsistently across Retina pixel grid,
                // so we lean on a slightly higher-opacity fill to
                // carry accent instead. Clean edges on every DPI
                Text("cmd\(n)")
                    .font(.system(size: 11, weight: .semibold, design: .monospaced))
                    .foregroundStyle(theme.primaryText)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(
                        RoundedRectangle(cornerRadius: 5, style: .continuous)
                            .fill(theme.accent.opacity(0.32))
                    )
            } else if candidate.actions.count > 1 {
                Image(systemName: "chevron.right")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(theme.tertiaryText)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .frame(height: 48)
        .background(selected ? theme.accent.opacity(theme.selectionOpacity) : Color.clear)
    }

    /// Render title with typed prefix highlighted. Falls back to a
    /// plain title when no case-insensitive prefix match exists
    /// (e.g., file-content matches, non-keyword candidates, etc.)
    @ViewBuilder
    private var titleView: some View {
        if isAiPalette {
            // Muted - palette row is a fallback offer, not a peer
            // of real provider hits. Keeping title in secondary
            // text makes visual hierarchy obvious: real matches
            // above, "ask AI as a last resort" below. When
            // auto-promoted to top (no real matches), selection
            // background still pops it forward
            Text(candidate.title)
                .font(.system(size: 14, weight: .regular))
                .foregroundStyle(selected ? theme.primaryText : theme.secondaryText)
        } else if let split = TitleHighlight.split(title: candidate.title, query: highlightQuery) {
            (
                Text(split.typed)
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundColor(theme.primaryText)
                + Text(split.rest)
                    .font(.system(size: 14, weight: .regular))
                    .foregroundColor(theme.secondaryText)
            )
        } else {
            Text(candidate.title)
                .font(.system(size: 14, weight: .medium))
                .foregroundStyle(theme.primaryText)
        }
    }
}

struct ActionRow: View {
    let action: CandidateAction
    let selected: Bool
    let cmdIndex: Int?
    @ObservedObject private var themeMgr = ThemeManager.shared

    private var theme: Theme { themeMgr.current }

    var body: some View {
        HStack(spacing: 10) {
            Text(action.label)
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(theme.primaryText)
            Spacer()
            if let n = cmdIndex {
                Text("cmd\(n)")
                    .font(.system(size: 11, weight: .semibold, design: .monospaced))
                    .foregroundStyle(theme.primaryText)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(
                        RoundedRectangle(cornerRadius: 5, style: .continuous)
                            .fill(theme.accent.opacity(0.32))
                    )
            }
        }
        .padding(.horizontal, 24)
        .padding(.vertical, 10)
        .frame(height: 40)
        .background(selected ? theme.accent.opacity(theme.selectionOpacity) : Color.clear)
    }
}

/// Which edges of result scroll view have content beyond them.
/// Equatable so edge-tracking loop can de-dupe redraws
private struct ScrollEdges: Equatable {
    let hasAbove: Bool
    let hasBelow: Bool
}

/// Snoops LazyVStack's Y offset within ScrollView's coordinate
/// space, turning raw frame position into a preference value that
/// `.onPreferenceChange` can listen on. Needed because
/// `onScrollGeometryChange` is macOS 15+ and we target 14
private struct ScrollOffsetKey: PreferenceKey {
    static var defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = nextValue()
    }
}

struct KeyCatcher: NSViewRepresentable {
    let isEditor: Bool
    let onUp: () -> Void
    let onDown: () -> Void
    let onEnter: () -> Void
    /// <-/-> are dual-purpose: in actions/preview modes they
    /// navigate (back / open-actions); in main mode they have to be
    /// a no-op so focused NSTextField can move its cursor. Handler
    /// returns `true` only when it actually consumed key - `false`
    /// falls through and TextField's caret moves as expected. Same
    /// shape as `onBackspace`
    let onLeft: () -> Bool
    let onRight: () -> Bool
    let onEscape: () -> Void
    let onTab: () -> Void
    /// Fires on Cmd press/release so ContentView can toggle
    /// cmd1..cmd9 badges live. No handler = no flagsChanged
    /// subscription
    let onCmdChanged: ((Bool) -> Void)?
    /// Cmd1..cmd9 at window level - fires regardless of SwiftUI
    /// focus state so it works in actions mode (no TextField to
    /// anchor `.onKeyPress`)
    let onCmdDigit: ((Int) -> Void)?
    /// Backspace intercept. Returns true when handler consumed
    /// event (e.g. popped a chain pill back into active field).
    /// False falls through to TextField's normal backspace
    /// behaviour - deleting a character
    let onBackspace: (() -> Bool)?
    /// Cmd| (Cmd + Shift + Backslash -> pipe character). Explicit
    /// keyboard gesture to commit current active text as a chain
    /// pill. A gesture rather than a typed character so literal `|`
    /// in user input never accidentally chains
    let onChainCommit: (() -> Bool)?
    /// Cmd+Y - Quick Look selected result. Returns true when
    /// preview was shown (selected row had a file URL), false to
    /// let keystroke fall through for text input
    let onQuickLook: (() -> Bool)?
    /// Cmd+return - invoke AI command palette with current input,
    /// regardless of which row is selected. Returns true when query
    /// was actually dispatched (input is non-empty in main mode);
    /// false falls back to whatever platform would do with keystroke
    let onCmdEnter: (() -> Bool)?

    init(
        isEditor: Bool,
        onUp: @escaping () -> Void,
        onDown: @escaping () -> Void,
        onEnter: @escaping () -> Void,
        onLeft: @escaping () -> Bool,
        onRight: @escaping () -> Bool,
        onEscape: @escaping () -> Void,
        onTab: @escaping () -> Void,
        onCmdChanged: ((Bool) -> Void)? = nil,
        onCmdDigit: ((Int) -> Void)? = nil,
        onBackspace: (() -> Bool)? = nil,
        onChainCommit: (() -> Bool)? = nil,
        onQuickLook: (() -> Bool)? = nil,
        onCmdEnter: (() -> Bool)? = nil
    ) {
        self.isEditor = isEditor
        self.onUp = onUp
        self.onDown = onDown
        self.onEnter = onEnter
        self.onLeft = onLeft
        self.onRight = onRight
        self.onEscape = onEscape
        self.onTab = onTab
        self.onCmdChanged = onCmdChanged
        self.onCmdDigit = onCmdDigit
        self.onBackspace = onBackspace
        self.onChainCommit = onChainCommit
        self.onQuickLook = onQuickLook
        self.onCmdEnter = onCmdEnter
    }

    func makeNSView(context: Context) -> KeyCatcherView {
        let v = KeyCatcherView()
        apply(to: v)
        return v
    }

    func updateNSView(_ nsView: KeyCatcherView, context: Context) {
        apply(to: nsView)
    }

    private func apply(to v: KeyCatcherView) {
        v.isEditor = isEditor
        v.onUp = onUp
        v.onDown = onDown
        v.onEnter = onEnter
        v.onLeft = onLeft
        v.onRight = onRight
        v.onEscape = onEscape
        v.onTab = onTab
        v.onCmdChanged = onCmdChanged
        v.onCmdDigit = onCmdDigit
        v.onBackspace = onBackspace
        v.onChainCommit = onChainCommit
        v.onQuickLook = onQuickLook
        v.onCmdEnter = onCmdEnter
    }
}

final class KeyCatcherView: NSView {
    var isEditor: Bool = false
    var onUp:         (() -> Void)?
    var onDown:       (() -> Void)?
    var onEnter:      (() -> Void)?
    var onLeft:       (() -> Bool)?
    var onRight:      (() -> Bool)?
    var onEscape:     (() -> Void)?
    var onTab:        (() -> Void)?
    var onCmdChanged: ((Bool) -> Void)?
    var onCmdDigit: ((Int) -> Void)?
    var onBackspace:  (() -> Bool)?
    var onChainCommit: (() -> Bool)?
    var onQuickLook: (() -> Bool)?
    var onCmdEnter: (() -> Bool)?
    private var monitor: Any?
    private var flagsMonitor: Any?
    private var cmdCurrentlyHeld = false

    override var acceptsFirstResponder: Bool { false }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if let monitor = monitor {
            NSEvent.removeMonitor(monitor)
            self.monitor = nil
        }
        if let flagsMonitor = flagsMonitor {
            NSEvent.removeMonitor(flagsMonitor)
            self.flagsMonitor = nil
        }
        guard window != nil else { return }

        // Track cmd press/release so ContentView can show/hide
        // cmd1..cmd9 badges. Cheap - flagsChanged fires per
        // modifier edge, not per keystroke
        flagsMonitor = NSEvent.addLocalMonitorForEvents(matching: .flagsChanged) { [weak self] event in
            guard let self = self, event.window === self.window else { return event }
            let held = event.modifierFlags.contains(.command)
            if held != self.cmdCurrentlyHeld {
                self.cmdCurrentlyHeld = held
                self.onCmdChanged?(held)
            }
            return event
        }

        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self = self, event.window === self.window else { return event }
            let userModifiers: NSEvent.ModifierFlags =
                [.command, .shift, .option, .control]
            // In editor mode we only intercept ESC so user can
            // always bail out. Everything else (arrows, enter, tab,
            // typing) must flow through to NSTextView-backed
            // SwiftUI TextEditor
            if self.isEditor {
                if event.keyCode == 53 && event.modifierFlags.intersection(userModifiers).isEmpty {
                    self.onEscape?()
                    return nil
                }
                return event
            }
            // Cmd-only + digit -> fire nth entry in whatever list
            // is open (main results, or actions menu). Window-level
            // so focus state doesn't matter - needed because
            // actions mode has no focused TextField for
            // `.onKeyPress` to anchor on
            if event.modifierFlags.intersection(userModifiers) == .command,
               let ch = event.charactersIgnoringModifiers?.first,
               let digit = ch.wholeNumberValue,
               digit >= 1, digit <= 9
            {
                if let handler = self.onCmdDigit {
                    handler(digit)
                    return nil
                }
            }
            // Opt| (Option+Shift+Backslash -> pipe char with Option
            // held): power-user "commit now, bypass bail list"
            // gesture. Also accept cmd| for users who instinctively
            // reach for Command. Match via
            // `charactersIgnoringModifiers` so layouts where `|`
            // isn't Shift+Backslash still work - key test is: user
            // pressed key that WOULD produce `|` (after shift/AltGr),
            // plus Command or Option as gate
            //
            // Rationale for having a gesture at all: typed `|` is
            // bail-listed for pipe-using providers (regex, YAML,
            // AI ...), so a user who WANTS to chain from those
            // needs an out. Gesture bypasses bail list
            if let chars = event.charactersIgnoringModifiers, chars == "|" {
                let mods = event.modifierFlags.intersection([.command, .option, .control])
                if mods == .command || mods == .option {
                    if let handler = self.onChainCommit, handler() {
                        return nil
                    }
                }
            }
            // Cmd+Y - Quick Look selected row. Finder's convention.
            // For file-bearing rows (notes, files) this pops native
            // preview panel. For everything else we silently
            // decline - caller's handler checks applicability
            if event.modifierFlags.contains(.command),
               let chars = event.charactersIgnoringModifiers,
               chars == "y"
            {
                if let handler = self.onQuickLook, handler() {
                    return nil
                }
            }
            // Cmd+return - AI command palette. Bypass any selected
            // row and route current input straight to configured AI
            // provider. Intent is "I typed something, AI just
            // figure it out" without user having to walk to bottom
            // Ask AI row. Match Return (36) and numeric-keypad
            // Enter (76); modifier check uses .command alone (no
            // .shift / .option) so we dont fight cmd+shift+Enter
            // if user has another binding wired there in future
            if event.modifierFlags.contains(.command),
               event.keyCode == 36 || event.keyCode == 76
            {
                if let handler = self.onCmdEnter, handler() {
                    return nil
                }
            }
            if !event.modifierFlags.intersection(userModifiers).isEmpty {
                return event
            }
            switch event.keyCode {
            case 125: self.onDown?();   return nil
            case 126: self.onUp?();     return nil
            case 36, 76: self.onEnter?(); return nil
            // <- - only swallow when handler claims it (back-out
            // of actions / preview). In main mode it returns false
            // and event falls through so focused NSTextField can
            // move its caret left
            case 123:
                if let handler = self.onLeft, handler() { return nil }
                return event
            // -> - additionally guarded by caret position. Opening
            // actions menu would feel wrong when user is mid-edit
            // and just wants to walk caret rightward through their
            // query, so we only call onRight when:
            //   - input field isn't first responder (e.g.
            //     actions/preview mode), OR
            //   - caret is parked at end of text with no active
            //     selection.
            // Anything else passes through and NSTextField moves
            // caret / collapses selection in standard way
            case 124:
                if let editor = self.window?.firstResponder as? NSTextView {
                    let textLength = (editor.string as NSString).length
                    let range = editor.selectedRange()
                    if !CaretPosition.caretIsAtEnd(
                        textLength: textLength,
                        selectedRange: range
                    ) {
                        return event
                    }
                }
                if let handler = self.onRight, handler() { return nil }
                return event
            case 53:  self.onEscape?(); return nil
            case 48:  self.onTab?();    return nil
            // Backspace: consult handler first. It swallows event
            // only when it actually did something (chain pill
            // popped); otherwise we fall through to `return event`
            // so focused TextField deletes characters normally
            case 51:
                if let handler = self.onBackspace, handler() {
                    return nil
                }
                return event
            default:  return event
            }
        }
    }

    deinit {
        if let monitor = monitor { NSEvent.removeMonitor(monitor) }
        if let flagsMonitor = flagsMonitor { NSEvent.removeMonitor(flagsMonitor) }
    }
}
