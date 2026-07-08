// HistoryHydrator.swift
// Best-effort hydration of a conversation's history from the server. There
// is no `GET .../sessions/:id/turns` REST route on philotic-web today (only
// the IPC-level `ListSessionTurns` exists, and `GET /api/sessions` is a
// stub returning `[]` — see crates/philotic-web/src/serve.rs), so this is
// deliberately a single speculative request that degrades to "unreachable"
// on any failure. The local `ConversationStore` remains the source of
// truth; this only ever supplements it.

import Foundation

/// Mirrors the shape `philotic-web` would plausibly serve for session turns
/// (see `SessionTurnView` in `philotic-client`), decoded defensively.
private struct RemoteSessionTurn: Decodable {
    let turnId: String
    let role: String
    let content: String
    let createdAt: UInt64?
    let status: String

    enum CodingKeys: String, CodingKey {
        case turnId = "turn_id"
        case role
        case content
        case createdAt = "created_at"
        case status
    }
}

public enum HistoryHydrator {
    /// Attempts to fetch remote turn history for `sessionId` from `baseURL`.
    /// Returns `nil` (never throws) if the server doesn't have this route,
    /// the request fails, or the response can't be decoded — callers should
    /// treat `nil` as "stay local-only".
    public static func hydrate(
        sessionId: String,
        baseURL: URL,
        bearerToken: String,
        session: URLSession = .shared
    ) async -> [ChatMessage]? {
        var request = URLRequest(url: baseURL.appendingPathComponent("api/sessions/\(sessionId)/turns"))
        request.setValue("Bearer \(bearerToken)", forHTTPHeaderField: "Authorization")
        request.timeoutInterval = 5

        guard let (data, response) = try? await session.data(for: request),
            let http = response as? HTTPURLResponse,
            (200..<300).contains(http.statusCode)
        else {
            return nil
        }

        guard let turns = try? JSONDecoder().decode([RemoteSessionTurn].self, from: data) else {
            return nil
        }

        return turns.map { turn in
            ChatMessage(
                // The server contract (SessionTurnView / expand_session_turn_views
                // in aiua) deliberately emits TWO items per completed turn —
                // role "operator" and role "agent" — sharing one turn_id, so
                // the role must be folded into the Identifiable id or SwiftUI
                // ForEach diffs over duplicate ids.
                id: "\(turn.turnId)#\(turn.role)",
                role: turn.role == "operator" ? .operatorUser : .agent,
                content: turn.content,
                createdAt: turn.createdAt.map { Date(timeIntervalSince1970: TimeInterval($0)) } ?? Date(),
                isStreaming: false,
                isError: turn.status == "failed"
            )
        }
    }
}
