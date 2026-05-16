import AppKit
import ApplicationServices
import Foundation

/// Resizes current frontmost window to a named geometry via
/// Accessibility API. Using AX (instead of AppleScript->System
/// Events) means we only need Accessibility permission - no
/// Automation TCC prompt
enum WindowArranger {
    enum Geometry: String {
        case leftHalf     = "left-half"
        case rightHalf    = "right-half"
        case topHalf      = "top-half"
        case bottomHalf   = "bottom-half"
        case full         = "full"
        case center       = "center"
        case leftThird    = "left-third"
        case centerThird  = "center-third"
        case rightThird   = "right-third"
        case topLeft      = "top-left"
        case topRight     = "top-right"
        case bottomLeft   = "bottom-left"
        case bottomRight  = "bottom-right"
    }

    static func arrange(geometryId: String) {
        guard let geometry = Geometry(rawValue: geometryId) else {
            NSLog("gyors: unknown window geometry: \(geometryId)")
            return
        }
        if !AXIsProcessTrusted() {
            Accessibility.promptUserIfNeeded(
                reason: "Gyors needs Accessibility permission to move and resize windows (`wm …` commands)."
            )
            return
        }
        apply(geometry)
    }

    private static func apply(_ geometry: Geometry) {
        guard let screen = NSScreen.main else {
            NSLog("gyors: no main screen")
            return
        }
        // Get frontmost window via system-wide AX element
        let systemWide = AXUIElementCreateSystemWide()
        guard let axApp = copyAttr(systemWide, kAXFocusedApplicationAttribute as CFString) else {
            showError("No focused application found.")
            return
        }
        let app = unsafeBitCast(axApp, to: AXUIElement.self)
        guard let axWindow = copyAttr(app, kAXFocusedWindowAttribute as CFString) else {
            showError("The frontmost app has no focused window.")
            return
        }
        let window = unsafeBitCast(axWindow, to: AXUIElement.self)

        let (position, size) = frame(for: geometry, on: screen)
        setFrame(window: window, position: position, size: size)
    }

    /// Compute target position (AX / top-left origin) and size for
    /// given geometry within screen's `visibleFrame` (menu bar +
    /// Dock excluded)
    private static func frame(for geometry: Geometry, on screen: NSScreen) -> (CGPoint, CGSize) {
        let fullHeight = screen.frame.height
        let visible = screen.visibleFrame
        // VisibleFrame origin is Cartesian (bottom-left). AX wants
        // top-left origin. Flip Y
        let axY = fullHeight - (visible.origin.y + visible.height)
        let sx = visible.origin.x
        let sy = axY
        let sw = visible.width
        let sh = visible.height

        switch geometry {
        case .leftHalf:
            return (CGPoint(x: sx, y: sy), CGSize(width: sw / 2, height: sh))
        case .rightHalf:
            return (CGPoint(x: sx + sw / 2, y: sy), CGSize(width: sw / 2, height: sh))
        case .topHalf:
            return (CGPoint(x: sx, y: sy), CGSize(width: sw, height: sh / 2))
        case .bottomHalf:
            return (CGPoint(x: sx, y: sy + sh / 2), CGSize(width: sw, height: sh / 2))
        case .full:
            return (CGPoint(x: sx, y: sy), CGSize(width: sw, height: sh))
        case .center:
            let w = sw * 0.6
            let h = sh * 0.7
            return (
                CGPoint(x: sx + (sw - w) / 2, y: sy + (sh - h) / 2),
                CGSize(width: w, height: h)
            )
        case .leftThird:
            return (CGPoint(x: sx, y: sy), CGSize(width: sw / 3, height: sh))
        case .centerThird:
            return (CGPoint(x: sx + sw / 3, y: sy), CGSize(width: sw / 3, height: sh))
        case .rightThird:
            return (CGPoint(x: sx + sw * 2 / 3, y: sy), CGSize(width: sw / 3, height: sh))
        case .topLeft:
            return (CGPoint(x: sx, y: sy), CGSize(width: sw / 2, height: sh / 2))
        case .topRight:
            return (CGPoint(x: sx + sw / 2, y: sy), CGSize(width: sw / 2, height: sh / 2))
        case .bottomLeft:
            return (CGPoint(x: sx, y: sy + sh / 2), CGSize(width: sw / 2, height: sh / 2))
        case .bottomRight:
            return (CGPoint(x: sx + sw / 2, y: sy + sh / 2), CGSize(width: sw / 2, height: sh / 2))
        }
    }

    private static func setFrame(window: AXUIElement, position: CGPoint, size: CGSize) {
        var p = position
        var s = size
        // Order matters for some apps: set position first, then size
        if let pValue = AXValueCreate(.cgPoint, &p) {
            AXUIElementSetAttributeValue(window, kAXPositionAttribute as CFString, pValue)
        }
        if let sValue = AXValueCreate(.cgSize, &s) {
            AXUIElementSetAttributeValue(window, kAXSizeAttribute as CFString, sValue)
        }
        // Some apps clamp first set; setting position again after
        // size ensures window ends up where we asked
        if let pValue = AXValueCreate(.cgPoint, &p) {
            AXUIElementSetAttributeValue(window, kAXPositionAttribute as CFString, pValue)
        }
    }

    /// Wrap `AXUIElementCopyAttributeValue` to return `Optional`
    private static func copyAttr(_ element: AXUIElement, _ attribute: CFString) -> AnyObject? {
        var value: AnyObject?
        let err = AXUIElementCopyAttributeValue(element, attribute, &value)
        return err == .success ? value : nil
    }

    private static func showError(_ message: String) {
        DispatchQueue.main.async {
            NSApp.activate(ignoringOtherApps: true)
            let alert = NSAlert()
            alert.messageText = "Window command failed"
            alert.informativeText = message
            alert.alertStyle = .warning
            alert.addButton(withTitle: "OK")
            alert.runModal()
        }
    }
}
