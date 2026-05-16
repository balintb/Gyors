import AppKit
import Foundation
import SwiftUI

/// Wire format for user-authored themes. Colors are `#rrggbb` or
/// `#rrggbbaa` hex strings. Users import via
/// `gyors://theme?import=<base64>` and importer converts to a
/// `Theme` for active session
struct CustomTheme: Codable {
    let id: String
    let label: String
    let usesSystemMaterial: Bool
    let panelTint: String
    let accent: String
    let primaryText: String
    let secondaryText: String
    let tertiaryText: String
    let border: String
    let borderWidth: Double
    let cornerRadius: Double
    let selectionOpacity: Double
    /// Optional in wire format so older themes (created before
    /// translucency knob existed) still import - they fall back to
    /// sensible defaults rather than failing round-trip
    let backgroundOpacity: Double?
    let usesBlur: Bool?

    enum CodingKeys: String, CodingKey {
        case id, label
        case usesSystemMaterial = "uses_system_material"
        case panelTint = "panel_tint"
        case accent
        case primaryText = "primary_text"
        case secondaryText = "secondary_text"
        case tertiaryText = "tertiary_text"
        case border
        case borderWidth = "border_width"
        case cornerRadius = "corner_radius"
        case selectionOpacity = "selection_opacity"
        case backgroundOpacity = "background_opacity"
        case usesBlur = "uses_blur"
    }

    enum ImportError: Error, LocalizedError {
        case badColor(field: String, value: String)
        case emptyId

        var errorDescription: String? {
            switch self {
            case .badColor(let f, let v): return "Invalid color for \(f): \(v)"
            case .emptyId: return "Theme id cannot be empty."
            }
        }
    }

    func toTheme() throws -> Theme {
        if id.trimmingCharacters(in: .whitespaces).isEmpty {
            throw ImportError.emptyId
        }
        return Theme(
            id: id,
            label: label.isEmpty ? id : label,
            usesSystemMaterial: usesSystemMaterial,
            panelTint: try Color.fromHex(panelTint, field: "panel_tint"),
            backgroundOpacity: backgroundOpacity ?? 0.86,
            usesBlur: usesBlur ?? true,
            accent: try Color.fromHex(accent, field: "accent"),
            primaryText: try Color.fromHex(primaryText, field: "primary_text"),
            secondaryText: try Color.fromHex(secondaryText, field: "secondary_text"),
            tertiaryText: try Color.fromHex(tertiaryText, field: "tertiary_text"),
            border: try Color.fromHex(border, field: "border"),
            borderWidth: CGFloat(borderWidth),
            cornerRadius: CGFloat(cornerRadius),
            selectionOpacity: selectionOpacity
        )
    }

    static func from(theme: Theme) -> CustomTheme {
        CustomTheme(
            id: theme.id,
            label: theme.label,
            usesSystemMaterial: theme.usesSystemMaterial,
            panelTint: theme.panelTint.toHexString(),
            accent: theme.accent.toHexString(),
            primaryText: theme.primaryText.toHexString(),
            secondaryText: theme.secondaryText.toHexString(),
            tertiaryText: theme.tertiaryText.toHexString(),
            border: theme.border.toHexString(),
            borderWidth: Double(theme.borderWidth),
            cornerRadius: Double(theme.cornerRadius),
            selectionOpacity: theme.selectionOpacity,
            backgroundOpacity: theme.backgroundOpacity,
            usesBlur: theme.usesBlur
        )
    }
}

extension Color {
    static func fromHex(_ hex: String, field: String) throws -> Color {
        var s = hex.trimmingCharacters(in: .whitespaces)
        if s.hasPrefix("#") { s.removeFirst() }
        guard (s.count == 6 || s.count == 8) && s.allSatisfy({ $0.isHexDigit }) else {
            throw CustomTheme.ImportError.badColor(field: field, value: hex)
        }
        let val = UInt64(s, radix: 16) ?? 0
        if s.count == 6 {
            let r = Double((val >> 16) & 0xff) / 255.0
            let g = Double((val >> 8) & 0xff) / 255.0
            let b = Double(val & 0xff) / 255.0
            return Color(red: r, green: g, blue: b)
        }
        let r = Double((val >> 24) & 0xff) / 255.0
        let g = Double((val >> 16) & 0xff) / 255.0
        let b = Double((val >> 8) & 0xff) / 255.0
        let a = Double(val & 0xff) / 255.0
        return Color(red: r, green: g, blue: b, opacity: a)
    }

    /// Serialize to `#rrggbb` (or `#rrggbbaa` when alpha < 1)
    func toHexString() -> String {
        let ns = NSColor(self).usingColorSpace(.deviceRGB) ?? NSColor.black
        let r = Int((ns.redComponent   * 255).rounded())
        let g = Int((ns.greenComponent * 255).rounded())
        let b = Int((ns.blueComponent  * 255).rounded())
        let a = ns.alphaComponent
        if a >= 0.9995 {
            return String(format: "#%02x%02x%02x", r, g, b)
        }
        let ai = Int((a * 255).rounded())
        return String(format: "#%02x%02x%02x%02x", r, g, b, ai)
    }
}
