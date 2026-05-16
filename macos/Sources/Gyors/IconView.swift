import SwiftUI
import AppKit

struct IconView: View {
    let kind: UInt8
    let value: String

    var body: some View {
        switch kind {
        case 0:
            // No icon - render empty space, dont fall back to a
            // placeholder SF symbol (which looked like a stray
            // checkbox next to results)
            Color.clear
        case 1:
            Image(systemName: value.isEmpty ? "app" : value)
                .resizable()
                .aspectRatio(contentMode: .fit)
                .foregroundStyle(.primary)
        case 2:
            Image(nsImage: NSWorkspace.shared.icon(forFile: value))
                .resizable()
                .aspectRatio(contentMode: .fit)
        case 3:
            Image(nsImage: NSWorkspace.shared.icon(forFile: value))
                .resizable()
                .aspectRatio(contentMode: .fit)
        case 4:
            ColorSwatch(hexString: value)
        case 5:
            // Arbitrary text glyph (emoji, symbol, ...)
            Text(value)
                .font(.system(size: 22))
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
        case 6:
            // Bitmap thumbnail loaded from disk - used for clipboard
            // image entries. NSImage(contentsOfFile:) handles all
            // formats Cocoa understands (PNG today; JPEG/HEIC later
            // if we extend PasteboardWatcher)
            if let img = NSImage(contentsOfFile: value) {
                Image(nsImage: img)
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .clipShape(RoundedRectangle(cornerRadius: 4, style: .continuous))
            } else {
                Image(systemName: "photo")
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .foregroundStyle(.secondary)
            }
        default:
            Color.clear
        }
    }
}

struct ColorSwatch: View {
    let hexString: String

    var body: some View {
        RoundedRectangle(cornerRadius: 6, style: .continuous)
            .fill(Color(hexString: hexString) ?? Color.gray)
            .overlay(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .strokeBorder(Color.primary.opacity(0.12), lineWidth: 0.5)
            )
    }
}

extension Color {
    init?(hexString: String) {
        var s = hexString.trimmingCharacters(in: .whitespaces)
        if s.hasPrefix("#") { s.removeFirst() }
        guard s.count == 6, let val = UInt32(s, radix: 16) else { return nil }
        let r = Double((val >> 16) & 0xff) / 255.0
        let g = Double((val >> 8) & 0xff) / 255.0
        let b = Double(val & 0xff) / 255.0
        self = Color(red: r, green: g, blue: b)
    }
}
