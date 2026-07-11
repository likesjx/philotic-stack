// SessionDirectoryClient.swift
// Fetches the edge-scoped conversation history (`GET /api/edge/sessions` and
// `GET /api/edge/sessions/:id/turns`): the hotel's recorded operator sessions
// served from the context graph. The hotel — not the device's local store —
// is the canonical history authority; local persistence is a cache.

import Foundation

/// One session summary as reported by `GET /api/edge/sessions`
/// (mirrors `OperatorSessionView` in `philotic-client`).
public struct EdgeSessionEntry: Codable, Equatable, Sendable {
    public let sessionId: String
    public let agentId: String?
    public let transport: String?
    public let status: String
    /// Unix epoch seconds of the last recorded activity on the session.
    public let lastActivityAt: UInt64
    public let title: String?
    public let preview: String?

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case agentId = "agent_id"
        case transport
        case status
        case lastActivityAt = "last_activity_at"
        case title
        case preview
    }

    public init(
        sessionId: String,
        agentId: String?,
        transport: String?,
        status: String,
        lastActivityAt: UInt64,
        title: String?,
        preview: String?
    ) {
        self.sessionId = sessionId
        self.agentId = agentId
        self.transport = transport
        self.status = status
        self.lastActivityAt = lastActivityAt
        self.title = title
        self.preview = preview
    }
}

/// One operator/agent message expanded from a stored session turn (mirrors
/// `SessionTurnView`). A completed turn yields TWO entries sharing a
/// `turnId` — role "operator" then role "agent" — so row identity must fold
/// the role in (`"\(turnId)#\(role)"`).
public struct EdgeSessionTurn: Codable, Equatable, Sendable {
    public let turnId: String
    /// "operator" for the inbound user message, "agent" for the reply.
    public let role: String
    public let content: String
    /// Unix epoch seconds; startedAt for operator items, completedAt for agent items.
    public let createdAt: UInt64?
    /// Turn processing status ("queued", "running", "completed", "failed").
    public let status: String

    enum CodingKeys: String, CodingKey {
        case turnId = "turn_id"
        case role
        case content
        case createdAt = "created_at"
        case status
    }

    public init(turnId: String, role: String, content: String, createdAt: UInt64?, status: String) {
        self.turnId = turnId
        self.role = role
        self.content = content
        self.createdAt = createdAt
        self.status = status
    }
}

public struct SessionDirectoryClient: Sendable {
    public enum SessionDirectoryError: Error, Equatable {
        case badResponse(status: Int)
    }

    private let session: URLSession

    public init(session: URLSession = .shared) {
        self.session = session
    }

    /// List the hotel's recorded operator sessions, most recent activity
    /// first. Pass `agentId` to restrict to one philote's conversations.
    public func fetchSessions(
        baseURL: URL,
        bearerToken: String,
        agentId: String? = nil,
        limit: UInt32? = nil
    ) async throws -> [EdgeSessionEntry] {
        var components = URLComponents(
            url: baseURL.appending(path: "api/edge/sessions"),
            resolvingAgainstBaseURL: false
        )
        var query: [URLQueryItem] = []
        if let agentId { query.append(URLQueryItem(name: "agent_id", value: agentId)) }
        if let limit { query.append(URLQueryItem(name: "limit", value: String(limit))) }
        if !query.isEmpty { components?.queryItems = query }
        guard let url = components?.url else {
            throw SessionDirectoryError.badResponse(status: -1)
        }
        struct Envelope: Codable { let sessions: [EdgeSessionEntry] }
        let envelope: Envelope = try await get(url, bearerToken: bearerToken)
        return envelope.sessions
    }

    /// Fetch one session's expanded turns, oldest first. `beforeTurnId` pages
    /// strictly older history; an unknown cursor yields an empty page.
    public func fetchTurns(
        baseURL: URL,
        bearerToken: String,
        sessionId: String,
        limit: UInt32? = nil,
        beforeTurnId: String? = nil
    ) async throws -> [EdgeSessionTurn] {
        var components = URLComponents(
            url: baseURL.appending(path: "api/edge/sessions/\(sessionId)/turns"),
            resolvingAgainstBaseURL: false
        )
        var query: [URLQueryItem] = []
        if let limit { query.append(URLQueryItem(name: "limit", value: String(limit))) }
        if let beforeTurnId {
            query.append(URLQueryItem(name: "before_turn_id", value: beforeTurnId))
        }
        if !query.isEmpty { components?.queryItems = query }
        guard let url = components?.url else {
            throw SessionDirectoryError.badResponse(status: -1)
        }
        struct Envelope: Codable { let turns: [EdgeSessionTurn] }
        let envelope: Envelope = try await get(url, bearerToken: bearerToken)
        return envelope.turns
    }

    private func get<T: Decodable>(_ url: URL, bearerToken: String) async throws -> T {
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.setValue("Bearer \(bearerToken)", forHTTPHeaderField: "Authorization")
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
            let status = (response as? HTTPURLResponse)?.statusCode ?? -1
            throw SessionDirectoryError.badResponse(status: status)
        }
        return try JSONDecoder().decode(T.self, from: data)
    }
}
