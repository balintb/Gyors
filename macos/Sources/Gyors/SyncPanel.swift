// No-cloud build: drop entire panel. build-app.sh WITH_CLOUD=0
// path leaves file in source tree (simpler than excluding it
// from swiftc invocation) but compiles nothing - `#if CLOUD`
// collapses to whitespace
#if CLOUD
import AppKit
import SwiftUI

/// Cloud-sync sign-in panel
///
/// Shows current session status (signed-in info + outbox depth) or
/// a signup/signin form. All Rust calls go through FFI helpers in
/// `BridgingHeader.h::gyors_sync_*`. Long-running calls (Argon2id +
/// HTTP round-trip) run on a background queue so UI doesn't lock
/// up; responses come back to main thread before mutating state
///
/// Why a window not an NSAlert: alerts can't host SecureField, and
/// recovery-code copy flow benefits from window staying open after
/// success callback
///
/// Window lifecycle management lives in `SyncPanelHost`
/// (AppKit-only, unit-tested) - see doc comment there for why
/// strong-ref + animation-suppression dance exists
enum SyncPanel {
    @MainActor
    static func present() {
        let window = SyncPanelHost.makeWindow()
        let view = SyncPanelView(close: { [weak window] in window?.close() })
        window.contentView = NSHostingView(rootView: view)
        SyncPanelHost.track(window)
        NSApp.activate(ignoringOtherApps: true)
        window.makeKeyAndOrderFront(nil)
    }
    // Helper lives in `SyncPanelAuth.swift` so test
    // harness can link it without pulling in SwiftUI
}

// MARK: - View

private struct SyncPanelView: View {
    let close: () -> Void
    @State private var status: SyncStatus = .loading
    @State private var mode: AuthMode = .signin
    @State private var email: String = ""
    @State private var password: String = ""
    @State private var kdfSalt: String = ""
    @State private var working: Bool = false
    @State private var errorMessage: String?
    @State private var recoveryCodeShown: Bool = false

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            header
            Divider()
            switch status {
            case .loading:
                ProgressView("loading sync status…")
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding(.vertical, 24)
            case .signedOut:
                signedOutBody
            case .signedIn(let info):
                signedInBody(info)
            case .error(let message):
                Text(message)
                    .foregroundColor(.red)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            Spacer(minLength: 0)
            footer
        }
        .padding(20)
        .frame(width: 460)
        .onAppear { refreshStatus() }
    }

    private var header: some View {
        HStack(alignment: .firstTextBaseline) {
            Text("Cloud Sync")
                .font(.title2).bold()
            Spacer()
            if case .signedIn = status {
                Button("Sync Now") { runTick() }
                    .disabled(working)
            }
        }
    }

    private var footer: some View {
        HStack {
            if let err = errorMessage {
                Text(err).foregroundColor(.red)
                    .font(.callout)
            } else if working {
                ProgressView().controlSize(.small)
                Text("working…").font(.callout).foregroundColor(.secondary)
            }
            Spacer()
            Button("Close", action: close).keyboardShortcut(.cancelAction)
        }
    }

    // MARK: signed-out form

    private var signedOutBody: some View {
        VStack(alignment: .leading, spacing: 12) {
            Picker("", selection: $mode) {
                Text("Sign In").tag(AuthMode.signin)
                Text("Create Account").tag(AuthMode.signup)
            }
            .pickerStyle(.segmented)
            .labelsHidden()

            TextField("you@example.com", text: $email)
                .textFieldStyle(.roundedBorder)
                .disableAutocorrection(true)

            SecureField("Password", text: $password)
                .textFieldStyle(.roundedBorder)

            if mode == .signin {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Recovery code (kdf_salt)")
                        .font(.callout)
                        .foregroundColor(.secondary)
                    SecureField("base64 from your other device", text: $kdfSalt)
                        .textFieldStyle(.roundedBorder)
                    Text("Required when signing in on a new device. Copy it from another signed-in Mac under \"Recovery code\".")
                        .font(.footnote)
                        .foregroundColor(.secondary)
                }
            } else {
                Text("Plus tier ($X/mo). The encryption key is derived from your password and never leaves this Mac.")
                    .font(.footnote)
                    .foregroundColor(.secondary)
            }

            HStack {
                Button(mode == .signin ? "Sign In" : "Create Account") {
                    submitAuth()
                }
                .disabled(working || email.isEmpty || password.isEmpty
                          || (mode == .signin && kdfSalt.isEmpty))
                .keyboardShortcut(.defaultAction)
                Spacer()
            }
            .padding(.top, 4)
        }
    }

    // MARK: signed-in summary

    private func signedInBody(_ info: SyncStatusResponse) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            row(label: "User", value: info.email ?? "?")
            row(label: "Tier", value: info.tier?.uppercased() ?? "?")
            row(label: "Backend", value: info.backend)
            row(label: "Outbox", value: "\(info.outbox_pending) pending")
            if let cursor = info.pull_cursor {
                row(label: "Pull tip", value: cursor)
            }
            if let exp = info.expires_at {
                row(label: "Expires", value: exp)
            }

            if let salt = info.kdf_salt {
                Divider().padding(.vertical, 4)
                Text("Recovery code")
                    .font(.callout).bold()
                Text("Required to sign in on another Mac. Save it somewhere safe - losing it locks you out.")
                    .font(.footnote)
                    .foregroundColor(.secondary)
                HStack {
                    if recoveryCodeShown {
                        Text(salt)
                            .font(.system(.callout, design: .monospaced))
                            .textSelection(.enabled)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    } else {
                        Text(String(repeating: "•", count: max(8, min(24, salt.count))))
                            .font(.system(.callout, design: .monospaced))
                            .foregroundColor(.secondary)
                    }
                    Spacer()
                    Button(recoveryCodeShown ? "Hide" : "Reveal") {
                        recoveryCodeShown.toggle()
                    }
                    Button("Copy") {
                        let pb = NSPasteboard.general
                        pb.clearContents()
                        pb.setString(salt, forType: .string)
                    }
                    Button("Save…") {
                        saveRecoveryCode(
                            salt: salt,
                            email: info.email ?? ""
                        )
                    }
                }
            }

            HStack {
                Button("Sign Out", role: .destructive) { runSignout() }
                    .disabled(working)
                Spacer()
            }
            .padding(.top, 6)
        }
    }

    private func row(label: String, value: String) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(label).foregroundColor(.secondary).frame(width: 80, alignment: .leading)
            Text(value)
                .font(.system(.body, design: .monospaced))
                .lineLimit(1)
                .truncationMode(.middle)
        }
    }

    // MARK: actions

    private func refreshStatus() {
        working = false
        errorMessage = nil
        DispatchQueue.global(qos: .userInitiated).async {
            let json = SyncFFI.statusJson()
            DispatchQueue.main.async {
                if let resp: SyncStatusResponse = decode(json), resp.signed_in {
                    self.status = .signedIn(resp)
                    self.email = resp.email ?? ""
                } else if let resp: SyncStatusResponse = decode(json) {
                    self.status = .signedOut
                    // Preserve email across signed-out reload so user
                    // doesn't retype after a failed signup
                    self.email = resp.email ?? self.email
                } else {
                    self.status = .error("Couldn't read sync status: \(json)")
                }
            }
        }
    }

    private func submitAuth() {
        working = true
        errorMessage = nil
        let m = mode
        let e = email
        let p = password
        let s = kdfSalt
        DispatchQueue.global(qos: .userInitiated).async {
            let json: String
            switch m {
            case .signup: json = SyncFFI.signupJson(email: e, password: p)
            case .signin: json = SyncFFI.signinJson(email: e, password: p, kdfSalt: s)
            }
            DispatchQueue.main.async {
                self.working = false
                if let resp: SyncOkResponse = decode(json), resp.ok {
                    // Zero BOTH password and kdfSalt on
                    // success. Without kdfSalt clear, recovery code
                    // stays on heap for life of this SwiftUI view -
                    // sign-out clears it but success branch used to
                    // leave it. Recovery-code panel reads from
                    // server's status response (`info.kdf_salt`),
                    // not local @State, so dropping local copy here
                    // doesn't affect display
                    let next = SyncPanelAuth.resetAfterSuccess(
                        isSignup: m == .signup,
                        gotSalt: resp.kdf_salt != nil
                    )
                    self.password = next.password
                    self.kdfSalt = next.kdfSalt
                    if next.revealRecovery {
                        self.recoveryCodeShown = true
                    }
                    self.refreshStatus()
                } else {
                    self.password = ""
                    let resp: SyncOkResponse? = decode(json)
                    self.errorMessage = resp?.error ?? "Auth failed: \(json)"
                }
            }
        }
    }

    private func runTick() {
        // No password prompt - encryption key is cached in
        // Keychain-resident session blob, derived once at sign-in
        //
        // Tick returns three distinct shapes we have to map to
        // user-visible text:
        //   - ok + no flags:        "synced - pushed X, pulled Y"
        //   - ok + session_expired: prompt re-signin; bg loop already paused
        //   - ok + quota_exceeded:  "storage full" banner; bg loop paused
        //   - !ok (error):          surface server's error text
        working = true
        errorMessage = nil
        DispatchQueue.global(qos: .userInitiated).async {
            let json = SyncFFI.tickJson()
            DispatchQueue.main.async {
                self.working = false
                let resp: SyncOkResponse? = decode(json)
                if let r = resp, r.ok {
                    if r.session_expired == true {
                        // 401 from server. bg loop has latched
                        // paused in Rust; we wipe session-derived
                        // UI state and route user back to sign-in
                        // form. They'll see explanation in
                        // errorMessage
                        self.errorMessage = "Session expired - please sign in again."
                        self.mode = .signin
                        self.recoveryCodeShown = false
                    } else if r.quota_exceeded == true {
                        self.errorMessage = "Sync paused - your storage is full. Free space or upgrade to continue."
                    } else {
                        self.errorMessage =
                            "synced - pushed \(r.pushed ?? 0), pulled \(r.pulled ?? 0)"
                    }
                } else {
                    self.errorMessage = resp?.error ?? "Sync failed: \(json)"
                }
                self.refreshStatus()
            }
        }
    }

    private func runSignout() {
        working = true
        DispatchQueue.global(qos: .userInitiated).async {
            _ = SyncFFI.signoutJson()
            DispatchQueue.main.async {
                self.working = false
                self.kdfSalt = ""
                self.recoveryCodeShown = false
                self.refreshStatus()
            }
        }
    }

    /// Save recovery code to a user-chosen `.txt` file
    ///
    /// Losing this salt = permanent lockout (no other device can
    /// derive right auth verifier without it), so "Copy" alone
    /// isn't enough - user almost certainly nukes their clipboard
    /// before they remember to paste it somewhere safe. A real
    /// file with their email + a warning header is most-recoverable
    /// shape we can ship before email integration + forgot-password
    /// lands
    ///
    /// Permissions: clamped to 0600 after write so a wider-readable
    /// home directory doesn't accidentally expose file. Recovery
    /// code itself can't decrypt anything (it's salt, not key), but
    /// it does let an attacker who ALSO has password sign in on a
    /// new device, which is worth one extra `chmod` call
    private func saveRecoveryCode(salt: String, email: String) {
        let panel = NSSavePanel()
        panel.title = "Save Gyors Recovery Code"
        panel.message = "Save this somewhere safe. You'll need it to sign in on a new Mac."
        panel.nameFieldStringValue = "gyors-recovery-code.txt"
        panel.allowedContentTypes = [.plainText]
        panel.canCreateDirectories = true
        NSApp.activate(ignoringOtherApps: true)
        guard panel.runModal() == .OK, let url = panel.url else { return }

        let body = Self.recoveryCodeFileBody(salt: salt, email: email)
        do {
            try body.write(to: url, atomically: true, encoding: .utf8)
            try? FileManager.default.setAttributes(
                [.posixPermissions: 0o600],
                ofItemAtPath: url.path
            )
        } catch {
            let alert = NSAlert()
            alert.messageText = "Couldn't save recovery code"
            alert.informativeText = error.localizedDescription
            alert.alertStyle = .warning
            alert.runModal()
        }
    }

    /// Build contents of recovery-code file. Pulled out as a static
    /// helper so it's tested without spinning up an NSSavePanel
    static func recoveryCodeFileBody(salt: String, email: String) -> String {
        """
        Gyors Recovery Code
        ===================

        Account: \(email)

        Recovery code (kdf_salt):
        \(salt)

        WHAT THIS IS
        ------------
        Your recovery code is the salt that lets another Mac derive
        the same encryption keys as this one. To sign in on a new
        device, paste this code + your password into the Sync panel.

        WHY YOU NEED IT
        ---------------
        Without this code, signing in on a new device is impossible
        - even with the correct email and password. Print it,
        screenshot it, store it in a password manager, anything that
        survives losing this Mac.

        WHAT YOU CAN SHARE
        ------------------
        This code by itself can't decrypt your data. An attacker
        also needs your password. Treat it like a 2-of-2 secret;
        share neither half.
        """
    }
}

private enum AuthMode: Hashable { case signup, signin }

private enum SyncStatus {
    case loading
    case signedOut
    case signedIn(SyncStatusResponse)
    case error(String)
}

// MARK: - FFI bridge

private enum SyncFFI {
    static func statusJson() -> String {
        guard let raw = gyors_sync_status() else { return "{}" }
        defer { gyors_free_string(raw) }
        return String(cString: raw)
    }

    static func signupJson(email: String, password: String) -> String {
        var out = "{}"
        email.withCString { e in
            password.withCString { p in
                if let raw = gyors_sync_signup(e, p) {
                    out = String(cString: raw)
                    gyors_free_string(raw)
                }
            }
        }
        return out
    }

    static func signinJson(email: String, password: String, kdfSalt: String) -> String {
        var out = "{}"
        email.withCString { e in
            password.withCString { p in
                kdfSalt.withCString { k in
                    if let raw = gyors_sync_signin(e, p, k) {
                        out = String(cString: raw)
                        gyors_free_string(raw)
                    }
                }
            }
        }
        return out
    }

    static func signoutJson() -> String {
        guard let raw = gyors_sync_signout() else { return "{}" }
        defer { gyors_free_string(raw) }
        return String(cString: raw)
    }

    static func tickJson() -> String {
        guard let raw = gyors_sync_tick() else { return "{}" }
        defer { gyors_free_string(raw) }
        return String(cString: raw)
    }
}

// MARK: - Decoded responses

private struct SyncStatusResponse: Decodable {
    let signed_in: Bool
    let email: String?
    let tier: String?
    let user_id: String?
    let expires_at: String?
    let kdf_salt: String?
    let backend: String
    let base_url: String
    let outbox_pending: Int
    let pull_cursor: String?
}

private struct SyncOkResponse: Decodable {
    let ok: Bool
    let error: String?
    let kdf_salt: String?
    let pushed: Int?
    let pulled: Int?
    /// Server returned 401 - bearer dead. Swift surfaces a re-signin
    /// prompt and bg loop pauses until user signs back in
    let session_expired: Bool?
    /// Server returned 507 - storage full. Sync stays paused until
    /// they free bytes (or upgrade)
    let quota_exceeded: Bool?
}

private func decode<T: Decodable>(_ json: String) -> T? {
    guard let data = json.data(using: .utf8) else { return nil }
    return try? JSONDecoder().decode(T.self, from: data)
}

#endif // CLOUD
