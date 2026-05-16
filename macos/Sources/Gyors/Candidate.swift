import Foundation

struct CandidateAction: Codable, Hashable {
    let id: String
    let label: String
}

struct Candidate: Identifiable, Hashable, Codable {
    let id: String
    let title: String
    let subtitle: String
    let iconKind: UInt8
    let iconValue: String
    let kind: UInt8
    let score: Int64
    let actions: [CandidateAction]

    enum CodingKeys: String, CodingKey {
        case id, title, subtitle, kind, score, actions
        case iconKind = "icon_kind"
        case iconValue = "icon_value"
    }
}
