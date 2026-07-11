// HistoryHydrator.swift
// Best-effort hydration of a conversation's history from the server via
// `GET /api/edge/sessions/:id/turns` (the edge-sessions-bridge REST route
// backed by the hotel's `ListSessionTurns` IPC). The hotel is the canonical
// history authority; the local `ConversationStore` is a cache, so failures
// here degrade to "stay local-only" rather than erroring the UI.

import Foundation
import PhiloticKit

public enum HistoryHydrator {
    /// Attempts to fetch remote turn history for `sessionId` from `baseURL`.
    /// Returns `nil` (never throws) if the server is unreachable, the bearer
    /// is rejected, or the response can't be decoded — callers should treat
    /// `nil` as "stay local-only".
    public static func hydrate(
        sessionId: String,
        baseURL: URL,
        bearerToken: String,
        session: URLSession? = nil
    ) async -> [ChatMessage]? {
        // Hydration is best-effort background fill: keep the default request
        // timeout short so a dark hotel doesn't stall the conversation open.
        let urlSession = session ?? {
            let config = URLSessionConfiguration.ephemeral
            config.timeoutIntervalForRequest = 5
            return URLSession(configuration: config)
        }()
        let client = SessionDirectoryClient(session: urlSession)
        guard
            let turns = try? await client.fetchTurns(
                baseURL: baseURL,
                bearerToken: bearerToken,
                sessionId: sessionId
            )
        else {
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
