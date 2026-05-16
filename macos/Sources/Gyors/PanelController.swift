import AppKit
import Combine
import SwiftUI

@MainActor
final class PanelController {
    private let bridge: GyorsBridge
    private let viewModel: GyorsViewModel
    private var panel: GyorsPanel?
    private var hosting: ResizingHostingController<ContentView>?
    /// Blur substrate. Held so we can re-round its corners when theme
    /// changes (each theme can declare its own `cornerRadius`)
    private var blurView: NSVisualEffectView?
    private var cancellables = Set<AnyCancellable>()
    /// Tokens for `NotificationCenter` block-based observers
    /// installed by `ensurePanel`. Without these we have no handle
    /// to call `removeObserver(_:)` with - observer sticks around
    /// for panel's lifetime via NotificationCenter retaining
    /// closure. App-lifetime today, but `deinit` removes them
    /// defensively so an early teardown (or future panel-recreation
    /// path) doesn't leak
    private var notificationTokens: [NSObjectProtocol] = []

    init(bridge: GyorsBridge) {
        self.bridge = bridge
        self.viewModel = GyorsViewModel(bridge: bridge)
        // Any view-mode transition can change content height
        // dramatically (editor / preview popping open or shut). Mark
        // next resize as a "recenter" rather than a pin-top one;
        // hosting controller consumes flag when SwiftUI actually
        // publishes new size, which is only moment at which we know
        // final dimensions
        viewModel.$viewMode
            .dropFirst()
            .removeDuplicates()
            .sink { [weak self] _ in
                self?.hosting?.recenterOnNextResize = true
            }
            .store(in: &cancellables)

        // Theme switches mid-session should re-round substrate;
        // otherwise a theme with a different corner radius leaves
        // blur layer with previous theme's curve
        ThemeManager.shared.$current
            .sink { [weak self] theme in
                self?.applyCornerRadius(theme.cornerRadius)
            }
            .store(in: &cancellables)
    }

    private func applyCornerRadius(_ radius: CGFloat) {
        // Round every layer that takes part in panel's silhouette.
        // Container is visible rounded edge against desktop; host's
        // layer + blur layer (if present) need to share same radius
        // so nothing pokes past
        if let container = panel?.contentView {
            container.layer?.cornerRadius = radius
        }
        hosting?.view.layer?.cornerRadius = radius
        blurView?.layer?.cornerRadius = radius
    }

    /// Pull in-effect blur preference at moment we're building
    /// panel: user override (`panel_blur` config key) wins,
    /// otherwise fall back to active theme's `usesBlur`. Read once
    /// at construction because we dont recreate panel on theme
    /// change today; toggling blur live would need rebuilding
    /// contentView
    private func effectivePanelBlurAtConstruction() -> Bool {
        let cfg = Config.load()
        if let v = cfg.effectivePanelBlur { return v }
        return ThemeManager.shared.current.usesBlur
    }

    func toggle() {
        if let panel = panel, panel.isVisible {
            hide()
        } else {
            show()
        }
    }

    /// Re-fire current query if panel is visible and user is
    /// browsing clipboard history. Wired up to
    /// `PasteboardWatcher.onChange` so a fresh copy lands in list
    /// without making user close + reopen panel. No-op for any
    /// other view-mode / query so non-clipboard work (an open AI
    /// thinking screen, an active note editor, ...) isn't disturbed
    /// by background pasteboard activity
    func refreshIfClipboardVisible() {
        guard let p = panel, p.isVisible else { return }
        guard viewModel.viewMode == .main else { return }
        let prefix = viewModel.query.lowercased()
        let isClipboardQuery = prefix == "clip"
            || prefix == "paste"
            || prefix == "cb"
            || prefix == "c"
            || prefix.hasPrefix("clip ")
            || prefix.hasPrefix("paste ")
            || prefix.hasPrefix("cb ")
            || prefix.hasPrefix("c ")
        guard isClipboardQuery else { return }
        viewModel.update(query: viewModel.query)
    }

    /// Build NSPanel + NSHostingView offscreen so first hotkey
    /// press doesn't pay SwiftUI runtime / Auto-Layout warmup cost
    /// in user-visible path. Called from
    /// `applicationDidFinishLaunching` once bridge is ready and
    /// menu bar is up. Idempotent - `ensurePanel()`
    /// short-circuits if panel already exists
    func prewarm() {
        let t = DispatchTime.now()
        _ = ensurePanel()
        let elapsedMs = Double(DispatchTime.now().uptimeNanoseconds - t.uptimeNanoseconds) / 1_000_000
        NSLog("gyors: panel prewarm %.0fms", elapsedMs)
    }

    /// Open panel and pre-fill input with `text`. Does NOT activate
    /// first result - exposes filled query for user to adjust or
    /// confirm. Used by `gyors://open?query=...` URL hook
    func show(withQuery text: String) {
        show()
        // Show() already called reset() + focusTick bump; layer
        // query update on top so text field reflects it
        DispatchQueue.main.async { [weak self] in
            self?.viewModel.setInput(text)
        }
    }

    func show() {
        _ = ensurePanel()
        viewModel.reset()
        viewModel.focusTick &+= 1
        // Settle-and-present: sizeThatFits often returns 0 on very
        // first invocation because SwiftUI hasn't finished its first
        // layout pass yet. Presenting at that stale 0 gives classic
        // "menu flash" where panel paints at 1pt for one frame then
        // snaps to full size. We retry across a few runloop hops
        // until SwiftUI reports a real height, then present once
        // size is correct
        settleAndPresent(attempt: 0)
    }

    private func settleAndPresent(attempt: Int) {
        DispatchQueue.main.async { [weak self] in
            guard let self = self, let p = self.panel else { return }
            var measuredOK = false
            if let hosting = self.hosting {
                hosting.view.layoutSubtreeIfNeeded()
                let measured = hosting.sizeThatFits(
                    in: NSSize(width: 720, height: CGFloat(10_000))
                )
                if measured.height > 0 {
                    self.resize(p, to: measured)
                    measuredOK = true
                }
            }
            // Cap attempts at 4 (~4 frames @ 60 Hz); if SwiftUI
            // still hasn't settled, present anyway and accept a
            // minor visible growth. In practice 1 extra hop is
            // enough on fresh launches
            if !measuredOK && attempt < 4 {
                self.settleAndPresent(attempt: attempt + 1)
                return
            }
            self.center(p)
            NSApp.activate(ignoringOtherApps: true)
            p.makeKeyAndOrderFront(nil)
            self.hosting?.recenterOnNextResize = false
        }
    }

    func hide() {
        panel?.orderOut(nil)
    }

    private func ensurePanel() -> GyorsPanel {
        if let existing = panel { return existing }

        let root = ContentView(vm: viewModel, onDismiss: { [weak self] in self?.hide() })
        let host = ResizingHostingController(rootView: root)
        host.sizingOptions = [.preferredContentSize]
        host.view.wantsLayer = true
        host.view.layer?.backgroundColor = .clear
        // NSHostingView on macOS 26 ships with `layer.isOpaque =
        // true` by default - so even with a clear background the
        // layer still claims to be opaque, and CoreAnimation skips
        // alpha compositing for it. Explicitly flipping this to
        // false is what actually lets panel substrate's blur show
        // through
        host.view.layer?.isOpaque = false
        // Clip host's layer to same rounded shape as substrate.
        // Even if some hidden NSHostingView paint
        // snuck past our background config, masksToBounds at layer
        // level guarantees nothing renders past rounded edge - no
        // more square corners poking out behind curve
        host.view.layer?.cornerRadius = ThemeManager.shared.current.cornerRadius
        if #available(macOS 11.0, *) {
            host.view.layer?.cornerCurve = .continuous
        }
        host.view.layer?.masksToBounds = true
        self.hosting = host

        // Start at a sensible input-height default (72 pt) so first
        // present never shows a 1-pt sliver while SwiftUI is still
        // measuring. Settle loop in `show()` refines this to real
        // preferred size before ordering front, but even a
        // worst-case "sizeThatFits returned 0 for every retry"
        // fallback lands on something that looks like a search bar
        // rather than a 1-pt flash-then-grow.
        // `.borderless` is canonical Spotlight / Raycast / Alfred
        // style mask - drops title bar entirely and lets window's
        // contentView fill from edge to edge. Previous
        // `[.nonactivatingPanel, .fullSizeContentView]` combo
        // silently forced an opaque content backing on macOS 26
        // because `.fullSizeContentView` is only meaningful in
        // combination with `.titled`, and without that system fell
        // back to a default backing that ignored `isOpaque =
        // false`. Result: no translucency, no matter what we drew
        // on top
        let p = GyorsPanel(
            contentRect: NSRect(x: 0, y: 0, width: 720, height: 72),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        p.isFloatingPanel = true
        p.level = .floating
        p.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]
        p.isMovableByWindowBackground = true
        p.hidesOnDeactivate = true
        p.backgroundColor = .clear
        p.isOpaque = false
        p.hasShadow = true

        // Layered transparency: a transparent rounded container
        // view is panel's contentView, with optional blur and
        // SwiftUI host both pinned inside it as siblings. Why
        // this shape:
        //
        //   - Putting `NSVisualEffectView` AS contentView (earlier
        //     shape) made whole panel render opaque on this Mac -
        //     possibly a `.hudWindow` material quirk on macOS 26,
        //     possibly a SwiftUI/NSHostingView interaction. Either
        //     way, basic alpha stops working when set up that way,
        //     and blur disappears too.
        //   - Putting blur INSIDE host (via NSViewRepresentable in
        //     SwiftUI) collapses it to zero size in a ZStack
        //
        // A transparent NSView container with blur as a *sibling*
        // (not a parent) of host view side-steps both. If blur
        // silently fails on a given Mac, SwiftUI tint over
        // transparent container still produces visible alpha
        let container = NSView()
        container.wantsLayer = true
        container.layer?.backgroundColor = .clear
        container.layer?.isOpaque = false
        // NB: no `cornerRadius` / `masksToBounds` on container -
        // when combined, those flags force CoreAnimation into
        // offscreen rasterisation, and on macOS 26 resulting
        // buffer composites OPAQUE against window even though
        // layer's background is clear. (That's regression that
        // re-introduced "no translucency" symptom after bare-bones
        // approach was working.) Rounded edge lives on *children*
        // instead - host.view and blur both get their own corner
        // clip below

        let blurEnabled = effectivePanelBlurAtConstruction()
        if blurEnabled {
            let blur = NSVisualEffectView()
            // `.popover` reads as "translucent floating UI" on
            // macOS 26 more reliably than `.hudWindow` (which on
            // this OS sometimes renders solid). Behind-window
            // blending blurs whatever desktop or app is sitting
            // underneath panel
            blur.material = .popover
            blur.blendingMode = .behindWindow
            blur.state = .active
            blur.wantsLayer = true
            // Rounded clip lives on blur layer itself - keeping
            // container's layer flat is what unblocks window's
            // transparency on macOS 26
            blur.layer?.cornerRadius = ThemeManager.shared.current.cornerRadius
            if #available(macOS 11.0, *) {
                blur.layer?.cornerCurve = .continuous
            }
            blur.layer?.masksToBounds = true
            blur.translatesAutoresizingMaskIntoConstraints = false
            container.addSubview(blur)
            NSLayoutConstraint.activate([
                blur.leadingAnchor.constraint(equalTo: container.leadingAnchor),
                blur.trailingAnchor.constraint(equalTo: container.trailingAnchor),
                blur.topAnchor.constraint(equalTo: container.topAnchor),
                blur.bottomAnchor.constraint(equalTo: container.bottomAnchor),
            ])
            self.blurView = blur
        } else {
            self.blurView = nil
        }

        host.view.translatesAutoresizingMaskIntoConstraints = false
        container.addSubview(host.view)
        NSLayoutConstraint.activate([
            host.view.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            host.view.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            host.view.topAnchor.constraint(equalTo: container.topAnchor),
            host.view.bottomAnchor.constraint(equalTo: container.bottomAnchor),
        ])

        p.contentView = container
        host.owningPanel = p

        // Warm SwiftUI's layout cache so first `show()` has a valid
        // measurement ready. With Auto Layout driving host view
        // from container, just setting `.frame` no longer takes -
        // constraints revert it on next pass and SwiftUI never
        // gets a chance to lay out at warm size. Resize *panel
        // itself* to warm dimensions, force layout, query, then
        // let `settleAndPresent` shrink to real measured height
        // before we order-front. User only ever sees panel after
        // that final resize, so this offscreen warming is
        // invisible - but without it first present shows input
        // flashing through 1-pt -> fitted height
        p.setContentSize(NSSize(width: 720, height: 600))
        container.layoutSubtreeIfNeeded()
        _ = host.sizeThatFits(in: NSSize(width: 720, height: 10_000))

        // Auto-hide on focus loss. `.nonactivatingPanel` opts out
        // of normal app-activation lifecycle, so `hidesOnDeactivate`
        // alone doesn't fire when an external app (a launched app,
        // a notification panel, a file-save dialog) steals focus -
        // panel was left "logically open but invisible", which
        // meant next hotkey press just hid already-hidden panel
        // and user had to press it twice to reopen.
        // `didResignKeyNotification` fires reliably whenever our
        // panel loses key status, even from non-activating
        // windows, so we listen for that and call `hide()`
        // synchronously
        let token = NotificationCenter.default.addObserver(
            forName: NSWindow.didResignKeyNotification,
            object: p,
            queue: .main
        ) { [weak self] _ in
            // Observer block dispatched on `.main` queue, so the
            // call to `hide()` is on the main actor in practice -
            // `assumeIsolated` tells the compiler what runtime
            // already guarantees
            MainActor.assumeIsolated {
                self?.hide()
            }
        }
        // Capture token so `deinit` can balance addObserver with a
        // removeObserver. Without this, closure (and
        // NotificationCenter's internal record of it) outlives
        // controller. Block-based observers dont auto-deregister
        notificationTokens.append(token)

        panel = p
        return p
    }

    deinit {
        // Balance block-based `addObserver` calls. `[weak self]`
        // capture on closures means a fired observer post-deinit
        // would be a no-op anyway, but NotificationCenter would
        // still hold closure forever without this cleanup - a
        // textbook small but real leak
        for token in notificationTokens {
            NotificationCenter.default.removeObserver(token)
        }
    }

    private func resize(_ p: GyorsPanel, to size: NSSize) {
        let newSize = NSSize(width: 720, height: max(size.height, 64))
        guard p.frame.size != newSize else { return }
        let oldTop = p.frame.origin.y + p.frame.size.height
        var frame = p.frame
        frame.size = newSize
        frame.origin.y = oldTop - newSize.height
        p.setFrame(frame, display: true, animate: false)
        // Re-evaluate panel shadow against new bounds so
        // window-server doesn't keep a stale rounded-corner mask.
        // We used to also call `layoutSubtreeIfNeeded()` here to
        // force rounded sublayers to settle in same hop, but that
        // turned out to interleave badly with SwiftUI's own
        // dispatch when panel SHRANK after a result-set change
        // (typing then backspacing): window-server held a
        // half-flushed snapshot from larger size, painting a
        // phantom darker rectangle over new content for ~one
        // second. `setFrame(_:display:animate:)` already drives a
        // full display pass; trusting AppKit's redraw and only
        // nudging shadow recompute keeps resize visually atomic
        p.invalidateShadow()
    }

    private func center(_ p: GyorsPanel) {
        guard let screen = NSScreen.main else { return }
        let vis = screen.visibleFrame
        let frame = p.frame
        let x = vis.midX - frame.width / 2
        let tall = frame.height > vis.height * 0.5
        let y: CGFloat
        if tall {
            // Tall (editor / preview) -> vertically centred so
            // content doesn't run off bottom edge
            y = vis.midY - frame.height / 2
        } else {
            // Short (search-style) -> anchor panel TOP at a fixed
            // Spotlight-style upper-third Y, derive origin.y from
            // there. Panel height grows downward (input box stays
            // put, results extend below)
            //
            // Earlier shape: `y = midY + height/2 + 40`. That made
            // origin.y depend on height and SHIFTED PANEL TOP UP as
            // panel grew - fine when an empty panel was just 72pt
            // of input, broken once empty-state discovery rows
            // pushed initial height to ~440pt and input ended up
            // half-way up screen. Anchor-and-derive form keeps
            // input at same screen Y on every open regardless of
            // how many rows are showing
            y = Self.panelTopAnchor(visibleFrame: vis) - frame.height
        }
        p.setFrameOrigin(NSPoint(x: x, y: y))
    }

    /// Where panel's top edge sits on screen for short (search-
    /// style) panels. Spotlight-style upper-third position,
    /// computed once so `center()` and
    /// `ResizingHostingController` can't drift apart on formula
    fileprivate static func panelTopAnchor(visibleFrame vis: NSRect) -> CGFloat {
        // Roughly upper-third of visible area. Previous empty-panel
        // layout landed at `midY + 148`; keeping that value
        // preserves Spotlight-feel users had calibrated for, while
        // making position invariant in panel height
        vis.midY + 148
    }
}

final class GyorsPanel: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }
}

/// Hosting controller that resizes its owning panel whenever
/// SwiftUI's preferred content size changes. Two positioning
/// regimes:
///
/// - Pin top (default): panel's top edge stays put while
///   content grows/shrinks. Right for search-mode typing - input
///   field doesn't jump as rows appear below it.
/// - Re-center: computed from new size. Short panels go to
///   Spotlight upper-third; tall panels centre vertically on
///   screen. PanelController flips `recenterOnNextResize` when
///   view mode changes (editor/preview toggles) so the one resize
///   following transition uses this path; subsequent typing goes
///   back to pin-top
final class ResizingHostingController<Content: View>: NSHostingController<Content> {
    weak var owningPanel: NSPanel?
    var recenterOnNextResize: Bool = false

    override var preferredContentSize: NSSize {
        get { super.preferredContentSize }
        set {
            super.preferredContentSize = newValue
            guard let panel = owningPanel else { return }
            let width = panel.frame.width
            let newHeight = max(newValue.height, 64)

            var frame = panel.frame
            frame.size = NSSize(width: width, height: newHeight)

            if recenterOnNextResize, let screen = NSScreen.main {
                let vis = screen.visibleFrame
                frame.origin.x = vis.midX - width / 2
                if newHeight > vis.height * 0.5 {
                    // Tall (editor / preview) -> vertically
                    // centered so content doesn't run off bottom
                    frame.origin.y = vis.midY - newHeight / 2
                } else {
                    // Short (main search) -> anchor panel top at
                    // Spotlight upper-third Y, derive origin.y.
                    // Same formula as `PanelController.center()`;
                    // see there for why this replaced old
                    // `midY + height/2 + 40` shape
                    frame.origin.y =
                        PanelController.panelTopAnchor(visibleFrame: vis) - newHeight
                }
                recenterOnNextResize = false
            } else {
                // Pin top during search-mode typing so results grow
                // downward without input field jumping
                let oldTop = panel.frame.origin.y + panel.frame.size.height
                frame.origin.y = oldTop - newHeight
            }

            panel.setFrame(frame, display: true, animate: false)
            // Same shadow-staleness fix as
            // `PanelController.resize`: re-rasterise panel shadow
            // so it tracks new rounded silhouette. Earlier
            // `layoutSubtreeIfNeeded` here was the ringleader of a
            // phantom-snapshot artefact on shrink - see comment in
            // `PanelController.resize`
            panel.invalidateShadow()
        }
    }
}
