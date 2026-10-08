import XCTest
@testable import PhiloticKit

final class VoiceTurnGateTests: XCTestCase {
    private let scope = VoiceTurnGate.Scope(nodeID: "hotel", agentID: "agent-a", conversationID: "chat-a")

    private func accepted(_ gate: inout VoiceTurnGate, turn: String = "turn-a") -> VoiceTurnGate.Ticket {
        let ticket = gate.begin(scope: scope)
        XCTAssertTrue(gate.accept(conversationID: scope.conversationID, requestID: ticket.requestID, turnID: turn))
        return ticket
    }

    func testMissingCorrelationNeverAuthorizesVoice() {
        var gate = VoiceTurnGate()
        let ticket = gate.begin(scope: scope)
        XCTAssertFalse(gate.accept(conversationID: "chat-a", requestID: nil, turnID: "old"))
        XCTAssertFalse(gate.accept(conversationID: "chat-a", requestID: ticket.requestID, turnID: nil))
        XCTAssertFalse(gate.finish(conversationID: "chat-a", turnID: "old"))
        XCTAssertFalse(gate.receiveAudio(conversationID: "chat-a", turnID: nil, chunkSequence: 0, isFinal: false))
    }

    func testOldFinalCannotLiftInterruptSuppression() {
        var gate = VoiceTurnGate()
        let old = accepted(&gate)
        let cancellation = gate.invalidate()
        XCTAssertEqual(cancellation?.turnID, "turn-a")
        XCTAssertEqual(cancellation?.ticket, old)
        let fresh = gate.begin(scope: scope)
        XCTAssertFalse(gate.finish(conversationID: "chat-a", turnID: "turn-a"))
        XCTAssertFalse(gate.accept(conversationID: "chat-a", requestID: old.requestID, turnID: "turn-a"))
        XCTAssertTrue(gate.accept(conversationID: "chat-a", requestID: fresh.requestID, turnID: "turn-b"))
        XCTAssertFalse(gate.receiveAudio(conversationID: "chat-a", turnID: "turn-a", chunkSequence: 0, isFinal: false))
        XCTAssertTrue(gate.receiveAudio(conversationID: "chat-a", turnID: "turn-b", chunkSequence: 0, isFinal: false))
        XCTAssertFalse(gate.finish(conversationID: "chat-a", turnID: "turn-a"))
        XCTAssertTrue(gate.finish(conversationID: "chat-a", turnID: "turn-b"))
    }

    func testRepeatedInterruptBeforeAcceptanceCannotRebindOldRequests() {
        var gate = VoiceTurnGate()
        for index in 0..<5 {
            let old = gate.begin(scope: scope)
            XCTAssertNil(gate.invalidate()?.turnID)
            let new = gate.begin(scope: scope)
            XCTAssertFalse(gate.accept(conversationID: "chat-a", requestID: old.requestID, turnID: "old-\(index)"))
            XCTAssertTrue(gate.accept(conversationID: "chat-a", requestID: new.requestID, turnID: "new-\(index)"))
            gate.invalidate()
        }
    }

    func testAgentSwitchAwayAndBackInvalidatesTimerAndAudio() {
        var gate = VoiceTurnGate()
        let firstA = accepted(&gate)
        _ = gate.begin(scope: .init(nodeID: "hotel", agentID: "agent-b", conversationID: "chat-b"))
        let secondA = gate.begin(scope: scope)
        XCTAssertFalse(gate.isCurrent(firstA))
        XCTAssertTrue(gate.isCurrent(secondA))
        XCTAssertFalse(gate.accept(conversationID: "chat-a", requestID: firstA.requestID, turnID: "turn-a"))
    }

    func testReconnectAndFreshSessionRejectReplay() {
        var gate = VoiceTurnGate()
        let beforeReconnect = accepted(&gate)
        gate.invalidate()
        let afterReconnect = gate.begin(scope: scope)
        XCTAssertFalse(gate.accept(conversationID: "chat-a", requestID: beforeReconnect.requestID, turnID: "turn-a"))
        XCTAssertTrue(gate.accept(conversationID: "chat-a", requestID: afterReconnect.requestID, turnID: "turn-b"))
        _ = gate.begin(scope: .init(nodeID: "hotel", agentID: "agent-a", conversationID: "fresh-chat"))
        XCTAssertFalse(gate.finish(conversationID: "chat-a", turnID: "turn-b"))
    }

    func testDuplicateFinalWholeReplyAndChunkOrdering() {
        var gate = VoiceTurnGate()
        _ = accepted(&gate)
        XCTAssertTrue(gate.finish(conversationID: "chat-a", turnID: "turn-a"))
        XCTAssertFalse(gate.finish(conversationID: "chat-a", turnID: "turn-a"))
        XCTAssertTrue(gate.receiveAudio(conversationID: "chat-a", turnID: "turn-a", chunkSequence: nil, isFinal: nil))
        XCTAssertFalse(gate.receiveAudio(conversationID: "chat-a", turnID: "turn-a", chunkSequence: nil, isFinal: nil))
        _ = accepted(&gate, turn: "chunked")
        XCTAssertTrue(gate.receiveAudio(conversationID: "chat-a", turnID: "chunked", chunkSequence: 0, isFinal: false))
        XCTAssertFalse(gate.receiveAudio(conversationID: "chat-a", turnID: "chunked", chunkSequence: 0, isFinal: false))
        XCTAssertTrue(gate.receiveAudio(conversationID: "chat-a", turnID: "chunked", chunkSequence: 1, isFinal: true))
        XCTAssertFalse(gate.receiveAudio(conversationID: "chat-a", turnID: "chunked", chunkSequence: 2, isFinal: true))
    }

    func testGapAndWrongConversationFailClosed() {
        var gate = VoiceTurnGate()
        _ = accepted(&gate)
        XCTAssertFalse(gate.receiveAudio(conversationID: "chat-b", turnID: "turn-a", chunkSequence: 0, isFinal: false))
        XCTAssertFalse(gate.receiveAudio(conversationID: "chat-a", turnID: "turn-a", chunkSequence: 1, isFinal: false))
        XCTAssertFalse(gate.receiveAudio(conversationID: "chat-a", turnID: "turn-a", chunkSequence: 0, isFinal: false))
    }

    func testPreferenceNeverGrantsExternalEligibility() {
        for preference in VoiceProviderPreference.allCases {
            XCTAssertEqual(preference.availableProvider, .appleLocal)
        }
        XCTAssertNotNil(VoiceProviderPreference.elevenLabs.availabilityExplanation)
        XCTAssertNil(VoiceProviderPreference.appleLocal.availabilityExplanation)
    }
}
