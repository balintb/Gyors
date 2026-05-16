import AppKit
import SwiftUI

/// Rich install preview for a plugin arriving via `gyors://plugin/install`
/// or a `.gyorsplugin` file drop
///
/// This is not an `NSAlert` - alerts are fine for yes/no prompts,
/// but installing a plugin means running someone else's code, so
/// user needs to SEE full command + metadata before they click
/// Install. SwiftUI layout makes every field legible and
/// colour-codes validation problems so user never installs
/// something that's broken or that shadows a built-in
///
/// Flow:
///   URL handler / file opener -> `PluginInstallPanel.present(specJson:)`
///     -> Rust preview (validate + detect existing version)
///     -> SwiftUI sheet with install button gated by validation
///     -> on confirm: Rust commit (upsert plugins.json)
///     -> Notification + hint to restart
enum PluginInstallPanel {
    /// Main entry. `specJson` may be either a bare `ShellPluginSpec`
    /// JSON or a full `.gyorsplugin` manifest; Rust preview API
    /// accepts both
    @MainActor
    static func present(specJson: String, onCompletion: (() -> Void)? = nil) {
        guard let previewJson = callPreview(specJson) else {
            showGenericError(message: """
                This plugin manifest couldn't be parsed.

                Check that the JSON is well-formed and declares a \
                shell plugin (`shell:` block with id / keywords / command).
                """)
            onCompletion?()
            return
        }

        guard let preview = PluginInstallPreview.decode(from: previewJson) else {
            showGenericError(message: "Install preview decode failed. Please report.")
            onCompletion?()
            return
        }

        // Shell-mode plugins run command output as
        // /bin/sh. User MUST see a stronger warning and
        // affirmatively confirm by typing plugin's id before
        // install panel even appears. Other on_activate modes
        // (copy / open / quicklook / none) keep existing flow -
        // they dont widen shell attack surface
        if preview.spec.onActivate == "shell" {
            guard confirmShellPluginById(spec: preview.spec) else {
                onCompletion?()
                return
            }
        }

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 520, height: 560),
            styleMask: [.titled, .closable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.isMovableByWindowBackground = true
        window.level = .modalPanel
        window.center()

        let view = PluginInstallPanelView(
            preview: preview,
            specJson: specJson,
            onInstall: { result in
                window.close()
                switch result {
                case .success(let outcome):
                    showInstallSuccess(name: preview.spec.name, outcome: outcome)
                case .failure(let error):
                    showInstallError(message: error)
                }
                onCompletion?()
            },
            onCancel: {
                window.close()
                onCompletion?()
            }
        )
        window.contentView = NSHostingView(rootView: view)
        NSApp.activate(ignoringOtherApps: true)
        window.makeKeyAndOrderFront(nil)
    }

    // MARK: FFI

    private static func callPreview(_ specJson: String) -> String? {
        var out: String?
        specJson.withCString { ptr in
            if let raw = gyors_plugin_install_preview(ptr) {
                out = String(cString: raw)
                gyors_free_string(raw)
            }
        }
        return out
    }

    static func callCommit(_ specJson: String) -> CommitResult {
        var out = CommitResult.failure("no response from backend")
        specJson.withCString { ptr in
            if let raw = gyors_plugin_install_commit(ptr) {
                let json = String(cString: raw)
                gyors_free_string(raw)
                out = CommitResult.decode(from: json)
                    ?? CommitResult.failure("unparseable commit result: \(json)")
            }
        }
        return out
    }

    // MARK: Alerts

    /// Gate shell-mode plugin installs behind a typed
    /// confirmation. User must enter plugin's `id` exactly into
    /// accessory text field; "Install" stays disabled until typed
    /// text matches. Full `command` template is shown above field
    /// so user actually reads it before consenting
    ///
    /// Returns `true` when user confirmed with a matching id;
    /// `false` if they cancelled or dialog dismissed without a
    /// match. Caller bails out on `false`
    @MainActor
    static func confirmShellPluginById(spec: PluginSpec) -> Bool {
        let alert = NSAlert()
        alert.messageText = "This plugin runs shell commands"
        alert.informativeText = """
            Install only from sources you trust. The plugin "\(spec.name)" \
            (id: \(spec.id)) will run the command below as /bin/sh whenever \
            you activate one of its keywords.

            Command template:
              \(spec.command)

            Type the plugin's id below to confirm.
            """
        alert.alertStyle = .warning
        let installButton = alert.addButton(withTitle: "Continue")
        alert.addButton(withTitle: "Cancel")
        // Continue button has to stay disabled until typed value
        // matches id exactly. NSAlert doesn't ship a
        // gate-on-text-input affordance, so we wire it via a
        // delegate on accessory field
        installButton.isEnabled = false

        let input = NSTextField(frame: NSRect(x: 0, y: 0, width: 360, height: 24))
        input.placeholderString = spec.id
        let gate = ShellPluginIdGate(expectedId: spec.id, button: installButton)
        input.delegate = gate
        alert.accessoryView = input
        alert.window.initialFirstResponder = input

        NSApp.activate(ignoringOtherApps: true)
        let response = alert.runModal()
        // Even if gate let button flicker enabled,
        // only accept on an exact final match. Trailing whitespace
        // is forgiven; case is not (ids are slug-shaped)
        let typed = input.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        return response == .alertFirstButtonReturn && typed == spec.id
    }

    @MainActor
    private static func showGenericError(message: String) {
        let alert = NSAlert()
        alert.messageText = "Couldn't install plugin"
        alert.informativeText = message
        alert.alertStyle = .warning
        NSApp.activate(ignoringOtherApps: true)
        alert.runModal()
    }

    @MainActor
    private static func showInstallError(message: String) {
        let alert = NSAlert()
        alert.messageText = "Install failed"
        alert.informativeText = message
        alert.alertStyle = .warning
        NSApp.activate(ignoringOtherApps: true)
        alert.runModal()
    }

    @MainActor
    private static func showInstallSuccess(name: String, outcome: String) {
        let verb = outcome == "replaced" ? "updated" : "installed"
        let alert = NSAlert()
        alert.messageText = "\(name) \(verb)"
        alert.informativeText =
            "Restart Gyors (menu bar → Quit, then reopen) to finish loading the plugin."
        alert.alertStyle = .informational
        alert.addButton(withTitle: "Got it")
        NSApp.activate(ignoringOtherApps: true)
        alert.runModal()
    }

    enum CommitResult {
        case success(outcome: String)
        case failure(String)

        static func decode(from json: String) -> CommitResult? {
            guard let data = json.data(using: .utf8),
                  let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
            else { return nil }
            if (obj["ok"] as? Bool) == true {
                let outcome = obj["outcome"] as? String ?? "installed"
                return .success(outcome: outcome)
            }
            let err = (obj["error"] as? String) ?? "unknown error"
            return .failure(err)
        }
    }
}

// MARK: id-confirmation delegate

/// Enables an NSAlert's primary button only when accessory text
/// field's contents (trimmed) exactly match expected plugin id.
/// Used by `PluginInstallPanel.confirmShellPluginById`
private final class ShellPluginIdGate: NSObject, NSTextFieldDelegate {
    let expectedId: String
    weak var button: NSButton?

    init(expectedId: String, button: NSButton) {
        self.expectedId = expectedId
        self.button = button
        super.init()
    }

    func controlTextDidChange(_ notification: Notification) {
        guard let tf = notification.object as? NSTextField else { return }
        let typed = tf.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        button?.isEnabled = (typed == expectedId)
    }
}

// MARK: Preview model

/// Decoded form of `gyors_plugin_install_preview`'s JSON output.
/// Mirrors Rust side's field set; any new keys backend adds are
/// tolerated (unknown keys decoded via JSONSerialization)
struct PluginInstallPreview {
    let spec: PluginSpec
    /// "install" on a fresh id, "update" when an entry with this id
    /// already lives in plugins.json
    let kind: String
    let existing: PluginSpec?
    let validationOk: Bool
    let validationErrors: [String]

    static func decode(from json: String) -> PluginInstallPreview? {
        guard let data = json.data(using: .utf8),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return nil }

        guard let specDict = root["spec"] as? [String: Any],
              let spec = PluginSpec.decode(from: specDict) else { return nil }

        let kind = (root["kind"] as? String) ?? "install"
        let existing = (root["existing"] as? [String: Any])
            .flatMap(PluginSpec.decode(from:))

        var validationOk = true
        var validationErrors: [String] = []
        if let v = root["validation"] as? [String: Any] {
            validationOk = (v["ok"] as? Bool) ?? true
            validationErrors = (v["errors"] as? [String]) ?? []
        }

        return PluginInstallPreview(
            spec: spec,
            kind: kind,
            existing: existing,
            validationOk: validationOk,
            validationErrors: validationErrors
        )
    }
}

struct PluginSpec {
    let id: String
    let name: String
    let description: String
    let keywords: [String]
    let command: String
    let onActivate: String
    let icon: String?
    let version: String?
    let sourceUrl: String?
    let author: String?

    static func decode(from dict: [String: Any]) -> PluginSpec? {
        guard let id = dict["id"] as? String,
              let name = dict["name"] as? String,
              let command = dict["command"] as? String,
              let keywords = dict["keywords"] as? [String]
        else { return nil }

        return PluginSpec(
            id: id,
            name: name,
            description: (dict["description"] as? String) ?? "",
            keywords: keywords,
            command: command,
            onActivate: (dict["on_activate"] as? String) ?? "copy",
            icon: dict["icon"] as? String,
            version: dict["version"] as? String,
            sourceUrl: dict["source_url"] as? String,
            author: dict["author"] as? String
        )
    }
}

// MARK: SwiftUI view

/// Install dialog. Everything user needs to decide "yes install"
/// or "no reject" is on one screen: icon + name + version,
/// description, keywords as tag pills, full command in a readable
/// code block, author + source-url link, validation errors in red,
/// and an Install/Update/Cancel button bar
private struct PluginInstallPanelView: View {
    let preview: PluginInstallPreview
    let specJson: String
    let onInstall: (PluginInstallResult) -> Void
    let onCancel: () -> Void

    @ObservedObject private var themeMgr = ThemeManager.shared
    @State private var commandExpanded = false

    private var theme: Theme { themeMgr.current }
    private var spec: PluginSpec { preview.spec }
    private var isUpdate: Bool { preview.kind == "update" }
    private var canInstall: Bool { preview.validationOk }

    private var iconSymbol: String {
        spec.icon ?? "puzzlepiece.extension"
    }

    private var primaryButtonLabel: String {
        if !canInstall { return "Can't install" }
        if isUpdate {
            if let existing = preview.existing?.version,
               let incoming = spec.version,
               existing == incoming
            {
                return "Reinstall \(spec.name)"
            }
            return "Update \(spec.name)"
        }
        return "Install \(spec.name)"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            header
            Divider().opacity(0.3)
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    if !spec.description.isEmpty {
                        Text(spec.description)
                            .font(.system(size: 14))
                            .foregroundStyle(theme.primaryText)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    keywordsRow
                    activationRow
                    commandBlock
                    metadataRow
                    updateHint
                    if !canInstall {
                        validationBlock
                    }
                }
                .padding(.vertical, 4)
            }
            Spacer(minLength: 0)
            footerButtons
        }
        .padding(24)
        .frame(width: 520, height: 560)
        .background(themeMgr.current.panelTint)
    }

    // MARK: Header

    private var header: some View {
        HStack(alignment: .center, spacing: 16) {
            ZStack {
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .fill(theme.accent.opacity(0.22))
                Image(systemName: iconSymbol)
                    .font(.system(size: 30, weight: .medium))
                    .foregroundStyle(theme.accent)
            }
            .frame(width: 64, height: 64)

            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 8) {
                    Text(spec.name)
                        .font(.system(size: 22, weight: .semibold))
                        .foregroundStyle(theme.primaryText)
                    if let v = spec.version {
                        Text("v\(v)")
                            .font(.system(size: 12, weight: .medium, design: .monospaced))
                            .foregroundStyle(theme.secondaryText)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 2)
                            .background(
                                RoundedRectangle(cornerRadius: 4)
                                    .fill(theme.accent.opacity(0.15))
                            )
                    }
                }
                Text(isUpdate ? "Update plugin" : "Install plugin")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(theme.secondaryText)
                Text(spec.id)
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(theme.tertiaryText)
            }
            Spacer()
        }
    }

    // MARK: Keywords

    private var keywordsRow: some View {
        VStack(alignment: .leading, spacing: 6) {
            sectionHeader("Triggers")
            HStack(spacing: 6) {
                ForEach(spec.keywords, id: \.self) { kw in
                    Text(kw)
                        .font(.system(size: 12, weight: .medium, design: .monospaced))
                        .foregroundStyle(theme.primaryText)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 4)
                        .background(
                            RoundedRectangle(cornerRadius: 6)
                                .fill(theme.accent.opacity(0.22))
                        )
                }
                Spacer(minLength: 0)
            }
        }
    }

    private var activationRow: some View {
        HStack(spacing: 8) {
            sectionHeader("On Enter")
            Text(activationDescription(spec.onActivate))
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(theme.secondaryText)
        }
    }

    private func activationDescription(_ kind: String) -> String {
        switch kind {
        case "copy":  return "Copy command output to clipboard"
        case "open":  return "Open command output as URL"
        case "shell": return "Run command output as shell (⚠ advanced)"
        case "none":  return "No action (command is the side effect)"
        default:      return kind
        }
    }

    // MARK: Command

    private var commandBlock: some View {
        VStack(alignment: .leading, spacing: 6) {
            sectionHeader("Command")
            VStack(alignment: .leading, spacing: 0) {
                Text(spec.command)
                    .font(.system(size: 12, design: .monospaced))
                    .foregroundStyle(theme.primaryText)
                    .textSelection(.enabled)
                    .padding(12)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(
                        RoundedRectangle(cornerRadius: 8)
                            .fill(Color.black.opacity(0.25))
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: 8)
                            .strokeBorder(theme.border, lineWidth: 1)
                    )
            }
            Text("`{query}` gets shell-escaped before substitution, so typing a single quote in Gyors can't break out of the command.")
                .font(.system(size: 10))
                .foregroundStyle(theme.tertiaryText)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    // MARK: Metadata

    private var metadataRow: some View {
        HStack(spacing: 24) {
            if let author = spec.author {
                VStack(alignment: .leading, spacing: 2) {
                    Text("AUTHOR")
                        .font(.system(size: 9, weight: .semibold))
                        .foregroundStyle(theme.tertiaryText)
                    Text(author)
                        .font(.system(size: 12))
                        .foregroundStyle(theme.primaryText)
                }
            }
            if let url = spec.sourceUrl, let u = URL(string: url) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("SOURCE")
                        .font(.system(size: 9, weight: .semibold))
                        .foregroundStyle(theme.tertiaryText)
                    Link(destination: u) {
                        HStack(spacing: 4) {
                            Image(systemName: "arrow.up.right.square")
                                .font(.system(size: 10))
                            Text(shortenUrl(url))
                                .font(.system(size: 12))
                                .lineLimit(1)
                                .truncationMode(.middle)
                        }
                    }
                    .foregroundStyle(theme.accent)
                }
            }
            Spacer(minLength: 0)
        }
    }

    private func shortenUrl(_ url: String) -> String {
        guard let host = URL(string: url)?.host else { return url }
        return host + (URL(string: url)?.path ?? "")
    }

    // MARK: Update hint

    @ViewBuilder
    private var updateHint: some View {
        if isUpdate, let existing = preview.existing {
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: "arrow.up.circle.fill")
                    .foregroundStyle(theme.accent)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Plugin already installed")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(theme.primaryText)
                    Text(updateDescription(
                        existing: existing.version ?? "-",
                        incoming: spec.version ?? "-"
                    ))
                    .font(.system(size: 11))
                    .foregroundStyle(theme.secondaryText)
                    .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 0)
            }
            .padding(10)
            .background(
                RoundedRectangle(cornerRadius: 8)
                    .fill(theme.accent.opacity(0.12))
            )
        }
    }

    private func updateDescription(existing: String, incoming: String) -> String {
        if existing == incoming {
            return "Same version (\(existing)) - reinstall will replace the entry in plugins.json."
        }
        return "Current \(existing) → new \(incoming). Install will replace the entry."
    }

    // MARK: Validation

    private var validationBlock: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)
                Text("This plugin can't be installed")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(theme.primaryText)
            }
            ForEach(preview.validationErrors, id: \.self) { err in
                Text("• \(err)")
                    .font(.system(size: 11))
                    .foregroundStyle(theme.secondaryText)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(10)
        .background(
            RoundedRectangle(cornerRadius: 8)
                .fill(Color.orange.opacity(0.15))
        )
    }

    // MARK: Footer

    private var footerButtons: some View {
        HStack(spacing: 12) {
            Spacer()
            Button("Cancel") { onCancel() }
                .keyboardShortcut(.cancelAction)
            Button(action: { confirmInstall() }) {
                Text(primaryButtonLabel)
                    .fontWeight(.semibold)
                    .padding(.horizontal, 10)
            }
            .buttonStyle(.borderedProminent)
            .tint(canInstall ? theme.accent : .gray)
            .disabled(!canInstall)
            .keyboardShortcut(.defaultAction)
        }
    }

    private func confirmInstall() {
        let result = PluginInstallPanel.callCommit(specJson)
        onInstall(result.asResult())
    }

    private func sectionHeader(_ label: String) -> some View {
        Text(label.uppercased())
            .font(.system(size: 9, weight: .semibold))
            .foregroundStyle(theme.tertiaryText)
    }
}

enum PluginInstallResult {
    case success(outcome: String)
    case failure(String)
}

extension PluginInstallPanel.CommitResult {
    func asResult() -> PluginInstallResult {
        switch self {
        case .success(let outcome): return .success(outcome: outcome)
        case .failure(let err):     return .failure(err)
        }
    }
}
