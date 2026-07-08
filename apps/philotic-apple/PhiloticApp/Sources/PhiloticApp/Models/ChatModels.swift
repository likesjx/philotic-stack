// ChatModels.swift
// Local chat/conversation data model, persisted independently of the server
// (the server-side turn-history REST surface is still new — see
// HistoryHydrator). Codable so it round-trips through the local JSON store.

import Foundation

/// A mesh agent this app can chat with, addressed by hotel node + agent id.
///
/// TODO(v1): replace the hardcoded catalog in `AgentTarget.builtIn` with a
/// live `GET /api/mesh/targets/:node/agents` fetch once the edge bearer
/// token is accepted on that route. Tracked as a follow-up seam.
public struct AgentTarget: Codable, Equatable, Hashable, Identifiable, Sendable {
    public var targetNodeId: String
    public var targetAgentId: String
    public var displayName: String

    public var id: String { "\(targetNodeId)/\(targetAgentId)" }

    public init(targetNodeId: String, targetAgentId: String, displayName: String) {
        self.targetNodeId = targetNodeId
        self.targetAgentId = targetAgentId
        self.displayName = displayName
    }

    /// Hardcoded v0 catalog, drawn from the known mesh topology. TODO: fetch
    /// this live per `id`.
    public static let builtIn: [AgentTarget] = [
        AgentTarget(targetNodeId: "mbp-jane", targetAgentId: "jane", displayName: "Jane"),
        AgentTarget(targetNodeId: "mbp-jane", targetAgentId: "astrid", displayName: "Astrid"),
        AgentTarget(targetNodeId: "mac-jane", targetAgentId: "bjork", displayName: "Bjork"),
        AgentTarget(targetNodeId: "vps-jane", targetAgentId: "beacon", displayName: "Beacon"),
    ]
}

/// Who authored a chat message, for bubble alignment/styling.
public enum ChatRole: String, Codable, Equatable, Sendable {
    case operatorUser = "operator"
    case agent
    case system
}

/// A turn-in-progress or completed chat message rendered in `ChatView`.
public struct ChatMessage: Codable, Equatable, Identifiable, Sendable {
    public var id: String
    public var role: ChatRole
    public var content: String
    public var createdAt: Date
    /// True while tokens are still streaming in for this message.
    public var isStreaming: Bool
    /// True if this message represents a failed/errored turn.
    public var isError: Bool

    public init(
        id: String = UUID().uuidString,
        role: ChatRole,
        content: String,
        createdAt: Date = Date(),
        isStreaming: Bool = false,
        isError: Bool = false
    ) {
        self.id = id
        self.role = role
        self.content = content
        self.createdAt = createdAt
        self.isStreaming = isStreaming
        self.isError = isError
    }
}

/// A locally persisted conversation with one agent target.
public struct Conversation: Codable, Equatable, Identifiable, Sendable {
    public var id: String
    public var agentTarget: AgentTarget
    public var conversationId: String
    public var messages: [ChatMessage]
    public var updatedAt: Date

    public init(
        id: String = UUID().uuidString,
        agentTarget: AgentTarget,
        conversationId: String,
        messages: [ChatMessage] = [],
        updatedAt: Date = Date()
    ) {
        self.id = id
        self.agentTarget = agentTarget
        self.conversationId = conversationId
        self.messages = messages
        self.updatedAt = updatedAt
    }
}
