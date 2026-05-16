import AppKit
import Foundation
import UserNotifications

/// Tracks running countdown timers and surfaces shortest remaining
/// time in menu bar. When a timer hits zero we fire a macOS user
/// notification (falling back to a modal alert if notifications are
/// denied). All public methods hop to main thread
@MainActor
final class TimerManager {
    static let shared = TimerManager()

    struct Timer: Identifiable, Equatable {
        let id: UUID
        let label: String
        let endAt: Date

        func remaining(now: Date = Date()) -> TimeInterval {
            max(0, endAt.timeIntervalSince(now))
        }
    }

    private(set) var timers: [Timer] = []

    /// Installed by `MenuBar` at startup. Called with current
    /// shortest-remaining-time formatted string (or `nil` when no
    /// timers are running) so status-item title can update live
    var onMenuBarUpdate: ((String?) -> Void)?

    /// Repeating 1-second tick updating menu-bar and firing
    /// completion notifications. `nil` when no timers are running -
    /// we dont burn runloop cycles for nothing
    private var ticker: Foundation.Timer?

    private init() {}

    func start(secs: UInt64, label: String) {
        requestNotificationPermissionIfNeeded()
        let t = Timer(
            id: UUID(),
            label: label,
            endAt: Date(timeIntervalSinceNow: TimeInterval(secs))
        )
        timers.append(t)
        startTickerIfNeeded()
        updateMenuBar()
    }

    func cancelAll() {
        timers.removeAll()
        stopTicker()
        updateMenuBar()
    }

    /// Show a small floating panel listing running timers and their
    /// remaining time. Tapping a row cancels it
    func showPanel() {
        if timers.isEmpty {
            NSApp.activate(ignoringOtherApps: true)
            let a = NSAlert()
            a.messageText = "No timers running"
            a.informativeText = "Start one with `timer 25m` or `timer 1h 30m`."
            a.addButton(withTitle: "OK")
            a.runModal()
            return
        }
        NSApp.activate(ignoringOtherApps: true)
        let a = NSAlert()
        a.messageText = "Running timers"
        let lines = timers.map { t -> String in
            let mins = Int(t.remaining()) / 60
            let secs = Int(t.remaining()) % 60
            let time = String(format: "%02d:%02d", mins, secs)
            return t.label.isEmpty ? time : "\(time)  ·  \(t.label)"
        }
        a.informativeText = lines.joined(separator: "\n")
        a.addButton(withTitle: "Cancel All")
        a.addButton(withTitle: "Close")
        if a.runModal() == .alertFirstButtonReturn {
            cancelAll()
        }
    }


    private func startTickerIfNeeded() {
        guard ticker == nil else { return }
        let t = Foundation.Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            Task { @MainActor [weak self] in self?.tick() }
        }
        // Ensure tick fires while user is interacting with a menu
        // (default mode doesn't run during NSMenu tracking)
        RunLoop.main.add(t, forMode: .common)
        ticker = t
    }

    private func stopTicker() {
        ticker?.invalidate()
        ticker = nil
    }

    private func tick() {
        let now = Date()
        let (expired, running) = timers.reduce(into: (expired: [Timer](), running: [Timer]())) { acc, t in
            if t.remaining(now: now) <= 0 {
                acc.expired.append(t)
            } else {
                acc.running.append(t)
            }
        }
        timers = running
        for t in expired { fireCompletion(for: t) }
        if timers.isEmpty { stopTicker() }
        updateMenuBar()
    }

    private func updateMenuBar() {
        guard let callback = onMenuBarUpdate else { return }
        guard let shortest = timers.min(by: { $0.endAt < $1.endAt }) else {
            callback(nil)
            return
        }
        let r = Int(shortest.remaining())
        let mins = r / 60
        let secs = r % 60
        callback(String(format: "%02d:%02d", mins, secs))
    }


    private func fireCompletion(for t: Timer) {
        let center = UNUserNotificationCenter.current()
        let content = UNMutableNotificationContent()
        content.title = t.label.isEmpty ? "Timer done" : "Timer: \(t.label)"
        content.body = "Your timer finished."
        content.sound = .default
        let request = UNNotificationRequest(
            identifier: t.id.uuidString,
            content: content,
            trigger: nil
        )
        center.add(request) { [weak self] err in
            if err != nil {
                // Notifications are denied or broken - surface
                // completion as a modal alert so user still sees it
                Task { @MainActor [weak self] in self?.fallbackAlert(for: t) }
            }
        }
    }

    private func fallbackAlert(for t: Timer) {
        NSApp.activate(ignoringOtherApps: true)
        let a = NSAlert()
        a.messageText = t.label.isEmpty ? "Timer done" : "Timer: \(t.label)"
        a.informativeText = "Your timer finished."
        a.addButton(withTitle: "OK")
        a.runModal()
    }

    private func requestNotificationPermissionIfNeeded() {
        UNUserNotificationCenter.current().requestAuthorization(
            options: [.alert, .sound]
        ) { _, _ in }
    }
}
