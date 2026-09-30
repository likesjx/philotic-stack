import Foundation

/// Only operator-editable text fields. Missing originals are encoded as null,
/// not omitted: the server compares them before changing canonical state.
public struct LifeNodeEdit: Encodable, Equatable, Sendable {
    public let before: [String: LifeJSONValue]
    public let changes: [String: String]
    public static let fields = ["title", "claim_summary", "description"]

    public init(node: LifeGraphNode, values: [String: String]) {
        var before: [String: LifeJSONValue] = [:]
        var changes: [String: String] = [:]
        for key in Self.fields {
            guard let value = values[key] else { continue }
            let original = node.properties[key] ?? .null
            // Never stringify/overwrite unexpected structured values.
            switch original {
            case .string(let text):
                guard value != text else { continue }
            case .null:
                guard !value.isEmpty else { continue }
            default: continue
            }
            before[key] = original
            changes[key] = value
        }
        self.before = before
        self.changes = changes
    }
}

public struct LifeNodeEditReceipt: Decodable, Sendable {
    public let status: String
    public let nodeId: String
    public let auditId: String
    enum CodingKeys: String, CodingKey {
        case status
        case nodeId = "node_id"
        case auditId = "audit_id"
    }
}
