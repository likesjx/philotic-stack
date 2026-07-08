// EdgeProtocolTests.swift
// Round-trip and golden-fixture tests for the edge-protocol Codable mirrors.
// Golden JSON strings are copy-pasted verbatim from
// `crates/philotic-edge-protocol/src/lib.rs` (`golden_json_hello`,
// `golden_json_turn_event`) so a drift in either language's wire form fails
// loudly here.

import XCTest

@testable import PhiloticKit

final class EdgeProtocolTests: XCTestCase {
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    private func caps() -> EdgeCapabilities {
        EdgeCapabilities(
            deviceName: "Jared's iPhone",
            platform: "ios",
            roles: ["ClientNode", "ModelNode"],
            tools: ["os.ios.healthkit.read@1"],
            models: ["stt.whisper.coreml-tiny-en@1"]
        )
    }

    /// Encodes then decodes `envelope`, asserting the result is equal to the
    /// original (mirrors the Rust `round_trip` helper).
    private func roundTrip(_ envelope: EdgeEnvelope) throws {
        let data = try encoder.encode(envelope)
        let back = try decoder.decode(EdgeEnvelope.self, from: data)
        XCTAssertEqual(envelope, back, "round trip mismatch")
    }

    // MARK: - Round trips (one per EdgeMessage variant)

    func testRoundTripHello() throws {
        try roundTrip(
            EdgeEnvelope(
                seq: 7,
                ack: 3,
                msg: .hello(EdgeHello(nodeId: "edge-abc123", capabilities: caps(), cursor: "cur-opaque-42"))
            ))
        try roundTrip(
            EdgeEnvelope(
                seq: 7,
                ack: 3,
                msg: .hello(EdgeHello(nodeId: "edge-abc123", capabilities: caps(), cursor: nil))
            ))
    }

    func testRoundTripHelloAck() throws {
        try roundTrip(
            EdgeEnvelope(seq: 7, ack: 3, msg: .helloAck(sessionId: "sess-1", replayFrom: "cur-opaque-42")))
        try roundTrip(EdgeEnvelope(seq: 7, ack: 3, msg: .helloAck(sessionId: "sess-1", replayFrom: nil)))
    }

    func testRoundTripTurnSubmit() throws {
        try roundTrip(
            EdgeEnvelope(
                seq: 7,
                ack: 3,
                msg: .turnSubmit(
                    targetNodeId: "mbp-jane",
                    targetAgentId: "jane",
                    conversationId: "conv-9",
                    content: "hello there",
                    blobRefs: [
                        BlobRef(blobId: "blob-1", downloadUrl: "https://example/blob-1", mime: "audio/ogg")
                    ]
                )
            ))
        try roundTrip(
            EdgeEnvelope(
                seq: 7,
                ack: 3,
                msg: .turnSubmit(
                    targetNodeId: "mbp-jane",
                    targetAgentId: "jane",
                    conversationId: nil,
                    content: "hello",
                    blobRefs: []
                )
            ))
    }

    func testRoundTripTurnEvent() throws {
        for kind in [TurnEventKind.token, .final, .status, .error] {
            try roundTrip(
                EdgeEnvelope(
                    seq: 7,
                    ack: 3,
                    msg: .turnEvent(conversationId: "conv-9", eventKind: kind, content: "chunk", turnId: "turn-4")
                ))
        }
    }

    func testRoundTripApprovalRequest() throws {
        try roundTrip(
            EdgeEnvelope(
                seq: 7,
                ack: 3,
                msg: .approvalRequest(approvalId: "appr-1", description: "Run shell command", risk: "high")
            ))
    }

    func testRoundTripApprovalResolve() throws {
        try roundTrip(
            EdgeEnvelope(
                seq: 7,
                ack: 3,
                msg: .approvalResolve(approvalId: "appr-1", approved: true, note: "looks fine")
            ))
    }

    func testRoundTripLifeGraphChange() throws {
        try roundTrip(
            EdgeEnvelope(
                seq: 7,
                ack: 3,
                msg: .lifeGraphChange(
                    changeKind: "created",
                    nodeId: "lg-77",
                    label: "Project",
                    summary: "New project node"
                )
            ))
    }

    func testRoundTripVoiceBlob() throws {
        try roundTrip(
            EdgeEnvelope(
                seq: 7,
                ack: 3,
                msg: .voiceBlob(
                    blobId: "blob-9",
                    downloadUrl: "https://example/blob-9",
                    mime: "audio/ogg",
                    transcript: "hi"
                )
            ))
    }

    func testRoundTripToolInvoke() throws {
        try roundTrip(
            EdgeEnvelope(
                seq: 7,
                ack: 3,
                msg: .toolInvoke(
                    invocationId: "inv-1",
                    toolRef: "os.ios.healthkit.read@1",
                    argsJson: "{\"metric\":\"steps\"}"
                )
            ))
    }

    func testRoundTripToolResult() throws {
        try roundTrip(
            EdgeEnvelope(
                seq: 7,
                ack: 3,
                msg: .toolResult(invocationId: "inv-1", ok: true, resultJson: "{\"steps\":1200}")
            ))
    }

    func testRoundTripCapabilitiesUpdate() throws {
        try roundTrip(EdgeEnvelope(seq: 7, ack: 3, msg: .capabilitiesUpdate(capabilities: caps())))
    }

    func testRoundTripPingPong() throws {
        try roundTrip(EdgeEnvelope(seq: 7, ack: 3, msg: .ping(nonce: 42)))
        try roundTrip(EdgeEnvelope(seq: 7, ack: 3, msg: .pong(nonce: 42)))
    }

    func testRoundTripError() throws {
        try roundTrip(
            EdgeEnvelope(
                seq: 7,
                ack: 3,
                msg: .error(code: "bad_version", message: "unsupported protocol version", fatal: true)
            ))
    }

    func testRoundTripEnrollmentTypes() throws {
        let req = EnrollmentRequest(
            inviteCode: "INV-1234",
            devicePubkeyB64: "cHVia2V5",
            deviceName: "Jared's iPhone",
            platform: "ios"
        )
        let reqData = try encoder.encode(req)
        let reqBack = try decoder.decode(EnrollmentRequest.self, from: reqData)
        XCTAssertEqual(req, reqBack)

        let resp = EnrollmentResponse(nodeId: "edge-abc123", edgeToken: "tok-secret")
        let respData = try encoder.encode(resp)
        let respBack = try decoder.decode(EnrollmentResponse.self, from: respData)
        XCTAssertEqual(resp, respBack)
    }

    // MARK: - Envelope shape

    func testEnvelopeVersionFieldIsVAndAckOmittedWhenNil() throws {
        let envelope = EdgeEnvelope(seq: 1, ack: nil, msg: .ping(nonce: 1))
        let data = try encoder.encode(envelope)
        let object = try JSONSerialization.jsonObject(with: data) as? [String: Any]
        XCTAssertEqual(object?["v"] as? Int, Int(protocolVersion))
        XCTAssertNil(object?["ack"], "ack must be omitted, not encoded as null, when nil")
    }

    func testUnknownTypeTagFailsCleanly() {
        let json = #"{"v":1,"seq":1,"msg":{"type":"warp_drive","nonce":1}}"#
        XCTAssertThrowsError(try decoder.decode(EdgeEnvelope.self, from: Data(json.utf8))) { error in
            XCTAssertTrue(
                String(describing: error).contains("warp_drive"),
                "error should name the unknown tag: \(error)"
            )
        }
    }

    // MARK: - Golden JSON fixtures (copy-pasted from the Rust crate's tests)

    /// Byte-for-byte from `philotic-edge-protocol`'s `golden_json_hello`.
    private let goldenHello =
        #"{"v":1,"seq":1,"msg":{"type":"hello","node_id":"edge-abc123","capabilities":{"device_name":"Jared's iPhone","platform":"ios","roles":["ClientNode","ModelNode"],"tools":["os.ios.healthkit.read@1"],"models":["stt.whisper.coreml-tiny-en@1"]},"cursor":"cur-opaque-42"}}"#

    /// Byte-for-byte from `philotic-edge-protocol`'s `golden_json_turn_event`.
    private let goldenTurnEvent =
        #"{"v":1,"seq":12,"ack":5,"msg":{"type":"turn_event","conversation_id":"conv-9","event_kind":"token","content":"Hel","turn_id":"turn-4"}}"#

    /// Byte-for-byte from `philotic-edge-protocol`'s `golden_json_turn_submit`.
    private let goldenTurnSubmit =
        #"{"v":1,"seq":2,"msg":{"type":"turn_submit","target_node_id":"mbp-jane","target_agent_id":"jane","conversation_id":"conv-9","content":"hello there","blob_refs":[{"blob_id":"blob-1","download_url":"https://example/blob-1","mime":"audio/ogg"}]}}"#

    /// Byte-for-byte from `philotic-edge-protocol`'s
    /// `golden_json_turn_submit_minimal` — `conversation_id` and the empty
    /// `blob_refs` are omitted on the wire, not encoded as null / [].
    private let goldenTurnSubmitMinimal =
        #"{"v":1,"seq":3,"msg":{"type":"turn_submit","target_node_id":"mbp-jane","target_agent_id":"jane","content":"hello"}}"#

    /// Byte-for-byte from `philotic-edge-protocol`'s `golden_json_approval_resolve`.
    private let goldenApprovalResolve =
        #"{"v":1,"seq":4,"ack":9,"msg":{"type":"approval_resolve","approval_id":"appr-1","approved":true,"note":"looks fine"}}"#

    /// Byte-for-byte from `philotic-edge-protocol`'s `golden_json_tool_result`.
    private let goldenToolResult =
        #"{"v":1,"seq":5,"msg":{"type":"tool_result","invocation_id":"inv-1","ok":true,"result_json":"{\"steps\":1200}"}}"#

    /// Byte-for-byte from `philotic-edge-protocol`'s `golden_json_capabilities_update`.
    private let goldenCapabilitiesUpdate =
        #"{"v":1,"seq":6,"msg":{"type":"capabilities_update","capabilities":{"device_name":"Jared's iPhone","platform":"ios","roles":["ClientNode","ModelNode"],"tools":["os.ios.healthkit.read@1"],"models":["stt.whisper.coreml-tiny-en@1"]}}}"#

    func testGoldenJsonHelloDecodesToExpectedValue() throws {
        let envelope = try decoder.decode(EdgeEnvelope.self, from: Data(goldenHello.utf8))
        XCTAssertEqual(
            envelope,
            EdgeEnvelope(
                seq: 1,
                ack: nil,
                msg: .hello(EdgeHello(nodeId: "edge-abc123", capabilities: caps(), cursor: "cur-opaque-42"))
            )
        )
    }

    func testGoldenJsonHelloReencodesEquivalently() throws {
        let envelope = try decoder.decode(EdgeEnvelope.self, from: Data(goldenHello.utf8))
        let reencoded = try encoder.encode(envelope)
        try assertJSONEquivalent(reencoded, Data(goldenHello.utf8))
    }

    func testGoldenJsonTurnEventDecodesToExpectedValue() throws {
        let envelope = try decoder.decode(EdgeEnvelope.self, from: Data(goldenTurnEvent.utf8))
        XCTAssertEqual(
            envelope,
            EdgeEnvelope(
                seq: 12,
                ack: 5,
                msg: .turnEvent(conversationId: "conv-9", eventKind: .token, content: "Hel", turnId: "turn-4")
            )
        )
    }

    func testGoldenJsonTurnEventReencodesEquivalently() throws {
        let envelope = try decoder.decode(EdgeEnvelope.self, from: Data(goldenTurnEvent.utf8))
        let reencoded = try encoder.encode(envelope)
        try assertJSONEquivalent(reencoded, Data(goldenTurnEvent.utf8))
    }

    // Client->server variants: these goldens guard the hand-written Swift
    // `encode(to:)` against key/shape drift the self-round-trip tests cannot
    // catch (a consistent encode+decode rename still round-trips locally but
    // is rejected by the hotel as `bad_frame`).

    func testGoldenJsonTurnSubmitDecodesToExpectedValue() throws {
        let envelope = try decoder.decode(EdgeEnvelope.self, from: Data(goldenTurnSubmit.utf8))
        XCTAssertEqual(
            envelope,
            EdgeEnvelope(
                seq: 2,
                ack: nil,
                msg: .turnSubmit(
                    targetNodeId: "mbp-jane",
                    targetAgentId: "jane",
                    conversationId: "conv-9",
                    content: "hello there",
                    blobRefs: [
                        BlobRef(blobId: "blob-1", downloadUrl: "https://example/blob-1", mime: "audio/ogg")
                    ]
                )
            )
        )
    }

    func testGoldenJsonTurnSubmitReencodesEquivalently() throws {
        let envelope = try decoder.decode(EdgeEnvelope.self, from: Data(goldenTurnSubmit.utf8))
        let reencoded = try encoder.encode(envelope)
        try assertJSONEquivalent(reencoded, Data(goldenTurnSubmit.utf8))
    }

    func testGoldenJsonTurnSubmitMinimalDecodesToExpectedValue() throws {
        let envelope = try decoder.decode(
            EdgeEnvelope.self, from: Data(goldenTurnSubmitMinimal.utf8))
        XCTAssertEqual(
            envelope,
            EdgeEnvelope(
                seq: 3,
                ack: nil,
                msg: .turnSubmit(
                    targetNodeId: "mbp-jane",
                    targetAgentId: "jane",
                    conversationId: nil,
                    content: "hello",
                    blobRefs: []
                )
            )
        )
    }

    /// The minimal fixture is the one that catches "always encode blobRefs /
    /// conversationId" drift: both keys must stay absent on the wire.
    func testGoldenJsonTurnSubmitMinimalReencodesEquivalently() throws {
        let envelope = try decoder.decode(
            EdgeEnvelope.self, from: Data(goldenTurnSubmitMinimal.utf8))
        let reencoded = try encoder.encode(envelope)
        try assertJSONEquivalent(reencoded, Data(goldenTurnSubmitMinimal.utf8))
    }

    func testGoldenJsonApprovalResolveDecodesToExpectedValue() throws {
        let envelope = try decoder.decode(
            EdgeEnvelope.self, from: Data(goldenApprovalResolve.utf8))
        XCTAssertEqual(
            envelope,
            EdgeEnvelope(
                seq: 4,
                ack: 9,
                msg: .approvalResolve(approvalId: "appr-1", approved: true, note: "looks fine")
            )
        )
    }

    func testGoldenJsonApprovalResolveReencodesEquivalently() throws {
        let envelope = try decoder.decode(
            EdgeEnvelope.self, from: Data(goldenApprovalResolve.utf8))
        let reencoded = try encoder.encode(envelope)
        try assertJSONEquivalent(reencoded, Data(goldenApprovalResolve.utf8))
    }

    func testGoldenJsonToolResultDecodesToExpectedValue() throws {
        let envelope = try decoder.decode(EdgeEnvelope.self, from: Data(goldenToolResult.utf8))
        XCTAssertEqual(
            envelope,
            EdgeEnvelope(
                seq: 5,
                ack: nil,
                msg: .toolResult(invocationId: "inv-1", ok: true, resultJson: "{\"steps\":1200}")
            )
        )
    }

    func testGoldenJsonToolResultReencodesEquivalently() throws {
        let envelope = try decoder.decode(EdgeEnvelope.self, from: Data(goldenToolResult.utf8))
        let reencoded = try encoder.encode(envelope)
        try assertJSONEquivalent(reencoded, Data(goldenToolResult.utf8))
    }

    func testGoldenJsonCapabilitiesUpdateDecodesToExpectedValue() throws {
        let envelope = try decoder.decode(
            EdgeEnvelope.self, from: Data(goldenCapabilitiesUpdate.utf8))
        XCTAssertEqual(
            envelope,
            EdgeEnvelope(seq: 6, ack: nil, msg: .capabilitiesUpdate(capabilities: caps()))
        )
    }

    func testGoldenJsonCapabilitiesUpdateReencodesEquivalently() throws {
        let envelope = try decoder.decode(
            EdgeEnvelope.self, from: Data(goldenCapabilitiesUpdate.utf8))
        let reencoded = try encoder.encode(envelope)
        try assertJSONEquivalent(reencoded, Data(goldenCapabilitiesUpdate.utf8))
    }

    /// Key-order-insensitive structural equality via `JSONSerialization`,
    /// since Swift's `JSONEncoder` does not guarantee Rust's field order.
    private func assertJSONEquivalent(
        _ lhs: Data,
        _ rhs: Data,
        file: StaticString = #filePath,
        line: UInt = #line
    ) throws {
        let lhsObject = try JSONSerialization.jsonObject(with: lhs) as? NSDictionary
        let rhsObject = try JSONSerialization.jsonObject(with: rhs) as? NSDictionary
        XCTAssertEqual(lhsObject, rhsObject, file: file, line: line)
    }
}
