import XCTest
@testable import PhiloticKit

final class LifeLoopActionTests: XCTestCase {
    override func tearDown() { StubURLProtocol.responder = nil; super.tearDown() }
    private func node(_ extra: [String: Any] = [:], label: String = "OpenLoop") throws -> LifeGraphNode {
        var properties: [String: Any] = ["id": "life:loop:test", "title": "Review", "validation_state": "proposed"]
        properties.merge(extra) { _, new in new }
        return try JSONDecoder().decode(LifeGraphNode.self, from: JSONSerialization.data(withJSONObject: ["labels": [label], "properties": properties]))
    }
    func testActionsKeepConfirmationAndResolutionSeparate() throws {
        XCTAssertEqual(LifeLoopAction.availableActions(for: try node()), [.confirm, .close])
        XCTAssertEqual(LifeLoopAction.availableActions(for: try node(["validation_state": "confirmed"])), [.close])
        XCTAssertEqual(LifeLoopAction.availableActions(for: try node(["validation_state": "conflicted"])), [.close])
        XCTAssertEqual(LifeLoopAction.availableActions(for: try node(["status": "open", "loop_status": "resolved"])), [.reopen])
        for properties: [String: Any] in [["status": "retired"], ["validation_state": "retired"], ["status": "unknown"], ["loop_action_revision": -1], ["description": ["nested": true]]] {
            XCTAssertTrue(LifeLoopAction.availableActions(for: try node(properties)).isEmpty)
        }
        XCTAssertTrue(LifeLoopAction.availableActions(for: try node(label: "Goal")).isEmpty)
    }
    func testCompleteSnapshotAndStableRetryEncoding() throws {
        let action = try XCTUnwrap(LifeLoopAction(node: try node(["loop_action_revision": 4]), action: .close))
        XCTAssertEqual(action.before.count, 10)
        let bytes = try JSONEncoder().encode(action)
        let body = try JSONSerialization.jsonObject(with: bytes) as! [String: Any]
        XCTAssertTrue((body["before"] as! [String: Any])["resolved_at"] is NSNull)
        XCTAssertEqual((body["before"] as! [String: Any])["loop_action_revision"] as? Int, 4)
        XCTAssertNil(body["actor"])
        XCTAssertEqual(body["request_id"] as? String, action.requestId)
    }
    func testClientRequiresReceiptBoundToCommandAndNode() async throws {
        let command = try XCTUnwrap(LifeLoopAction(node: try node(), action: .close))
        for mismatch in ["none", "node", "request", "action", "audit"] {
            StubURLProtocol.responder = { request in
                XCTAssertEqual(request.httpMethod, "POST")
                XCTAssertEqual(request.url?.path, "/api/edge/lifegraph/node/life:loop:test/action")
                XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer device")
                let body: [String: Any] = ["status": "saved", "node_id": mismatch == "node" ? "other" : "life:loop:test",
                    "request_id": mismatch == "request" ? "other" : command.requestId,
                    "action": mismatch == "action" ? "confirm" : "close", "audit_id": mismatch == "audit" ? "" : "audit:test", "replayed": true]
                return .init(body: try! JSONSerialization.data(withJSONObject: body))
            }
            do {
                let receipt = try await LifeGraphClient(session: .stubbed()).actOnLoop(baseURL: URL(string: "https://hotel.example")!, bearerToken: "device", nodeId: "life:loop:test", action: command)
                XCTAssertEqual(mismatch, "none"); XCTAssertTrue(receipt.replayed)
            } catch { XCTAssertNotEqual(mismatch, "none") }
        }
    }
    func testStaleAndOldHotelErrorsDoNotReportSuccess() async throws {
        for status in [409, 405, 403, 502] {
            StubURLProtocol.responder = { _ in .init(statusCode: status, body: Data("{}".utf8)) }
            do {
                _ = try await LifeGraphClient(session: .stubbed()).actOnLoop(baseURL: URL(string: "https://hotel.example")!, bearerToken: "device", nodeId: "life:loop:test", action: try XCTUnwrap(LifeLoopAction(node: try node(), action: .close)))
                XCTFail("Must not succeed")
            } catch { XCTAssertTrue(error is LifeGraphClient.LoopActionError) }
        }
    }
}
