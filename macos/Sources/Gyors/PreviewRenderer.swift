import SwiftUI

/// Inline preview rendering: markdown blocks, syntax-highlighted
/// code for json/yaml/toml, plain monospace fallback. All branches
/// respect user's `preview_markdown` / `preview_syntax` config
/// toggles; with both disabled output is identical to before
/// feature existed, so this change never forces a UX user didn't
/// opt into
struct PreviewRenderer: View {
    let text: String
    let language: String?
    let theme: Theme

    /// Config toggles are pulled fresh each render - cheap (a file
    /// read happens inside `Config.load`, but frequency here is
    /// per-view-update, not per-keystroke - user opens a preview,
    /// preview reads once, done)
    private var cfg: Config { Config.load() }

    var body: some View {
        switch detectedRenderer {
        case .markdown:
            MarkdownView(text: text, theme: theme)
        case .syntax(let lang):
            SyntaxHighlightView(text: text, language: lang, theme: theme)
        case .plain:
            PlainMonoView(text: text, theme: theme)
        }
    }

    private enum Choice {
        case markdown
        case syntax(CodeLanguage)
        case plain
    }

    private var detectedRenderer: Choice {
        let lang = (language ?? "").lowercased()
        if lang == "markdown" || lang == "md" {
            // Two independent switches: "render markdown" produces a
            // formatted view; "syntax" falls back to a colourised raw
            // source view. Both off -> plain monospace, same as before
            if cfg.effectivePreviewMarkdown { return .markdown }
            if cfg.effectivePreviewSyntax { return .syntax(.markdown) }
            return .plain
        }
        if cfg.effectivePreviewSyntax, let code = CodeLanguage(lang) {
            return .syntax(code)
        }
        return .plain
    }
}


private struct PlainMonoView: View {
    let text: String
    let theme: Theme
    var body: some View {
        Text(text)
            .font(.system(size: 12, weight: .regular, design: .monospaced))
            .foregroundStyle(theme.primaryText)
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}


enum CodeLanguage {
    case json
    case yaml
    case toml
    case markdown

    init?(_ s: String) {
        switch s {
        case "json": self = .json
        case "yaml", "yml": self = .yaml
        case "toml": self = .toml
        case "markdown", "md": self = .markdown
        default: return nil
        }
    }
}

private struct SyntaxHighlightView: View {
    let text: String
    let language: CodeLanguage
    let theme: Theme

    var body: some View {
        Text(SyntaxHighlighter.attributed(for: text, language: language, theme: theme))
            .font(.system(size: 12, weight: .regular, design: .monospaced))
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A coloured span within a source string. Emitted by tokenizer
/// and consumed by both SwiftUI `AttributedString` renderer and
/// AppKit `NSTextStorage` editor - same highlight logic, two
/// downstream attribute systems
struct TokenSpan {
    /// UTF-16 code-unit offsets (start inclusive, end exclusive).
    /// Tokenizers produce ASCII boundaries so this is exact for
    /// code; for markdown with mixed content worst case is a
    /// slightly off-by-one colour, never a crash
    let start: Int
    let end: Int
    let role: TokenRole
}

/// Semantic role of a coloured span. Kept abstract so themes can
/// pick actual `Color` per role without every tokenizer importing
/// `SwiftUI.Color`
enum TokenRole {
    case key          // JSON/YAML/TOML key, or markdown heading
    case marker       // markdown **, `, [, ] etc.
    case comment      // trailing YAML/TOML comments, fenced code body
    case link         // url inside [label](url)
    case literal      // json true/false/null keywords
}

/// Tiny per-language tokenizer. Deliberately regex-free - grammars
/// are simple enough that a single pass with a cursor produces
/// crisp colours without any regex-engine startup cost. Rendering
/// happens on main thread while panel is already visible, so
/// "fast" here means "not noticeable" rather than
/// "microbenchmark-worthy"
enum SyntaxHighlighter {
    static func attributed(
        for source: String,
        language: CodeLanguage,
        theme: Theme
    ) -> AttributedString {
        var out = AttributedString(source)
        out.foregroundColor = theme.primaryText
        switch language {
        case .json: tokenizeJson(source: source, into: &out, theme: theme)
        case .yaml: tokenizeYaml(source: source, into: &out, theme: theme)
        case .toml: tokenizeToml(source: source, into: &out, theme: theme)
        case .markdown: tokenizeMarkdown(source: source, into: &out, theme: theme)
        }
        return out
    }


    /// Colourise raw markdown *source* - this is fallback view when
    /// user has `preview_markdown` turned off but still wants
    /// `preview_syntax`. Also reused by in-editor syntax
    /// highlighter via `markdownSpans(source:)`
    private static func tokenizeMarkdown(
        source: String,
        into out: inout AttributedString,
        theme: Theme
    ) {
        for span in markdownSpans(source: source) {
            paint(source: source, from: span.start, to: span.end,
                  color: roleColor(span.role, theme: theme), in: &out)
        }
    }

    /// Map semantic roles to theme colours. Kept in one place so
    /// editor and preview use exact same palette
    static func roleColor(_ role: TokenRole, theme: Theme) -> Color {
        switch role {
        case .key:     return theme.accent
        case .marker:  return theme.accent
        case .comment: return theme.secondaryText
        case .link:    return theme.accent
        case .literal: return theme.accent
        }
    }

    /// Produce coloured token spans for markdown. Order matters: fenced
    /// code bodies are painted first so later inline markers inside
    /// them stay consistent ("everything inside ``` is literal"), and
    /// block markers are painted last so a heading line overrides any
    /// inline-bold attempt within it
    static func markdownSpans(source: String) -> [TokenSpan] {
        var spans: [TokenSpan] = []
        appendFencedCodeSpans(source: source, into: &spans)
        appendInlineMarkerSpans(source: source, into: &spans)
        appendBlockMarkerSpans(source: source, into: &spans)
        return spans
    }

    /// `` `inline` `` spans, `bold`, `*italic*`, `[text](url)`.
    /// Emit marker positions in accent role; body text keeps
    /// primary so it reads naturally
    private static func appendInlineMarkerSpans(
        source: String,
        into spans: inout [TokenSpan]
    ) {
        let chars = Array(source)
        var i = 0
        while i < chars.count {
            let c = chars[i]
            if c == "`" && !isTripleBacktick(chars, at: i) {
                let start = i
                i += 1
                while i < chars.count, chars[i] != "`", chars[i] != "\n" { i += 1 }
                if i < chars.count, chars[i] == "`" {
                    i += 1
                    spans.append(TokenSpan(start: start, end: i, role: .marker))
                }
                continue
            }
            if (c == "*" || c == "_") && i + 1 < chars.count && chars[i + 1] == c {
                if let close = findMatching(chars: chars, from: i + 2, pair: [c, c]) {
                    spans.append(TokenSpan(start: i, end: i + 2, role: .marker))
                    spans.append(TokenSpan(start: close, end: close + 2, role: .marker))
                    i = close + 2
                    continue
                }
            }
            if c == "*" || c == "_" {
                let nextOk = i + 1 < chars.count && chars[i + 1] != c && !chars[i + 1].isWhitespace
                if nextOk, let close = findSingle(chars: chars, from: i + 1, marker: c) {
                    spans.append(TokenSpan(start: i, end: i + 1, role: .marker))
                    spans.append(TokenSpan(start: close, end: close + 1, role: .marker))
                    i = close + 1
                    continue
                }
            }
            if c == "[" {
                if let link = findLink(chars: chars, from: i) {
                    spans.append(TokenSpan(start: link.urlStart, end: link.urlEnd, role: .link))
                    i = link.urlEnd
                    continue
                }
            }
            i += 1
        }
    }

    private static func appendBlockMarkerSpans(
        source: String,
        into spans: inout [TokenSpan]
    ) {
        var offset = 0
        for rawLine in source.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = String(rawLine)
            let indentLen = line.prefix(while: { $0 == " " || $0 == "\t" }).count
            let content = line.dropFirst(indentLen)

            if MarkdownParser.headingParts(line) != nil {
                spans.append(TokenSpan(start: offset, end: offset + line.count, role: .key))
            } else if content.hasPrefix("> ") {
                spans.append(TokenSpan(start: offset + indentLen,
                                       end: offset + indentLen + 2,
                                       role: .comment))
            } else if MarkdownParser.isBullet(line) {
                spans.append(TokenSpan(start: offset + indentLen,
                                       end: offset + indentLen + 2,
                                       role: .marker))
            } else if MarkdownParser.isNumbered(line) {
                var end = indentLen
                while end < line.count {
                    let idx = line.index(line.startIndex, offsetBy: end)
                    if line[idx] == "." { end += 2; break }
                    end += 1
                }
                spans.append(TokenSpan(start: offset + indentLen,
                                       end: offset + min(end, line.count),
                                       role: .marker))
            }
            offset += line.count + 1
        }
    }

    private static func appendFencedCodeSpans(
        source: String,
        into spans: inout [TokenSpan]
    ) {
        var offset = 0
        var inFence = false
        var fenceStart = 0
        for rawLine in source.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = String(rawLine)
            if line.trimmingCharacters(in: .whitespaces).hasPrefix("```") {
                if !inFence {
                    fenceStart = offset
                    inFence = true
                } else {
                    let end = offset + line.count
                    spans.append(TokenSpan(start: fenceStart, end: end, role: .comment))
                    inFence = false
                }
            }
            offset += line.count + 1
        }
        if inFence {
            spans.append(TokenSpan(start: fenceStart,
                                   end: source.utf16.count,
                                   role: .comment))
        }
    }

    private static func isTripleBacktick(_ chars: [Character], at i: Int) -> Bool {
        i + 2 < chars.count && chars[i + 1] == "`" && chars[i + 2] == "`"
    }

    private static func findSingle(
        chars: [Character],
        from: Int,
        marker: Character
    ) -> Int? {
        var i = from
        while i < chars.count {
            if chars[i] == "\n" { return nil }
            if chars[i] == marker {
                let prevOk = i > 0 && !chars[i - 1].isWhitespace
                return prevOk ? i : nil
            }
            i += 1
        }
        return nil
    }

    private static func findMatching(
        chars: [Character],
        from: Int,
        pair: [Character]
    ) -> Int? {
        var i = from
        while i + 1 < chars.count {
            if chars[i] == pair[0] && chars[i + 1] == pair[1] { return i }
            if chars[i] == "\n" { return nil }
            i += 1
        }
        return nil
    }

    private struct LinkSpan {
        let labelStart: Int
        let labelEnd: Int
        let urlStart: Int
        let urlEnd: Int
    }

    private static func findLink(chars: [Character], from: Int) -> LinkSpan? {
        // `[label](url)` - bail on any newline inside
        guard chars[from] == "[" else { return nil }
        let labelStart = from
        var i = from + 1
        while i < chars.count, chars[i] != "]", chars[i] != "\n" { i += 1 }
        guard i < chars.count, chars[i] == "]",
              i + 1 < chars.count, chars[i + 1] == "(" else { return nil }
        let labelEnd = i + 1 // include ]
        let urlStart = i + 1 // include (
        i += 2
        while i < chars.count, chars[i] != ")", chars[i] != "\n" { i += 1 }
        guard i < chars.count, chars[i] == ")" else { return nil }
        let urlEnd = i + 1
        return LinkSpan(
            labelStart: labelStart,
            labelEnd: labelEnd,
            urlStart: urlStart,
            urlEnd: urlEnd
        )
    }


    private static func tokenizeJson(
        source: String,
        into out: inout AttributedString,
        theme: Theme
    ) {
        let chars = Array(source)
        var i = 0
        while i < chars.count {
            let c = chars[i]
            if c == "\"" {
                // A string literal. Look ahead for closing quote,
                // honouring backslash escapes. Decide colour by
                // what sits after it: a `:` makes it a key
                let start = i
                i += 1
                while i < chars.count {
                    if chars[i] == "\\" && i + 1 < chars.count { i += 2; continue }
                    if chars[i] == "\"" { i += 1; break }
                    i += 1
                }
                let end = i
                var lookahead = end
                while lookahead < chars.count, chars[lookahead].isWhitespace { lookahead += 1 }
                let isKey = lookahead < chars.count && chars[lookahead] == ":"
                paint(source: source, from: start, to: end,
                      color: isKey ? theme.accent : theme.secondaryText,
                      in: &out)
                continue
            }
            if c.isNumber || (c == "-" && i + 1 < chars.count && chars[i + 1].isNumber) {
                let start = i
                i += 1
                while i < chars.count,
                      chars[i].isNumber || chars[i] == "." || chars[i] == "e" ||
                      chars[i] == "E" || chars[i] == "+" || chars[i] == "-"
                { i += 1 }
                paint(source: source, from: start, to: i, color: theme.primaryText, in: &out)
                continue
            }
            if c.isLetter {
                let start = i
                while i < chars.count, chars[i].isLetter { i += 1 }
                let word = String(chars[start..<i])
                if ["true", "false", "null"].contains(word) {
                    paint(source: source, from: start, to: i, color: theme.accent, in: &out)
                }
                continue
            }
            i += 1
        }
    }


    private static func tokenizeYaml(
        source: String,
        into out: inout AttributedString,
        theme: Theme
    ) {
        var offset = 0
        for rawLine in source.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = String(rawLine)
            if let hashIdx = line.firstIndex(of: "#") {
                let start = offset + line.distance(from: line.startIndex, to: hashIdx)
                let end = offset + line.count
                paint(source: source, from: start, to: end,
                      color: theme.tertiaryText, in: &out)
            }
            if let colonIdx = line.firstIndex(of: ":") {
                let keyStartInLine = line.prefix(while: { $0.isWhitespace || $0 == "-" }).count
                let keyStart = offset + keyStartInLine
                let keyEnd = offset + line.distance(from: line.startIndex, to: colonIdx)
                if keyEnd > keyStart {
                    paint(source: source, from: keyStart, to: keyEnd,
                          color: theme.accent, in: &out)
                }
            }
            offset += line.count + 1 // +1 for newline
        }
    }


    private static func tokenizeToml(
        source: String,
        into out: inout AttributedString,
        theme: Theme
    ) {
        var offset = 0
        for rawLine in source.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = String(rawLine)
            let trimmed = line.drop { $0.isWhitespace }
            if trimmed.hasPrefix("#") {
                paint(source: source, from: offset, to: offset + line.count,
                      color: theme.tertiaryText, in: &out)
            } else if trimmed.hasPrefix("[") {
                paint(source: source, from: offset, to: offset + line.count,
                      color: theme.accent, in: &out)
            } else if let eqIdx = line.firstIndex(of: "=") {
                let keyEnd = offset + line.distance(from: line.startIndex, to: eqIdx)
                let keyStart = offset + line.prefix(while: { $0.isWhitespace }).count
                if keyEnd > keyStart {
                    paint(source: source, from: keyStart, to: keyEnd,
                          color: theme.accent, in: &out)
                }
            }
            offset += line.count + 1
        }
    }

    /// Apply `color` to code-unit range `[start, end)` on `out`.
    /// Uses UTF-16 indexing to stay consistent with AttributedString's
    /// internal representation on ASCII input (our tokenizers only
    /// produce ASCII boundaries, so this is exact; on multibyte
    /// content worst case is a slightly off-by-one colour, never a
    /// crash)
    private static func paint(
        source: String,
        from start: Int,
        to end: Int,
        color: Color,
        in out: inout AttributedString
    ) {
        guard start < end else { return }
        let utf16 = source.utf16
        let lo = utf16.index(utf16.startIndex, offsetBy: min(start, utf16.count))
        let hi = utf16.index(utf16.startIndex, offsetBy: min(end, utf16.count))
        guard
            let s = AttributedString.Index(lo, within: out),
            let e = AttributedString.Index(hi, within: out)
        else { return }
        out[s..<e].foregroundColor = color
    }
}


private struct MarkdownView: View {
    let text: String
    let theme: Theme

    var body: some View {
        // Lazy stack: multi-paragraph answers can be long, avoid laying
        // out everything at once
        LazyVStack(alignment: .leading, spacing: 10) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                render(block)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var blocks: [MarkdownBlock] { MarkdownParser.parse(text) }

    @ViewBuilder
    private func render(_ block: MarkdownBlock) -> some View {
        switch block {
        case .heading(let level, let body):
            Text(inline(body))
                .font(.system(size: headingSize(level), weight: .semibold))
                .foregroundStyle(theme.primaryText)
        case .paragraph(let body):
            Text(inline(body))
                .font(.system(size: 13))
                .foregroundStyle(theme.primaryText)
                .textSelection(.enabled)
        case .bullet(let items):
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Text("•")
                            .foregroundStyle(theme.accent)
                        Text(inline(item))
                            .font(.system(size: 13))
                            .foregroundStyle(theme.primaryText)
                            .textSelection(.enabled)
                    }
                }
            }
        case .numbered(let items):
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(items.enumerated()), id: \.offset) { i, item in
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Text("\(i + 1).")
                            .foregroundStyle(theme.accent)
                            .font(.system(size: 13, weight: .medium))
                        Text(inline(item))
                            .font(.system(size: 13))
                            .foregroundStyle(theme.primaryText)
                            .textSelection(.enabled)
                    }
                }
            }
        case .code(let lang, let body):
            codeBlock(body, language: lang)
        }
    }

    /// Inline markdown (bold, italic, `code`, links) via built-in
    /// AttributedString parser. Falls back to raw string on parse
    /// errors - never swallows content
    private func inline(_ s: String) -> AttributedString {
        if let md = try? AttributedString(markdown: s, options: .init(
            allowsExtendedAttributes: false,
            interpretedSyntax: .inlineOnlyPreservingWhitespace
        )) {
            return md
        }
        return AttributedString(s)
    }

    private func headingSize(_ level: Int) -> CGFloat {
        switch level {
        case 1: return 20
        case 2: return 17
        case 3: return 15
        default: return 14
        }
    }

    /// Code block with syntax highlighting when fence specified a
    /// known language. Background tint comes from theme's border
    /// colour at a low alpha so we dont need a dedicated palette
    /// entry
    @ViewBuilder
    private func codeBlock(_ body: String, language: String?) -> some View {
        let attributed: AttributedString = {
            guard let lang = language.flatMap(CodeLanguage.init) else {
                var plain = AttributedString(body)
                plain.foregroundColor = theme.primaryText
                return plain
            }
            return SyntaxHighlighter.attributed(for: body, language: lang, theme: theme)
        }()
        Text(attributed)
            .font(.system(size: 12, weight: .regular, design: .monospaced))
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(theme.border.opacity(0.35))
            )
    }
}


enum MarkdownBlock {
    case heading(level: Int, body: String)
    case paragraph(String)
    case bullet([String])
    case numbered([String])
    case code(language: String?, body: String)
}

/// Block-level splitter - picks out headings, fenced code, bullet
/// / numbered lists, and merges everything else into paragraphs.
/// Inline formatting within each block is delegated to Foundation's
/// `AttributedString(markdown:)`, so we dont reinvent parser
enum MarkdownParser {
    static func parse(_ text: String) -> [MarkdownBlock] {
        var blocks: [MarkdownBlock] = []
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        var i = 0
        while i < lines.count {
            let line = lines[i]
            // Fenced code block: ``` or ```lang
            if let lang = fenceLanguage(line) {
                var body: [String] = []
                i += 1
                while i < lines.count, !isFence(lines[i]) {
                    body.append(lines[i])
                    i += 1
                }
                if i < lines.count { i += 1 } // skip closing fence
                blocks.append(.code(
                    language: lang.isEmpty ? nil : lang,
                    body: body.joined(separator: "\n")
                ))
                continue
            }
            // ATX heading: # foo / ## foo / ... up to 6 levels
            if let (level, body) = headingParts(line) {
                blocks.append(.heading(level: level, body: body))
                i += 1
                continue
            }
            // Bullet list: collect consecutive `- ` / `* ` lines
            if isBullet(line) {
                var items: [String] = []
                while i < lines.count, isBullet(lines[i]) {
                    items.append(stripBullet(lines[i]))
                    i += 1
                }
                blocks.append(.bullet(items))
                continue
            }
            // Numbered list: `1. ...`, `2. ...`, ..
            if isNumbered(line) {
                var items: [String] = []
                while i < lines.count, isNumbered(lines[i]) {
                    items.append(stripNumbered(lines[i]))
                    i += 1
                }
                blocks.append(.numbered(items))
                continue
            }
            // Blank line ends any running paragraph
            if line.trimmingCharacters(in: .whitespaces).isEmpty {
                i += 1
                continue
            }
            // Paragraph: merge consecutive non-structural lines
            var para = [line]
            i += 1
            while i < lines.count,
                  !lines[i].trimmingCharacters(in: .whitespaces).isEmpty,
                  headingParts(lines[i]) == nil,
                  !isBullet(lines[i]),
                  !isNumbered(lines[i]),
                  fenceLanguage(lines[i]) == nil
            {
                para.append(lines[i])
                i += 1
            }
            blocks.append(.paragraph(para.joined(separator: "\n")))
        }
        return blocks
    }

    private static func fenceLanguage(_ line: String) -> String? {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        guard trimmed.hasPrefix("```") else { return nil }
        return String(trimmed.dropFirst(3)).trimmingCharacters(in: .whitespaces)
    }

    private static func isFence(_ line: String) -> Bool {
        line.trimmingCharacters(in: .whitespaces).hasPrefix("```")
    }

    static func headingParts(_ line: String) -> (Int, String)? {
        let trimmed = line.drop { $0 == " " || $0 == "\t" }
        var level = 0
        var idx = trimmed.startIndex
        while idx < trimmed.endIndex, trimmed[idx] == "#", level < 6 {
            level += 1
            idx = trimmed.index(after: idx)
        }
        guard level > 0, idx < trimmed.endIndex, trimmed[idx] == " " else { return nil }
        let body = String(trimmed[trimmed.index(after: idx)...])
        return (level, body)
    }

    static func isBullet(_ line: String) -> Bool {
        let trimmed = line.drop { $0 == " " || $0 == "\t" }
        return trimmed.hasPrefix("- ") || trimmed.hasPrefix("* ") || trimmed.hasPrefix("+ ")
    }

    private static func stripBullet(_ line: String) -> String {
        var s = String(line.drop { $0 == " " || $0 == "\t" })
        for prefix in ["- ", "* ", "+ "] {
            if s.hasPrefix(prefix) {
                s.removeFirst(prefix.count)
                break
            }
        }
        return s
    }

    static func isNumbered(_ line: String) -> Bool {
        let trimmed = line.drop { $0 == " " || $0 == "\t" }
        var chars = trimmed.makeIterator()
        var seenDigit = false
        while let c = chars.next() {
            if c.isNumber { seenDigit = true; continue }
            if c == "." { break }
            return false
        }
        guard seenDigit else { return false }
        guard let space = chars.next() else { return false }
        return space == " "
    }

    private static func stripNumbered(_ line: String) -> String {
        var s = String(line.drop { $0 == " " || $0 == "\t" })
        while let c = s.first, c.isNumber { s.removeFirst() }
        if s.first == "." { s.removeFirst() }
        if s.first == " " { s.removeFirst() }
        return s
    }
}
