import Foundation

func runBusyTrackerTests() {
// BusyTracker - refcount + edge transitions
//
// Single-source-of-truth for "is Gyors doing something". The
// menu-bar animator listens on the edge transitions (idle <->
// busy), so off-by-one or unbalanced begin/end calls leak the
// animator state. Hammer the reference counting and the
// callback firing semantics

// Helper: drain any in-flight async callbacks from prior tests
// with callback detached so they fire as no-ops, then return
// the singleton to a clean zero-count state
func resetBusyTracker() {
    BusyTracker.shared.onBusyChange = nil
    BusyTracker.shared.onFlash = nil
    while BusyTracker.shared.debugRefcount > 0 { BusyTracker.shared.end() }
    RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.08))
}

runGroup("BusyTracker fires onBusyChange only on rising edge") {
    resetBusyTracker()

    var changes: [Bool] = []
    BusyTracker.shared.onBusyChange = { changes.append($0) }
    defer { BusyTracker.shared.onBusyChange = nil }

    BusyTracker.shared.begin()
    BusyTracker.shared.begin()
    BusyTracker.shared.begin()
    // OnBusyChange dispatches via DispatchQueue.main.async, so
    // run the runloop briefly to flush
    RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.05))
    expect(changes == [true],
        "rising edge fires once even with multiple begins, got \(changes)")

    BusyTracker.shared.end()
    BusyTracker.shared.end()
    RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.05))
    expect(changes == [true],
        "no falling edge until last end fires, got \(changes)")

    BusyTracker.shared.end()
    RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.05))
    expect(changes == [true, false],
        "falling edge fires on last end, got \(changes)")
}

runGroup("BusyTracker stray end clamps to zero, never negative") {
    resetBusyTracker()
    BusyTracker.shared.end()
    BusyTracker.shared.end()
    BusyTracker.shared.end()
    expect(BusyTracker.shared.debugRefcount == 0,
        "extra ends clamp to 0, got \(BusyTracker.shared.debugRefcount)")
    // Drain any falling-edge dispatches that resetBusyTracker
    // might still have scheduled before attaching our observer
    RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.05))

    var changes: [Bool] = []
    BusyTracker.shared.onBusyChange = { changes.append($0) }
    defer { BusyTracker.shared.onBusyChange = nil }
    BusyTracker.shared.begin()
    RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.05))
    expect(changes == [true], "begin after stray end still fires rising edge, got \(changes)")
    BusyTracker.shared.end()
    RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.05))
}

runGroup("BusyTracker.flash forwards via onFlash") {
    resetBusyTracker()
    var fired = 0
    BusyTracker.shared.onFlash = { fired += 1 }
    defer { BusyTracker.shared.onFlash = nil }
    BusyTracker.shared.flash()
    BusyTracker.shared.flash()
    BusyTracker.shared.flash()
    RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.05))
    expect(fired == 3, "flash should forward each call independently, got \(fired)")
}

runGroup("BusyTracker pairs begin/end across threads") {
    // Whole point of the lock is concurrent usage from AI
    // tasks running on background threads. Spawn N pairs and
    // verify the count returns to zero
    while BusyTracker.shared.debugRefcount > 0 { BusyTracker.shared.end() }
    let group = DispatchGroup()
    let queue = DispatchQueue(label: "busy-test", attributes: .concurrent)
    for _ in 0..<200 {
        group.enter()
        queue.async {
            BusyTracker.shared.begin()
            BusyTracker.shared.end()
            group.leave()
        }
    }
    _ = group.wait(timeout: .now() + 5)
    expect(BusyTracker.shared.debugRefcount == 0,
        "concurrent begin/end pairs leave count at 0, got \(BusyTracker.shared.debugRefcount)")
}
}
