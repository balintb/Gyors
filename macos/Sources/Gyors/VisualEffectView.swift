import AppKit
import SwiftUI

/// SwiftUI wrapper around `NSVisualEffectView` - Apple's blur-and-
/// vibrancy substrate behind Spotlight, Notification Center, and
/// the menu bar. We use it as the bottom layer of the panel
/// background. Text and chrome composite on top at full alpha, so
/// only the background blurs/translucents - the input text and
/// result rows stay pin-sharp
///
/// Material choice rationale:
/// - `.hudWindow` reads as "floating UI" and matches Spotlight's
///   blur level. Used for `.behindWindow` blending so the desktop
///   shows through.
/// - `.popover` is what Apple uses for menu-style panels. Too
///   muted for a launcher.
/// - `.headerView` is too cool/blue for warm themes
///
/// `.hudWindow` + `.behindWindow` is the closest match to the
/// "translucent floating panel" mental model
struct VisualEffectView: NSViewRepresentable {
    var material: NSVisualEffectView.Material = .hudWindow
    var blendingMode: NSVisualEffectView.BlendingMode = .behindWindow
    /// `state == .active` keeps blurring even when the window
    /// loses key focus. Without this the blur fades to opaque
    /// during transitions, which looks janky
    var state: NSVisualEffectView.State = .active

    func makeNSView(context: Context) -> NSVisualEffectView {
        let v = NSVisualEffectView()
        v.material = material
        v.blendingMode = blendingMode
        v.state = state
        v.isEmphasized = false
        // The panel itself rounds its corners via SwiftUI clipping;
        // making the effect view layer-backed lets the rounded
        // clip mask propagate without the visible square
        // edge bleed you sometimes see with NSVisualEffectView
        v.wantsLayer = true
        return v
    }

    func updateNSView(_ v: NSVisualEffectView, context: Context) {
        v.material = material
        v.blendingMode = blendingMode
        v.state = state
    }
}
