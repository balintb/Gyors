import Foundation

#if CLOUD

/// Contract: after a successful auth response, local
/// `kdfSalt` + `password` @State on sync panel MUST be zeroed.
/// Recovery-code panel reads `kdf_salt` from server status
/// response, not local @State, so dropping local salt doesn't
/// affect display
///
/// Lives as a standalone enum (rather than an extension on
/// `SyncPanel`) so test harness can link it without compiling
/// SwiftUI view code
enum SyncPanelAuth {
    struct ResetState: Equatable {
        let kdfSalt: String
        let password: String
        let revealRecovery: Bool
    }

    /// Returns @State shape after a successful auth response.
    /// `kdfSalt` and `password` are always empty; `revealRecovery`
    /// fires only on signup with a server-returned salt (signin
    /// must never open recovery-code panel because theres no
    /// fresh code to reveal)
    static func resetAfterSuccess(isSignup: Bool, gotSalt: Bool) -> ResetState {
        ResetState(
            kdfSalt: "",
            password: "",
            revealRecovery: isSignup && gotSalt
        )
    }
}

#endif
