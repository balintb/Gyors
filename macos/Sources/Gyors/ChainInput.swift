import Foundation

/// Parsed view of a flat query string as a chain of stages
///
/// LIVE chain state for input bar lives on ViewModel as
/// `chainCommits: [String]` + `activeBuffer: String`. This struct
/// is a display-side parser kept around for debugging, round-trip
/// tests, and scenarios where code holds a flat query string and
/// wants to understand its shape
///
/// Separator rules match Rust side (`crates/gyors-ipc/src/lib.rs`):
///
/// - ` | ` (space-pipe-space) is ONLY chain separator. A bare `|`
///   without flanking spaces is literal text. This preserves `|`
///   for regex alternation, YAML block scalars, shell pipelines,
///   AI free text, etc.
/// - Chains are committed in UI by explicit cmd| keyboard gesture -
///   typed `|` never produces a pill, typed ` | ` only round-trips
///   if active field happens to render it
struct ChainState: Equatable {
    let commits: [String]
    let active: String

    static let empty = ChainState(commits: [], active: "")

    /// Split a flat query on ` | ` (space-pipe-space) boundaries.
    /// Pure function - no I/O, no side effects
    static func parse(_ raw: String) -> ChainState {
        if raw.isEmpty { return .empty }

        // Single separator: ` | `. A bare `|` stays part of its
        // segment
        let segments = raw.components(separatedBy: " | ")
        guard let last = segments.last else { return .empty }
        let commits = segments.dropLast()
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        // Active: strip leading whitespace (artefacts from joiner
        // or paste), preserve trailing (for mid-word continuation)
        let active = String(last.drop(while: { $0.isWhitespace }))
        return ChainState(commits: Array(commits), active: active)
    }

    /// Canonical flat form: ` | `-joined commits followed by
    /// active. Round-trips through `parse` back to same state
    /// (modulo leading-whitespace normalisation parser applies to
    /// active)
    var flat: String {
        if commits.isEmpty { return active }
        return commits.joined(separator: " | ") + " | " + active
    }
}
