import Foundation

/// Thread-safe refcount for "Gyors is doing something" - menubar
/// observes this to swap to an animated icon while async work is
/// in flight (AI router, free-form ask, transforms)
///
/// Refcount semantics: `begin` and `end` must be paired. Multiple
/// concurrent calls compose - `begin; begin; end;` is still busy.
/// `onBusyChange` callback only fires on rising/falling edges, not
/// on every increment. `onFlash` is fire-and-forget
final class BusyTracker {
    static let shared = BusyTracker()

    /// Called on main queue when busy state transitions (idle <->
    /// busy). Wired up once at app launch by `MenuBar`
    var onBusyChange: ((Bool) -> Void)?

    /// Called on main queue for one-shot momentary flashes
    /// (clipboard copies, etc.)
    var onFlash: (() -> Void)?

    private let lock = NSLock()
    private var counter: Int = 0

    private init() {}

    /// Mark start of a busy operation. Pair with exactly one `end`
    func begin() {
        lock.lock()
        let wasBusy = counter > 0
        counter += 1
        let nowBusy = counter > 0
        lock.unlock()
        if !wasBusy && nowBusy {
            DispatchQueue.main.async { [weak self] in
                self?.onBusyChange?(true)
            }
        }
    }

    /// Mark end of a busy operation. Safe to call from any thread;
    /// guarded against drifting below zero so a stray extra `end`
    /// is a no-op rather than UB
    func end() {
        lock.lock()
        let wasBusy = counter > 0
        counter = max(0, counter - 1)
        let nowBusy = counter > 0
        lock.unlock()
        if wasBusy && !nowBusy {
            DispatchQueue.main.async { [weak self] in
                self?.onBusyChange?(false)
            }
        }
    }

    /// Trigger a one-shot strobe (e.g., on clipboard copy). Animator
    /// suppresses flashes during a sustained busy state so two
    /// effects dont collide visually
    func flash() {
        DispatchQueue.main.async { [weak self] in
            self?.onFlash?()
        }
    }

    /// Test/debug accessor - current refcount snapshot
    var debugRefcount: Int {
        lock.lock()
        defer { lock.unlock() }
        return counter
    }
}
