import Foundation

/// Explicit operator lifecycle actions, separate from text edits and Reminders.
public struct LifeLoopAction: Encodable, Equatable, Sendable {
    public enum Action: String, Codable, CaseIterable, Identifiable, Sendable {
        case confirm, close, reopen
        public var id: String { rawValue }
        public var title: String {
            switch self { case .confirm: "Confirm loop"; case .close: "Close loop"; case .reopen: "Reopen loop" }
        }
        public var explanation: String {
            switch self {
            case .confirm: "Mark this loop as verified by you. It remains open."
            case .close: "Mark this loop as resolved. This does not change its confirmation status or complete an Apple Reminder."
            case .reopen: "Return this loop to open. Its confirmation status and source provenance stay unchanged."
            }
        }
    }
    public static let fields = ["title", "claim_summary", "description", "status", "loop_status",
        "validation_state", "last_confirmed_at", "resolved_at", "resolution_note", "loop_action_revision"]
    public let requestId: String
    public let action: Action
    public let before: [String: LifeJSONValue]
    public let note: String
    enum CodingKeys: String, CodingKey { case requestId = "request_id", action, before, note }

    public init?(node: LifeGraphNode, action: Action, note: String = "", requestId: String = UUID().uuidString) {
        guard Self.availableActions(for: node).contains(action), note.utf8.count <= 4096,
              (32...64).contains(requestId.utf8.count),
              requestId.utf8.allSatisfy({ (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || $0 == 45 }) else { return nil }
        self.requestId = requestId
        self.action = action
        self.before = Dictionary(uniqueKeysWithValues: Self.fields.map { ($0, node.properties[$0] ?? .null) })
        self.note = note
    }

    public static func availableActions(for node: LifeGraphNode) -> [Action] {
        guard node.labels.contains("OpenLoop"), let id = node.canonicalId,
              id.hasPrefix("life:"), id.utf8.count > 5, id.utf8.count <= 512 else { return [] }
        for key in fields {
            switch node.properties[key] ?? .null {
            case .null: break
            case .string(let text) where key != "loop_action_revision" && text.utf8.count <= 16384: break
            case .number(let value) where key == "loop_action_revision" && value.isFinite &&
                value >= 0 && value < 9_007_199_254_740_991 && value.rounded() == value: break
            default: return []
            }
        }
        let validation = node.string("validation_state") ?? "inferred"
        guard ["proposed", "inferred", "confirmed", "conflicted"].contains(validation) else { return [] }
        let aliases = [node.string("status"), node.string("loop_status")].compactMap { $0 }
        guard !aliases.contains("retired") else { return [] }
        if aliases.contains(where: { ["resolved", "done", "fulfilled", "abandoned"].contains($0) }) { return [.reopen] }
        let status = node.string("status") ?? node.string("loop_status") ?? "open"
        guard ["open", "active", "pending", "blocked", "in_progress"].contains(status) else { return [] }
        return ["proposed", "inferred"].contains(validation) ? [.confirm, .close] : [.close]
    }
}

public struct LifeLoopActionReceipt: Decodable, Sendable {
    public let status: String
    public let nodeId: String
    public let auditId: String
    public let requestId: String
    public let action: LifeLoopAction.Action
    public let replayed: Bool
    enum CodingKeys: String, CodingKey {
        case status, action, replayed
        case nodeId = "node_id", auditId = "audit_id", requestId = "request_id"
    }
}
