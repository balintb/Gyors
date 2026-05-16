import SwiftUI
import AppKit

/// Window-scoped cmd+shift+P catcher that stays alive for lifetime of
/// note editor view, regardless of whether editor or preview pane is
/// mounted. NSTextView inside MarkdownSyntaxTextView has its own
/// monitor but it disappears when view unmounts (preview mode swaps
/// it out), leaving cmd+shift+P as a one-way door. This monitor runs
/// alongside and covers gap
private struct NoteEditorKeyMonitor: NSViewRepresentable {
    let onCommandShiftP: () -> Void

    func makeCoordinator() -> Coordinator {
        Coordinator(callback: onCommandShiftP)
    }

    func makeNSView(context: Context) -> NSView {
        let view = NSView(frame: .zero)
        context.coordinator.install(windowProvider: { [weak view] in view?.window })
        return view
    }

    func updateNSView(_: NSView, context: Context) {
        context.coordinator.callback = onCommandShiftP
    }

    final class Coordinator {
        var callback: () -> Void
        private var monitor: Any?

        init(callback: @escaping () -> Void) {
            self.callback = callback
        }

        deinit {
            if let m = monitor { NSEvent.removeMonitor(m) }
        }

        func install(windowProvider: @escaping () -> NSWindow?) {
            monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) {
                [weak self] event in
                guard let self = self,
                      event.window === windowProvider() else { return event }
                let mods = event.modifierFlags
                if mods.contains(.command), mods.contains(.shift),
                   event.charactersIgnoringModifiers?.lowercased() == "p"
                {
                    self.callback()
                    return nil
                }
                return event
            }
        }
    }
}

/// Ask user for a new relative path, then dispatch rename. Extracted
/// so key handler stays readable. Blocks on alert (modal) so editor
/// doesn't keep autosaving into old path while user is deciding
@MainActor
private func runMoveNoteFlow(vm: GyorsViewModel, currentPath: String) {
    let root = vm.bridge.notesFolder
    let currentRelative: String
    if !root.isEmpty, let range = currentPath.range(of: root) {
        var rel = String(currentPath[range.upperBound...])
        rel = rel.drop { $0 == "/" }.description
        currentRelative = rel
    } else {
        currentRelative = (currentPath as NSString).lastPathComponent
    }

    let alert = NSAlert()
    alert.messageText = "Move note"
    alert.informativeText = """
        Enter the new path, relative to your notes folder. Use `/` to
        move into a subfolder. `.md` is appended if missing.
        """
    alert.alertStyle = .informational
    alert.addButton(withTitle: "Move")
    alert.addButton(withTitle: "Cancel")

    let input = NSTextField(frame: NSRect(x: 0, y: 0, width: 360, height: 24))
    input.stringValue = currentRelative
    input.placeholderString = "work/sprint-planning"
    alert.accessoryView = input
    alert.window.initialFirstResponder = input

    if alert.runModal() == .alertFirstButtonReturn {
        let result = vm.renameCurrentNote(to: input.stringValue)
        switch result {
        case .ok: break
        case .notEditing: break
        case .invalidPath(let why),
             .conflict(let why),
             .ioError(let why):
            let err = NSAlert()
            err.messageText = "Move failed"
            err.informativeText = why
            err.alertStyle = .warning
            err.addButton(withTitle: "OK")
            err.runModal()
        }
    }
}

/// Inline markdown editor. Switches panel into a tall editing view
/// bound to `GyorsViewModel.editingContent`; autosaves via VM's
/// debounced scheduler and exposes `cmd+S` for explicit saves
struct NoteEditorView: View {
    @ObservedObject var vm: GyorsViewModel
    let path: String
    let onDismiss: () -> Void
    @ObservedObject private var themeMgr = ThemeManager.shared
    @FocusState private var editorFocused: Bool

    private var theme: Theme { themeMgr.current }

    private var filename: String {
        (path as NSString).lastPathComponent
    }

    private var displayTitle: String {
        if let firstH1 = vm.editingContent
            .split(separator: "\n")
            .lazy
            .map({ $0.trimmingCharacters(in: .whitespaces) })
            .first(where: { $0.hasPrefix("# ") })
        {
            let title = String(firstH1.dropFirst(2)).trimmingCharacters(in: .whitespaces)
            if !title.isEmpty { return title }
        }
        return (filename as NSString).deletingPathExtension
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider().opacity(0.3)
            if vm.editorPreviewOpen {
                previewPane
            } else {
                editor
            }
            Divider().opacity(0.3)
            statusBar
        }
        // Lifecycle-bound NSEvent monitor: catches cmd+shift+P regardless
        // of which pane is visible. MarkdownSyntaxTextView has its own
        // monitor but only while mounted; in preview mode it's gone,
        // and without this, cmd+shift+P became a one-way door
        .background(NoteEditorKeyMonitor(
            onCommandShiftP: { vm.editorPreviewOpen.toggle() }
        ))
    }

    /// Full markdown rendering via shared `PreviewRenderer` - same
    /// pipeline as `->`-from-search preview, so headings, lists,
    /// fenced code blocks with syntax highlighting, and link
    /// formatting all work here too. Previously this pane used a bare
    /// `AttributedString(markdown:)` which only handled inline markup,
    /// leaving `# Header` / `- bullet` as literal text
    private var previewPane: some View {
        ScrollView {
            PreviewRenderer(text: vm.editingContent, language: "markdown", theme: theme)
                .padding(.horizontal, 20)
                .padding(.vertical, 14)
        }
        .frame(height: 460)
    }


    private var header: some View {
        HStack(spacing: 12) {
            Image(systemName: "doc.text.fill")
                .font(.system(size: 14, weight: .regular))
                .foregroundStyle(theme.tertiaryText)
                .frame(width: 20)
            VStack(alignment: .leading, spacing: 1) {
                Text(displayTitle)
                    .font(.system(size: 16, weight: .semibold))
                    .foregroundStyle(theme.primaryText)
                    .lineLimit(1)
                Text(filename)
                    .font(.system(size: 11))
                    .foregroundStyle(theme.secondaryText)
                    .lineLimit(1)
            }
            Spacer()
            VStack(alignment: .trailing, spacing: 2) {
                // Hints track current pane so user always sees *how to
                // get back*. In preview, esc/cmd+shift+P both return
                // to editor; in editing, esc saves & exits and
                // cmd+shift+P opens preview
                if vm.editorPreviewOpen {
                    Text("⎋ back to editor   ⌘⇧P back to editor")
                        .font(.system(size: 11))
                        .foregroundStyle(theme.tertiaryText)
                    Text("preview · read-only")
                        .font(.system(size: 11))
                        .foregroundStyle(theme.tertiaryText)
                } else {
                    Text("⌘S save   ⎋ save & new command")
                        .font(.system(size: 11))
                        .foregroundStyle(theme.tertiaryText)
                    Text("⌘↵ save & close   ⌘⇧M move   ⌘⇧P preview")
                        .font(.system(size: 11))
                        .foregroundStyle(theme.tertiaryText)
                }
            }
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
    }


    private var editor: some View {
        // Syntax-highlighted markdown editor. Wraps NSTextView so we
        // can paint per-range attributes (heading bold, bold
        // markers in accent, fenced code greyed out, etc.) - SwiftUI
        // TextEditor has no attributed-string editing API
        MarkdownSyntaxTextView(
            text: Binding(
                get: { vm.editingContent },
                set: { vm.updateEditorContent($0) }
            ),
            isFocused: editorFocused,
            theme: theme,
            onCommandS: { vm.saveEditorContent() },
            onCommandReturn: {
                if vm.saveEditorAndRequestClose() { onDismiss() }
            },
            onCommandShiftM: { runMoveNoteFlow(vm: vm, currentPath: path) },
            onCommandShiftP: { vm.editorPreviewOpen.toggle() }
        )
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .frame(height: 460)
        .onAppear {
            // One runloop hop so NSTextView has landed in responder
            // chain before we assert focus
            DispatchQueue.main.async { editorFocused = true }
        }
    }


    private var statusBar: some View {
        HStack(spacing: 8) {
            statusIndicator
            Spacer()
            Text(path)
                .font(.system(size: 10))
                .foregroundStyle(theme.tertiaryText)
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 10)
    }

    @ViewBuilder
    private var statusIndicator: some View {
        switch vm.editorStatus {
        case .idle:
            if vm.editingDirty {
                label(icon: "pencil", text: "Editing…", tint: theme.secondaryText)
            } else {
                label(icon: "checkmark.circle", text: "Up to date", tint: theme.tertiaryText)
            }
        case .saving:
            label(icon: "arrow.triangle.2.circlepath", text: "Saving…", tint: theme.secondaryText)
        case .saved:
            label(icon: "checkmark.circle.fill", text: "Saved", tint: theme.accent)
        case .error(let msg):
            label(icon: "exclamationmark.triangle.fill", text: "Save failed: \(msg)", tint: .orange)
        }
    }

    private func label(icon: String, text: String, tint: Color) -> some View {
        HStack(spacing: 6) {
            Image(systemName: icon)
                .font(.system(size: 11, weight: .semibold))
            Text(text)
                .font(.system(size: 11, weight: .medium))
        }
        .foregroundStyle(tint)
    }
}
