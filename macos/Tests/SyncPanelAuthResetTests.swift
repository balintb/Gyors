import Foundation

#if CLOUD

/// Regression suite
///
/// `SyncPanelView.submitAuth` used to clear `password` on a
/// successful response but leave `kdfSalt` (the recovery code) in
/// SwiftUI `@State` for the life of the view. `SyncPanelAuth
/// .resetAfterSuccess` is the pinned contract: both fields are
/// zeroed regardless of mode, and `revealRecovery` fires only on
/// signup with a server-returned salt (the recovery-code panel
/// renders from the server status response, not local state, so
/// dropping the local copy doesn't affect display)
func runSyncPanelAuthResetTests() {

    runGroup("resetAfterSuccess: signup with salt clears AND reveals") {
        let out = SyncPanelAuth.resetAfterSuccess(isSignup: true, gotSalt: true)
        expect(out.kdfSalt == "", "kdfSalt zeroed")
        expect(out.password == "", "password zeroed")
        expect(out.revealRecovery, "signup + salt → reveal recovery panel")
    }

    runGroup("resetAfterSuccess: signup without salt still clears, no reveal") {
        // Server didn't echo a salt back (older protocol, race, etc.).
        // Dont open the recovery panel - theres nothing to show
        let out = SyncPanelAuth.resetAfterSuccess(isSignup: true, gotSalt: false)
        expect(out.kdfSalt == "", "kdfSalt zeroed")
        expect(out.password == "", "password zeroed")
        expect(!out.revealRecovery, "no salt → no reveal even on signup")
    }

    runGroup("resetAfterSuccess: signin with salt clears WITHOUT reveal") {
        // The salt landed in @State because the user pasted it into
        // the signin field. Success means we dont need it anymore.
        // Reveal MUST stay off - signin isn't path that shows
        // the recovery code, that's signup-only
        let out = SyncPanelAuth.resetAfterSuccess(isSignup: false, gotSalt: true)
        expect(out.kdfSalt == "", "kdfSalt zeroed - the case")
        expect(out.password == "", "password zeroed")
        expect(!out.revealRecovery, "signin must never reveal recovery panel")
    }

    runGroup("resetAfterSuccess: signin without salt clears, no reveal") {
        let out = SyncPanelAuth.resetAfterSuccess(isSignup: false, gotSalt: false)
        expect(out.kdfSalt == "", "kdfSalt zeroed")
        expect(out.password == "", "password zeroed")
        expect(!out.revealRecovery, "no signin reveal regardless")
    }

    runGroup("resetAfterSuccess: outcome is deterministic + Equatable") {
        let a = SyncPanelAuth.resetAfterSuccess(isSignup: true, gotSalt: true)
        let b = SyncPanelAuth.resetAfterSuccess(isSignup: true, gotSalt: true)
        expect(a == b, "deterministic outcome")
        let c = SyncPanelAuth.resetAfterSuccess(isSignup: false, gotSalt: true)
        expect(a != c, "signup vs signin produces distinct outcomes")
    }

    runGroup("resetAfterSuccess: clears regardless of input combination") {
        // Belt-and-braces - kdfSalt + password MUST be empty under
        // every (signup/signin x salt/no-salt) combination. Anyone
        // refactoring helper has to break this invariant
        // explicitly
        let combos: [(Bool, Bool)] = [
            (true, true), (true, false),
            (false, true), (false, false),
        ]
        for (signup, salt) in combos {
            let r = SyncPanelAuth.resetAfterSuccess(isSignup: signup, gotSalt: salt)
            expect(
                r.kdfSalt.isEmpty,
                "kdfSalt empty for (signup=\(signup), salt=\(salt))"
            )
            expect(
                r.password.isEmpty,
                "password empty for (signup=\(signup), salt=\(salt))"
            )
        }
    }
}

#else // !CLOUD - no-op so the test runner can link without the cloud module
func runSyncPanelAuthResetTests() {}
#endif
