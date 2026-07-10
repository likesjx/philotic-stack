// AgentDirectoryClient.swift
// Fetches the edge-scoped agent directory (`GET /api/edge/agents`): every
// mesh agent with its authority hotel pre-resolved to a registry node id
// suitable for `TurnSubmit.target_node_id`.

import Foundation

/// One agent as reported by the hotel's edge agent directory.
public struct EdgeAgentEntry: Codable, Equatable, Sendable {
    public let targetNodeId: String
    public let agentId: String
    public let displayName: String
    public let authorityHotel: String

    enum CodingKeys: String, CodingKey {
        case targetNodeId = "target_node_id"
        case agentId = "agent_id"
        case displayName = "display_name"
        case authorityHotel = "authority_hotel"
    }

    public init(targetNodeId: String, agentId: String, displayName: String, authorityHotel: String) {
        self.targetNodeId = targetNodeId
        self.agentId = agentId
        self.displayName = displayName
        self.authorityHotel = authorityHotel
    }
}

public struct AgentDirectoryClient: Sendable {
    public enum AgentDirectoryError: Error, Equatable {
        case badResponse(status: Int)
    }

    private let session: URLSession

    public init(session: URLSession = .shared) {
        self.session = session
    }

    /// Fetch the agent directory using the edge bearer token.
    public func fetchAgents(baseURL: URL, bearerToken: String) async throws -> [EdgeAgentEntry] {
        var request = URLRequest(url: baseURL.appending(path: "api/edge/agents"))
        request.httpMethod = "GET"
        request.setValue("Bearer \(bearerToken)", forHTTPHeaderField: "Authorization")
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
            let status = (response as? HTTPURLResponse)?.statusCode ?? -1
            throw AgentDirectoryError.badResponse(status: status)
        }
        struct Envelope: Codable { let agents: [EdgeAgentEntry] }
        return try JSONDecoder().decode(Envelope.self, from: data).agents
    }
}
