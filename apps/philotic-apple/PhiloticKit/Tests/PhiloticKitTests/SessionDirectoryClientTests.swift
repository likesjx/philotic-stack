// SessionDirectoryClientTests.swift
// Exercises SessionDirectoryClient's GET /api/edge/sessions and
// GET /api/edge/sessions/:id/turns against StubURLProtocol — server JSON
// fixtures mirror the philotic-web edge-sessions-bridge responses
// (OperatorSessionView / SessionTurnView shapes, snake_case keys).

import XCTest

@testable import PhiloticKit

final class SessionDirectoryClientTests: XCTestCase {
    override func tearDown() {
        StubURLProtocol.responder = nil
        super.tearDown()
    }

    private let baseURL = URL(string: "https://hotel.example")!

    func testFetchSessionsDecodesServerShapeAndSendsQuery() async throws {
        // Verbatim server shape: wrapped `sessions`, snake_case, optional
        // fields omitted on the second entry (skip_serializing_if on Rust side).
        let body = Data(
            """
            {"sessions":[
              {"session_id":"operator-chat:edge:n1:jane","agent_id":"jane",
               "transport":"operator_chat","status":"active",
               "last_activity_at":1770000000,"title":"edge:n1","preview":"hi"},
              {"session_id":"operator-chat:edge:n1:bjork","agent_id":"bjork",
               "status":"paused","last_activity_at":1769000000}
            ]}
            """.utf8)

        let capturedURL = Locked<URL?>(nil)
        let capturedAuth = Locked<String?>(nil)
        StubURLProtocol.responder = { request in
            capturedURL.value = request.url
            capturedAuth.value = request.value(forHTTPHeaderField: "Authorization")
            return .init(statusCode: 200, body: body)
        }

        let client = SessionDirectoryClient(session: .stubbed())
        let sessions = try await client.fetchSessions(
            baseURL: baseURL,
            bearerToken: "tok-1",
            agentId: "jane",
            limit: 25
        )

        XCTAssertEqual(capturedURL.value?.path, "/api/edge/sessions")
        let query = capturedURL.value?.query ?? ""
        XCTAssertTrue(query.contains("agent_id=jane"), "query was: \(query)")
        XCTAssertTrue(query.contains("limit=25"), "query was: \(query)")
        XCTAssertEqual(capturedAuth.value, "Bearer tok-1")

        XCTAssertEqual(sessions.count, 2)
        XCTAssertEqual(sessions[0].sessionId, "operator-chat:edge:n1:jane")
        XCTAssertEqual(sessions[0].agentId, "jane")
        XCTAssertEqual(sessions[0].lastActivityAt, 1_770_000_000)
        XCTAssertEqual(sessions[0].preview, "hi")
        XCTAssertNil(sessions[1].transport)
        XCTAssertNil(sessions[1].title)
        XCTAssertNil(sessions[1].preview)
    }

    func testFetchTurnsDecodesPairedTurnViews() async throws {
        let body = Data(
            """
            {"session_id":"operator-chat:edge:n1:jane","turns":[
              {"turn_id":"turn-1","role":"operator","content":"hello there",
               "created_at":1770000000,"status":"completed"},
              {"turn_id":"turn-1","role":"agent","content":"Hello from the hotel",
               "created_at":1770000005,"status":"completed"}
            ]}
            """.utf8)

        let capturedURL = Locked<URL?>(nil)
        StubURLProtocol.responder = { request in
            capturedURL.value = request.url
            return .init(statusCode: 200, body: body)
        }

        let client = SessionDirectoryClient(session: .stubbed())
        let turns = try await client.fetchTurns(
            baseURL: baseURL,
            bearerToken: "tok-1",
            sessionId: "operator-chat:edge:n1:jane",
            beforeTurnId: "turn-9"
        )

        XCTAssertEqual(
            capturedURL.value?.path,
            "/api/edge/sessions/operator-chat:edge:n1:jane/turns"
        )
        XCTAssertTrue((capturedURL.value?.query ?? "").contains("before_turn_id=turn-9"))

        XCTAssertEqual(turns.count, 2)
        // One stored turn expands to two views sharing the turn_id.
        XCTAssertEqual(turns[0].turnId, turns[1].turnId)
        XCTAssertEqual(turns[0].role, "operator")
        XCTAssertEqual(turns[1].role, "agent")
        XCTAssertEqual(turns[1].createdAt, 1_770_000_005)
    }

    func testNon200Throws() async {
        StubURLProtocol.responder = { _ in .init(statusCode: 401, body: Data()) }

        let client = SessionDirectoryClient(session: .stubbed())
        do {
            _ = try await client.fetchSessions(baseURL: baseURL, bearerToken: "bad")
            XCTFail("expected fetchSessions to throw")
        } catch let SessionDirectoryClient.SessionDirectoryError.badResponse(status) {
            XCTAssertEqual(status, 401)
        } catch {
            XCTFail("unexpected error: \(error)")
        }
    }
}
