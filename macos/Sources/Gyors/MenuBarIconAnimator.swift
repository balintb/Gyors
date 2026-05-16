import AppKit

/// Drives menu-bar icon's "busy" gradient sweep and one-shot strobe
/// used for clipboard copies
///
/// Busy mode - animates a single-hue gradient that sweeps L->R
/// across icon glyph (~30 fps, ~1.6s per cycle). Hue stays
/// constant; only brightness varies along sweep, so user gets
/// unmistakable "something is happening" feedback without a rainbow
/// eyesore
///
/// Flash mode - a brief 350 ms tint pulse for events that
/// complete instantly (e.g., a clipboard copy). Decays from full
/// accent -> resting color so eye catches it without icon changing
/// shape
///
/// Busy preempts flash: if a sustained operation is running we skip
/// flashes (sweep is already saying "active")
///
/// ## Color choice - why we hardcode white
///
/// macOS menu bars are functionally always dark in practice. Even
/// in "Light" mode, menu bar uses a dark lively material when
/// wallpaper is anything but a solid bright color, and
/// `NSStatusBarButton.effectiveAppearance` is unreliable: it often
/// resolves to *light* system appearance even when user's actual
/// menu bar is dark
///
/// Using `NSColor.labelColor` inside `lockFocus` is even worse - it
/// resolves against offscreen image's default light appearance, so
/// it returns black on a dark menu bar (user's reported bug)
///
/// We sidestep both traps by hardcoding resting glyph color to
/// white. This matches common case (~99% of menu bars), and
/// degrades on a truly-light menu bar (white on light
/// is low-contrast during animation, but animation only runs while
/// busy - not at rest, where system-tinted template image still
/// adapts correctly)
///
/// ## Rendering pipeline - `destinationIn`, not `sourceIn`
///
/// `sourceIn` (icon as dst, gradient as src) was obvious choice but
/// proved fragile: `NSGradient.draw` and `NSImage.draw` dont
/// reliably honor surrounding `compositingOperation` when called
/// via `lockFocus` - sometimes operation gets reset by inner draw
/// call's own state push, leaving previously-drawn black PDF
/// visible underneath gradient
///
/// Solid shape: draw gradient first (filling whole canvas), then
/// `destinationIn` with icon as alpha mask. That keeps gradient
/// where icon has alpha and clears it elsewhere
@MainActor
final class MenuBarIconAnimator {
    private weak var statusItem: NSStatusItem?
    private let baseIcon: NSImage

    private enum Mode {
        case idle
        case busy
        case flash(start: Date)
    }

    private var mode: Mode = .idle
    private var timer: Timer?
    private var sweepPhase: CGFloat = 0

    /// Peak (sweep highlight) color. System accent matches user's
    /// chosen theme highlight, so moving band feels native
    private var glowColor: NSColor { .controlAccentColor }

    /// Resting glyph color - what icon looks like when menu bar
    /// renders it as a normal template (white on a dark menu bar,
    /// black on a light one). Gradient endpoints sit at this
    /// color, so SWEEP looks like a colored highlight passing
    /// through usual icon - not a colored icon pulsing in place
    ///
    /// `NSApp.effectiveAppearance` is right signal here:
    /// `statusItem.button.effectiveAppearance` is unreliable
    /// (frequently reports light even on a dark menu bar), but
    /// `NSApp.effectiveAppearance` reflects global system mode
    /// menu bar follows by default
    private var menuBarRestColor: NSColor {
        let isDark = NSApp.effectiveAppearance.bestMatch(from: [
            .darkAqua, .vibrantDark,
            .accessibilityHighContrastDarkAqua,
            .accessibilityHighContrastVibrantDark,
        ]) != nil
        return isDark ? .white : .black
    }

    /// Gradient stops - recomputed each frame so a live accent or
    /// appearance change flows through without restart. Cheap (a
    /// few color-component reads), trivial vs. rasterise cost
    private var gradientStops: (rest: NSColor, peak: NSColor) {
        (rest: menuBarRestColor, peak: glowColor)
    }

    /// Roughly 30 fps. Lower stutters; higher costs CPU for no
    /// visible gain on a tiny menu-bar glyph
    private let frameInterval: TimeInterval = 1.0 / 30.0

    /// One full L->R sweep takes ~1.6s - slow enough to read as
    /// "thinking", fast enough that you can tell it's animated
    private let sweepIncrementPerFrame: CGFloat = 0.020

    /// Total duration of strobe. Long enough to register, short
    /// enough not to feel intrusive after a routine copy
    private let flashDuration: TimeInterval = 0.35

    init(statusItem: NSStatusItem, baseIcon: NSImage) {
        self.statusItem = statusItem
        self.baseIcon = baseIcon
    }

    // MARK: - Public state changes

    func setBusy(_ busy: Bool) {
        if busy {
            // Calling `setBusy(true)` on top of an active flash
            // drops flash and switches to sustained sweep
            mode = .busy
            sweepPhase = 0
            ensureTimerRunning()
        } else {
            mode = .idle
            stopTimer()
            restoreBaseIcon()
        }
    }

    func flash() {
        // While a sustained sweep is running, skip flashes so we
        // dont strobe through gradient
        if case .busy = mode { return }
        mode = .flash(start: Date())
        ensureTimerRunning()
    }

    // MARK: - Timer / rendering

    private func ensureTimerRunning() {
        guard timer == nil else { return }
        // Use a Timer rather than CADisplayLink so we dont depend
        // on a particular run-loop wiring; menu-bar item button
        // isn't attached to an NSWindow we can sync to anyway
        let t = Timer(timeInterval: frameInterval, repeats: true) { [weak self] _ in
            Task { @MainActor [weak self] in
                self?.tick()
            }
        }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    private func stopTimer() {
        timer?.invalidate()
        timer = nil
    }

    private func tick() {
        guard let button = statusItem?.button else {
            stopTimer()
            return
        }
        switch mode {
        case .idle:
            stopTimer()
            restoreBaseIcon()
        case .busy:
            sweepPhase += sweepIncrementPerFrame
            if sweepPhase >= 1.0 { sweepPhase -= 1.0 }
            if let frame = renderBusyFrame(phase: sweepPhase) {
                button.image = frame
            }
        case .flash(let start):
            let elapsed = Date().timeIntervalSince(start)
            if elapsed >= flashDuration {
                mode = .idle
                stopTimer()
                restoreBaseIcon()
                return
            }
            // Ease brightness out (cubic-ish) so strobe peaks
            // immediately and trails off. A linear fade looks
            // lifeless
            let t = CGFloat(elapsed / flashDuration)
            let intensity = (1 - t) * (1 - t)
            if let frame = renderTintedIcon(intensity: intensity) {
                button.image = frame
            }
        }
    }

    // MARK: - Icon rendering

    private func restoreBaseIcon() {
        guard let button = statusItem?.button else { return }
        let img = (baseIcon.copy() as? NSImage) ?? baseIcon
        img.size = baseIcon.size
        img.isTemplate = true
        button.image = img
    }

    /// Render a frame of sweep gradient using an explicit
    /// `NSBitmapImageRep`. Earlier `lockFocus` + CGContext pipeline
    /// was fragile - `cgImage(forProposedRect:context:hints:)` on a
    /// PDF-backed `NSImage` can return nil or an alpha-stripped
    /// representation, which made icon collapse to fully
    /// transparent
    ///
    /// 1. Stand up a fresh RGBA bitmap.
    /// 2. Fill it with a horizontal gradient that extends past
    ///    icon edges (so phase wrap-around is invisible).
    /// 3. Compose icon over gradient with `.destinationIn` -
    ///    Cocoa's `NSImage.draw(operation:)` API handles blend
    ///    mode itself, no CGContext state-machine fights needed
    private func renderBusyFrame(phase: CGFloat) -> NSImage? {
        guard let bitmap = makeBitmap() else { return nil }
        let size = baseIcon.size
        let bounds = NSRect(origin: .zero, size: size)

        NSGraphicsContext.saveGraphicsState()
        defer { NSGraphicsContext.restoreGraphicsState() }
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: bitmap)

        let rest = opaque(gradientStops.rest)
        let peak = opaque(gradientStops.peak)
        if let gradient = NSGradient(colors: [rest, peak, rest]) {
            let sweepWidth = size.width * 0.6
            let centerX = -sweepWidth / 2 + (size.width + sweepWidth) * phase
            let from = NSPoint(x: centerX - sweepWidth / 2, y: size.height / 2)
            let to = NSPoint(x: centerX + sweepWidth / 2, y: size.height / 2)
            // `drawsBefore/AfterEndingLocation` keeps area outside
            // [from, to] filled with corresponding end color, so
            // canvas is fully painted before we mask with icon
            gradient.draw(
                from: from,
                to: to,
                options: [.drawsBeforeStartingLocation, .drawsAfterEndingLocation]
            )
        } else {
            rest.setFill()
            bounds.fill()
        }

        // Mask via icon - `.destinationIn` keeps gradient where
        // icon's alpha is opaque, clears it elsewhere
        baseIcon.draw(
            in: bounds,
            from: .zero,
            operation: .destinationIn,
            fraction: 1.0
        )

        let result = NSImage(size: size)
        result.addRepresentation(bitmap)
        result.isTemplate = false
        return result
    }

    /// Render icon glyph in a uniform tint blended between
    /// gradient's rest tone (intensity=0) and its peak
    /// (intensity=1). Same fill-then-mask pattern as busy renderer
    private func renderTintedIcon(intensity: CGFloat) -> NSImage? {
        guard let bitmap = makeBitmap() else { return nil }
        let size = baseIcon.size
        let bounds = NSRect(origin: .zero, size: size)

        NSGraphicsContext.saveGraphicsState()
        defer { NSGraphicsContext.restoreGraphicsState() }
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: bitmap)

        let tint = opaque(mix(gradientStops.rest, gradientStops.peak,
                              t: max(0, min(1, intensity))))
        tint.setFill()
        bounds.fill()

        baseIcon.draw(
            in: bounds,
            from: .zero,
            operation: .destinationIn,
            fraction: 1.0
        )

        let result = NSImage(size: size)
        result.addRepresentation(bitmap)
        result.isTemplate = false
        return result
    }

    /// Allocate a fresh RGBA bitmap at retina resolution. Logical
    /// size is locked to `baseIcon.size` so callers draw in points;
    /// underlying pixel buffer stays sharp at 2x scale
    private func makeBitmap() -> NSBitmapImageRep? {
        let size = baseIcon.size
        let scale: CGFloat = 2
        let pw = max(1, Int(size.width * scale))
        let ph = max(1, Int(size.height * scale))
        guard let bitmap = NSBitmapImageRep(
            bitmapDataPlanes: nil,
            pixelsWide: pw,
            pixelsHigh: ph,
            bitsPerSample: 8,
            samplesPerPixel: 4,
            hasAlpha: true,
            isPlanar: false,
            colorSpaceName: .deviceRGB,
            bytesPerRow: 0,
            bitsPerPixel: 0
        ) else { return nil }
        bitmap.size = size
        return bitmap
    }

    /// Force opaque alpha on a color so it doesn't accidentally
    /// composite with menu bar background
    private func opaque(_ c: NSColor) -> NSColor {
        let c = c.usingColorSpace(.sRGB) ?? c
        return NSColor(
            srgbRed: c.redComponent,
            green: c.greenComponent,
            blue: c.blueComponent,
            alpha: 1.0
        )
    }

    private func mix(_ a: NSColor, _ b: NSColor, t: CGFloat) -> NSColor {
        let a = a.usingColorSpace(.sRGB) ?? a
        let b = b.usingColorSpace(.sRGB) ?? b
        let t = max(0, min(1, t))
        return NSColor(
            srgbRed: a.redComponent + (b.redComponent - a.redComponent) * t,
            green: a.greenComponent + (b.greenComponent - a.greenComponent) * t,
            blue: a.blueComponent + (b.blueComponent - a.blueComponent) * t,
            alpha: 1.0
        )
    }
}
