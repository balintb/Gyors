import SwiftUI

/// Visual theme applied across panel
///
/// A theme either uses system's vibrancy material (blends with
/// whatever is behind panel) or paints a solid tinted background.
/// Text and accent colors override defaults for custom themes
///
/// ## Translucency model
///
/// `backgroundOpacity` and `usesBlur` are *theme-level* defaults
/// that user can override per-install via `theme.opacity` /
/// `theme.blur` in `config.json`. Whatever value is in effect
/// drives only panel background - input text, result rows, and
/// glyphs all paint at full alpha on top, so translucency never
/// reads as "everything is faded."
///
/// `usesBlur=true` puts an `NSVisualEffectView` (`.hudWindow` /
/// `.behindWindow`) underneath tint so desktop blurs through;
/// `false` skips that and you see whatever's behind through alpha
/// alone (faster, but less Spotlight-y)
struct Theme: Hashable {
    let id: String
    let label: String

    /// When true, panel uses system vibrancy material and
    /// `panelTint` / `backgroundOpacity` / `usesBlur` are ignored -
    /// OS picks both blur and tint for you
    let usesSystemMaterial: Bool
    /// Solid background color when `usesSystemMaterial` is false.
    /// Stored *opaque*; effective alpha comes from
    /// `backgroundOpacity` so "translucency" knob has one canonical
    /// home
    let panelTint: Color
    /// 0.0 -> fully transparent, 1.0 -> fully opaque tint. Applied
    /// as `panelTint.opacity(backgroundOpacity)` at render time
    let backgroundOpacity: Double
    /// Whether to layer `NSVisualEffectView` under tint
    let usesBlur: Bool
    let accent: Color
    let primaryText: Color
    let secondaryText: Color
    let tertiaryText: Color
    let border: Color
    let borderWidth: CGFloat
    let cornerRadius: CGFloat
    /// Selection background opacity applied to accent color
    let selectionOpacity: Double
}

enum Themes {
    /// Built-in themes. Custom themes loaded from disk are appended
    /// by `ThemeManager` at runtime - call
    /// `ThemeManager.shared.allThemes` when you need full list
    /// including user-imported ones
    static let all: [Theme] = [
        system, midnight, sunset, forest, monochrome, neon,
        nord, dracula, solarizedDark, tokyoNight,
    ]

    static let system = Theme(
        id: "system",
        label: "System",
        usesSystemMaterial: true,
        panelTint: .clear,
        backgroundOpacity: 1.0,
        usesBlur: true,
        accent: Color.accentColor,
        primaryText: Color.primary,
        secondaryText: Color.secondary,
        tertiaryText: Color.primary.opacity(0.4),
        border: Color.primary.opacity(0.08),
        borderWidth: 1,
        cornerRadius: 14,
        selectionOpacity: 0.18
    )

    static let midnight = Theme(
        id: "midnight",
        label: "Midnight",
        usesSystemMaterial: false,
        panelTint: Color(red: 0.055, green: 0.07, blue: 0.12),
        // Spotlight-territory translucency - blurred desktop reads
        // through clearly enough to feel "floating" without washing
        // out result rows. ~0.75 is sweet spot for most dark
        // themes; tune per-theme below where palette demands it
        backgroundOpacity: 0.92,
        usesBlur: true,
        accent: Color(red: 0.58, green: 0.35, blue: 0.95),
        primaryText: Color(red: 0.96, green: 0.96, blue: 0.98),
        secondaryText: Color(red: 0.70, green: 0.72, blue: 0.82),
        tertiaryText: Color(red: 0.52, green: 0.54, blue: 0.62),
        border: Color(red: 0.58, green: 0.35, blue: 0.95).opacity(0.20),
        borderWidth: 1,
        cornerRadius: 14,
        selectionOpacity: 0.28
    )

    static let sunset = Theme(
        id: "sunset",
        label: "Sunset",
        usesSystemMaterial: false,
        panelTint: Color(red: 0.18, green: 0.09, blue: 0.16),
        backgroundOpacity: 0.9,
        usesBlur: true,
        accent: Color(red: 1.00, green: 0.55, blue: 0.30),
        primaryText: Color(red: 0.99, green: 0.95, blue: 0.88),
        // Strip per-color `.opacity(...)` we used to bake in - when
        // panel background is also translucent that multiplies into
        // mushy near-invisible text. Solid colours, muted by HSL
        // not alpha
        secondaryText: Color(red: 0.85, green: 0.72, blue: 0.58),
        tertiaryText: Color(red: 0.62, green: 0.50, blue: 0.50),
        border: Color(red: 1.00, green: 0.55, blue: 0.30).opacity(0.18),
        borderWidth: 1,
        cornerRadius: 14,
        selectionOpacity: 0.24
    )

    static let forest = Theme(
        id: "forest",
        label: "Forest",
        usesSystemMaterial: false,
        panelTint: Color(red: 0.05, green: 0.11, blue: 0.08),
        backgroundOpacity: 0.92,
        usesBlur: true,
        accent: Color(red: 0.32, green: 0.82, blue: 0.60),
        primaryText: Color(red: 0.93, green: 0.97, blue: 0.90),
        secondaryText: Color(red: 0.70, green: 0.85, blue: 0.74),
        // Solid (no `.opacity()`) so text stays readable over a
        // translucent panel background
        tertiaryText: Color(red: 0.48, green: 0.62, blue: 0.53),
        border: Color(red: 0.32, green: 0.82, blue: 0.60).opacity(0.16),
        borderWidth: 1,
        cornerRadius: 14,
        selectionOpacity: 0.22
    )

    static let monochrome = Theme(
        id: "monochrome",
        label: "Monochrome",
        usesSystemMaterial: false,
        panelTint: Color(white: 0.085),
        // Tighter (more opaque) for high-contrast monochrome look -
        // heavy translucency washes out lack-of-color and result
        // list becomes hard to scan
        backgroundOpacity: 0.92,
        usesBlur: true,
        accent: Color(white: 0.92),
        primaryText: Color(white: 0.97),
        secondaryText: Color(white: 0.60),
        tertiaryText: Color(white: 0.40),
        border: Color(white: 1.0).opacity(0.12),
        borderWidth: 1,
        cornerRadius: 14,
        selectionOpacity: 0.14
    )

    static let neon = Theme(
        id: "neon",
        label: "Neon",
        usesSystemMaterial: false,
        panelTint: Color(red: 0.02, green: 0.02, blue: 0.06),
        // Near-opaque on purpose: neon's whole identity is
        // saturated cyan glow against deep black, which a heavy
        // blur would dilute
        backgroundOpacity: 0.94,
        usesBlur: false,
        accent: Color(red: 0.10, green: 0.98, blue: 0.88),
        primaryText: Color(red: 0.96, green: 1.00, blue: 1.00),
        secondaryText: Color(red: 0.60, green: 0.90, blue: 0.95),
        // Solid muted teal - no alpha multiplier on text colours
        tertiaryText: Color(red: 0.40, green: 0.50, blue: 0.58),
        border: Color(red: 0.10, green: 0.98, blue: 0.88).opacity(0.28),
        borderWidth: 1,
        cornerRadius: 12,
        selectionOpacity: 0.26
    )

    // Based on Nord palette (nordtheme.com)
    static let nord = Theme(
        id: "nord",
        label: "Nord",
        usesSystemMaterial: false,
        panelTint: Color(red: 0.180, green: 0.204, blue: 0.251), // #2e3440
        backgroundOpacity: 0.92,
        usesBlur: true,
        accent: Color(red: 0.533, green: 0.753, blue: 0.816),                  // #88c0d0
        primaryText: Color(red: 0.925, green: 0.937, blue: 0.957),             // #eceff4
        secondaryText: Color(red: 0.667, green: 0.698, blue: 0.745),           // #aab0be
        tertiaryText: Color(red: 0.518, green: 0.565, blue: 0.624),            // #84909f
        border: Color(red: 0.533, green: 0.753, blue: 0.816).opacity(0.16),
        borderWidth: 1,
        cornerRadius: 14,
        selectionOpacity: 0.22
    )

    // Based on Dracula palette (draculatheme.com)
    static let dracula = Theme(
        id: "dracula",
        label: "Dracula",
        usesSystemMaterial: false,
        panelTint: Color(red: 0.157, green: 0.165, blue: 0.212), // #282a36
        backgroundOpacity: 0.92,
        usesBlur: true,
        accent: Color(red: 1.00, green: 0.475, blue: 0.776),                   // #ff79c6
        primaryText: Color(red: 0.973, green: 0.973, blue: 0.949),             // #f8f8f2
        secondaryText: Color(red: 0.733, green: 0.753, blue: 0.816),           // #bbc1d0
        tertiaryText: Color(red: 0.537, green: 0.557, blue: 0.604),            // #898e9a
        border: Color(red: 1.00, green: 0.475, blue: 0.776).opacity(0.18),
        borderWidth: 1,
        cornerRadius: 14,
        selectionOpacity: 0.24
    )

    // Based on Solarized Dark (ethanschoonover.com/solarized)
    static let solarizedDark = Theme(
        id: "solarized-dark",
        label: "Solarized Dark",
        usesSystemMaterial: false,
        panelTint: Color(red: 0.000, green: 0.169, blue: 0.212), // #002b36
        backgroundOpacity: 0.92,
        usesBlur: true,
        accent: Color(red: 0.149, green: 0.545, blue: 0.824),                  // #268bd2
        primaryText: Color(red: 0.514, green: 0.580, blue: 0.588),             // #839496
        secondaryText: Color(red: 0.345, green: 0.431, blue: 0.459),           // #586e75
        tertiaryText: Color(red: 0.259, green: 0.345, blue: 0.373),            // #425860
        border: Color(red: 0.149, green: 0.545, blue: 0.824).opacity(0.18),
        borderWidth: 1,
        cornerRadius: 14,
        selectionOpacity: 0.20
    )

    // Based on Tokyo Night
    static let tokyoNight = Theme(
        id: "tokyo-night",
        label: "Tokyo Night",
        usesSystemMaterial: false,
        panelTint: Color(red: 0.102, green: 0.114, blue: 0.169), // #1a1b2b
        backgroundOpacity: 0.92,
        usesBlur: true,
        accent: Color(red: 0.482, green: 0.639, blue: 0.937),                  // #7aa2ef
        primaryText: Color(red: 0.773, green: 0.824, blue: 0.933),             // #c5d3ee
        secondaryText: Color(red: 0.565, green: 0.604, blue: 0.725),           // #9099b9
        tertiaryText: Color(red: 0.392, green: 0.431, blue: 0.561),            // #646e8f
        border: Color(red: 0.482, green: 0.639, blue: 0.937).opacity(0.18),
        borderWidth: 1,
        cornerRadius: 14,
        selectionOpacity: 0.22
    )

    static func byId(_ id: String) -> Theme? {
        all.first(where: { $0.id == id })
    }
}
