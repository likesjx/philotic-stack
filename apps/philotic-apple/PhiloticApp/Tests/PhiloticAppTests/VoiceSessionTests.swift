import Foundation
import PhiloticKit
import XCTest
@testable import PhiloticApp

@MainActor
final class VoiceSessionTests: XCTestCase {
    @MainActor
    private final class Harness {
        var submitted: [EdgeMessage] = []
        var spoken: [String] = []
        var rejectCancellation = false
        let defaultsName = "VoiceSessionTests.\(UUID().uuidString)"
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        var manager: ChatSessionManager!

        init() throws {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            let defaults = UserDefaults(suiteName: defaultsName)!
            manager = ChatSessionManager(
                conversationStore: ConversationStore(fileURL: directory.appendingPathComponent("conversations.json")),
                initialSettings: ConnectionSettings(), startsReconnectTriggers: false,
                voiceDefaults: defaults,
                turnSender: { [weak self] message in
                    self?.submitted.append(message)
                    if case .turnCancel = message, self?.rejectCancellation == true {
                        throw NSError(domain: "synthetic cancellation failure", code: 1)
                    }
                },
                localSpeaker: { [weak self] text, _ in self?.spoken.append(text) },
                localSpeechDelayNanoseconds: 1_000_000)
        }

        func select(_ agent: AgentTarget) async throws {
            manager.currentAgent = agent
            for _ in 0..<100 {
                if manager.currentConversation?.agentTarget == agent { return }
                try await Task.sleep(nanoseconds: 1_000_000)
            }
            XCTFail("Agent selection did not finish")
        }

        func submit(turnID: String, correlated: Bool = true) async throws -> String {
            await manager.sendVoiceMessage(text: "synthetic local transcript")
            let lastSubmit = submitted.last { if case .turnSubmit = $0 { return true }; return false }
            guard case .turnSubmit(_, _, let conversationID, _, _, let modality, let requestID) = lastSubmit,
                  let conversationID, let requestID else {
                XCTFail("Missing submitted turn identity")
                throw NSError(domain: "VoiceSessionTests", code: 1)
            }
            XCTAssertNil(modality, "Local STT must not trigger remote persona TTS")
            await manager.handleInbound(.turnEvent(conversationId: conversationID, eventKind: .status,
                content: "accepted", turnId: turnID, requestId: correlated ? requestID : nil))
            return conversationID
        }

        func finish(conversationID: String, turnID: String, text: String) async {
            await manager.handleInbound(.turnEvent(conversationId: conversationID,
                eventKind: .final, content: text, turnId: turnID))
        }

        func waitForSpeech() async throws {
            for _ in 0..<100 {
                if !spoken.isEmpty { return }
                try await Task.sleep(nanoseconds: 1_000_000)
            }
            XCTFail("Expected local speech was not dispatched")
        }

        func settle() async throws { try await Task.sleep(nanoseconds: 10_000_000) }

        func close() async {
            await manager.disconnect()
            UserDefaults(suiteName: defaultsName)?.removePersistentDomain(forName: defaultsName)
            try? FileManager.default.removeItem(at: directory)
        }
    }

    func testRepeatedInterruptNewTurnOldFinalRaceSpeaksOnlyNewTurn() async throws {
        let h = try Harness()
        try await h.select(AgentTarget.builtIn[0])
        for index in 0..<3 {
            h.spoken.removeAll()
            let conversation = try await h.submit(turnID: "old-\(index)")
            h.manager.interruptVoiceReply()
            _ = try await h.submit(turnID: "new-\(index)")
            await h.finish(conversationID: conversation, turnID: "old-\(index)", text: "stale audio text")
            await h.finish(conversationID: conversation, turnID: "new-\(index)", text: "fresh \(index)")
            try await h.waitForSpeech()
            XCTAssertEqual(h.spoken, ["fresh \(index)"])
        }
        await h.close()
    }

    func testReconnectDropsRetainedFinalAndOldAcceptance() async throws {
        let h = try Harness()
        try await h.select(AgentTarget.builtIn[0])
        let conversation = try await h.submit(turnID: "before-reconnect")
        await h.manager.handleInbound(.helloAck(sessionId: "fresh-socket", replayFrom: "replay"))
        await h.finish(conversationID: conversation, turnID: "before-reconnect", text: "replayed old speech")
        try await h.settle()
        XCTAssertEqual(h.spoken, [])
        _ = try await h.submit(turnID: "after-reconnect")
        await h.finish(conversationID: conversation, turnID: "after-reconnect", text: "fresh socket speech")
        try await h.waitForSpeech()
        XCTAssertEqual(h.spoken, ["fresh socket speech"])
        await h.close()
    }

    func testAgentSwitchAwayAndBackNeverSpeaksOldTurn() async throws {
        let h = try Harness()
        let first = AgentTarget.builtIn[0]
        try await h.select(first)
        let conversation = try await h.submit(turnID: "old-agent-a")
        try await h.select(AgentTarget.builtIn[1])
        try await h.select(first)
        await h.finish(conversationID: conversation, turnID: "old-agent-a", text: "old recipient")
        try await h.settle()
        XCTAssertEqual(h.spoken, [])
        _ = try await h.submit(turnID: "new-agent-a")
        await h.finish(conversationID: conversation, turnID: "new-agent-a", text: "current recipient")
        try await h.waitForSpeech()
        XCTAssertEqual(h.spoken, ["current recipient"])
        await h.close()
    }

    func testLegacyServerFinalAndUnsolicitedAudioRemainSilent() async throws {
        let h = try Harness()
        try await h.select(AgentTarget.builtIn[0])
        let conversation = try await h.submit(turnID: "legacy", correlated: false)
        XCTAssertTrue(h.manager.voiceStatus?.contains("correlate") == true)
        await h.finish(conversationID: conversation, turnID: "legacy", text: "uncorrelated speech")
        await h.manager.handleInbound(.voiceReply(conversationId: conversation, turnId: "legacy",
            audioBase64: Data("synthetic".utf8).base64EncodedString(), mimeType: "audio/mpeg",
            transcript: nil, chunkSeq: 0, isFinal: true))
        try await h.settle()
        XCTAssertEqual(h.spoken, [])
        XCTAssertFalse(h.manager.voiceController.hasPendingReplyAudio)
        await h.close()
    }

    func testProviderChangeCancelsScheduledSpeech() async throws {
        let h = try Harness()
        try await h.select(AgentTarget.builtIn[0])
        let conversation = try await h.submit(turnID: "provider-old")
        await h.finish(conversationID: conversation, turnID: "provider-old", text: "obsolete profile")
        h.manager.speakingProvider = .appleLocal
        try await h.settle()
        XCTAssertEqual(h.spoken, [])
        await h.close()
    }

    func testNegotiatedCancellationCarriesExactRequestAndTurn() async throws {
        let h = try Harness()
        try await h.select(AgentTarget.builtIn[0])
        await h.manager.handleInbound(.helloAck(sessionId: "socket", replayFrom: nil, features: ["turn_cancel_v1"]))
        let conversation = try await h.submit(turnID: "cancel-this-turn")
        let sentSubmit = h.submitted.last!
        guard case .turnSubmit(let node, let agent, _, _, _, _, let request) = sentSubmit else {
            return XCTFail("Missing submit")
        }
        h.manager.interruptVoiceReply()
        try await h.settle()
        XCTAssertTrue(h.submitted.contains(.turnCancel(targetNodeId: node, targetAgentId: agent,
            conversationId: conversation, requestId: try XCTUnwrap(request), turnId: "cancel-this-turn")))
        await h.finish(conversationID: conversation, turnID: "cancel-this-turn", text: "cancelled final")
        try await h.settle()
        XCTAssertEqual(h.spoken, [])
        await h.close()
    }

    func testLegacyServerNeverReceivesCancellationCommand() async throws {
        let h = try Harness()
        try await h.select(AgentTarget.builtIn[0])
        _ = try await h.submit(turnID: "legacy-cancel")
        h.manager.interruptVoiceReply()
        try await h.settle()
        XCTAssertFalse(h.submitted.contains { if case .turnCancel = $0 { return true }; return false })
        XCTAssertTrue(h.manager.voiceStatus?.contains("does not support") == true)
        await h.close()
    }

    func testInterruptBeforeAcceptanceCancelsRequestAndRejectsLateAcceptance() async throws {
        let h = try Harness()
        try await h.select(AgentTarget.builtIn[0])
        await h.manager.handleInbound(.helloAck(sessionId: "socket", replayFrom: nil, features: ["turn_cancel_v1"]))
        await h.manager.sendVoiceMessage(text: "synthetic pending request")
        guard case .turnSubmit(let node, let agent, let conversation, _, _, _, let request) = h.submitted.last,
              let conversation, let request else { return XCTFail("Missing pending request") }
        h.manager.interruptVoiceReply()
        try await h.settle()
        XCTAssertTrue(h.submitted.contains(.turnCancel(targetNodeId: node, targetAgentId: agent,
            conversationId: conversation, requestId: request, turnId: nil)))
        await h.manager.handleInbound(.turnEvent(conversationId: conversation, eventKind: .status,
            content: "accepted", turnId: "late-bound", requestId: request))
        await h.finish(conversationID: conversation, turnID: "late-bound", text: "cancelled pending request")
        try await h.settle()
        XCTAssertEqual(h.spoken, [])
        await h.close()
    }

    func testCancellationTransportFailureCannotRestoreLocalSpeech() async throws {
        let h = try Harness()
        try await h.select(AgentTarget.builtIn[0])
        await h.manager.handleInbound(.helloAck(sessionId: "socket", replayFrom: nil, features: ["turn_cancel_v1"]))
        let conversation = try await h.submit(turnID: "transport-failure")
        h.rejectCancellation = true
        h.manager.interruptVoiceReply()
        try await h.settle()
        XCTAssertTrue(h.manager.voiceStatus?.contains("could not be sent") == true)
        await h.finish(conversationID: conversation, turnID: "transport-failure", text: "failed cancellation")
        try await h.settle()
        XCTAssertEqual(h.spoken, [])
        await h.close()
    }

    func testOrdinarySilentTextTurnsDoNotCancelReasoningThroughVoiceControls() async throws {
        let h = try Harness()
        try await h.select(AgentTarget.builtIn[0])
        await h.manager.handleInbound(.helloAck(sessionId: "socket", replayFrom: nil, features: ["turn_cancel_v1"]))
        await h.manager.send("ordinary typed turn")
        h.manager.interruptVoiceReply()
        await h.manager.send("ordinary follow-up")
        try await h.settle()
        XCTAssertFalse(h.submitted.contains { if case .turnCancel = $0 { return true }; return false })
        await h.close()
    }
}
