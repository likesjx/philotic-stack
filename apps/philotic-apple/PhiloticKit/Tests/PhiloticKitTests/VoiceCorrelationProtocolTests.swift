import Foundation
import XCTest
@testable import PhiloticKit

final class VoiceCorrelationProtocolTests: XCTestCase {
    func testCancellationNegotiationAndRequestBeforeAcceptance() throws {
        let legacy = Data(#"{"type":"hello_ack","session_id":"socket"}"#.utf8)
        XCTAssertEqual(try JSONDecoder().decode(EdgeMessage.self, from: legacy),
            .helloAck(sessionId: "socket", replayFrom: nil))
        let supported = EdgeMessage.helloAck(sessionId: "socket", replayFrom: nil, features: ["turn_cancel_v1"])
        XCTAssertEqual(try JSONDecoder().decode(EdgeMessage.self, from: JSONEncoder().encode(supported)), supported)
        for turnID in [nil, "turn-1"] as [String?] {
            let cancel = EdgeMessage.turnCancel(targetNodeId: "hotel", targetAgentId: "agent",
                conversationId: "chat", requestId: "request-1", turnId: turnID)
            let encoded = try JSONEncoder().encode(cancel)
            XCTAssertEqual(try JSONDecoder().decode(EdgeMessage.self, from: encoded), cancel)
            XCTAssertFalse(EdgeClient.advancesResumeCursor(cancel))
            let object = try XCTUnwrap(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
            XCTAssertEqual(object["request_id"] as? String, "request-1")
            XCTAssertEqual(object["turn_id"] as? String, turnID)
        }
    }
    func testSubmissionEncodesRequestIdentityWithoutVoiceModality() throws {
        let message = EdgeMessage.turnSubmit(targetNodeId: "hotel", targetAgentId: "agent",
            conversationId: "chat", content: "local transcript", blobRefs: [], requestId: "request-1")
        let data = try JSONEncoder().encode(message)
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(object["request_id"] as? String, "request-1")
        XCTAssertNil(object["message_kind"])
        XCTAssertNil(object["private"])
        XCTAssertNil(object["external_allowed"])
        XCTAssertEqual(try JSONDecoder().decode(EdgeMessage.self, from: data), message)
    }

    func testAcceptedIdentityAndLegacyDecode() throws {
        let accepted = Data(#"{"type":"turn_event","conversation_id":"chat","event_kind":"status","content":"accepted","turn_id":"turn-1","request_id":"request-1"}"#.utf8)
        XCTAssertEqual(try JSONDecoder().decode(EdgeMessage.self, from: accepted),
            .turnEvent(conversationId: "chat", eventKind: .status, content: "accepted", turnId: "turn-1", requestId: "request-1"))
        let legacy = Data(#"{"type":"turn_event","conversation_id":"chat","event_kind":"status","content":"accepted","turn_id":"turn-1"}"#.utf8)
        XCTAssertEqual(try JSONDecoder().decode(EdgeMessage.self, from: legacy),
            .turnEvent(conversationId: "chat", eventKind: .status, content: "accepted", turnId: "turn-1"))
    }
}
