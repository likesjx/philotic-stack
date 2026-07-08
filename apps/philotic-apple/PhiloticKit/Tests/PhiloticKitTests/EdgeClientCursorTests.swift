// EdgeClientCursorTests.swift
// Pins the ack/resume-cursor semantics of `EdgeClient`: only server-push
// (retained, replayable) frames advance the watermark, the watermark is
// monotonic across replayed frames, and control frames — most importantly
// the HelloAck, whose seq is minted AFTER the retained frames about to be
// replayed — never advance it.

import XCTest

@testable import PhiloticKit

final class EdgeClientCursorTests: XCTestCase {
    private func caps() -> EdgeCapabilities {
        EdgeCapabilities(deviceName: "Test", platform: "ios")
    }

    // MARK: - Which kinds advance the cursor

    func testServerPushKindsAdvanceTheResumeCursor() {
        let pushKinds: [EdgeMessage] = [
            .turnEvent(conversationId: "c", eventKind: .token, content: "t", turnId: nil),
            .approvalRequest(approvalId: "a", description: "d", risk: nil),
            .lifeGraphChange(changeKind: "created", nodeId: "n", label: nil, summary: nil),
            .voiceBlob(blobId: "b", downloadUrl: "https://example/b", mime: nil, transcript: nil),
            .toolInvoke(invocationId: "i", toolRef: "tool@1", argsJson: "{}"),
        ]
        for message in pushKinds {
            XCTAssertTrue(
                EdgeClient.advancesResumeCursor(message),
                "expected \(message) to advance the resume cursor"
            )
        }
    }

    func testControlAndClientKindsDoNotAdvanceTheResumeCursor() {
        let nonPushKinds: [EdgeMessage] = [
            .hello(EdgeHello(nodeId: "edge-1", capabilities: caps())),
            .helloAck(sessionId: "sess-1", replayFrom: nil),
            .turnSubmit(
                targetNodeId: "n", targetAgentId: "a", conversationId: nil, content: "hi",
                blobRefs: []),
            .approvalResolve(approvalId: "a", approved: true, note: nil),
            .toolResult(invocationId: "i", ok: true, resultJson: "{}"),
            .capabilitiesUpdate(capabilities: caps()),
            .ping(nonce: 1),
            .pong(nonce: 1),
            .error(code: "bad_frame", message: "boom", fatal: false),
        ]
        for message in nonPushKinds {
            XCTAssertFalse(
                EdgeClient.advancesResumeCursor(message),
                "expected \(message) NOT to advance the resume cursor"
            )
        }
    }

    // MARK: - Watermark arithmetic

    private func turnEvent(_ seq: UInt64) -> (UInt64, EdgeMessage) {
        (seq, .turnEvent(conversationId: "c", eventKind: .token, content: "t\(seq)", turnId: nil))
    }

    /// The HelloAck's seq (minted after every retained frame in the server's
    /// ring) must not seed the watermark: acking it would prune unprocessed
    /// replay frames server-side.
    func testHelloAckDoesNotSeedTheWatermark() {
        let after = EdgeClient.advanceWatermark(
            nil, envelopeSeq: 40, message: .helloAck(sessionId: "s", replayFrom: nil))
        XCTAssertNil(after, "HelloAck must not seed the ack watermark")
    }

    /// Replay scenario from the server's ring: live frame seq 12 was already
    /// processed, then replayed frames 8...10 re-arrive after a reconnect.
    /// The watermark must not regress.
    func testWatermarkIsMonotonicAcrossReplayedFrames() {
        var watermark: UInt64?
        let (liveSeq, liveMsg) = turnEvent(12)
        watermark = EdgeClient.advanceWatermark(watermark, envelopeSeq: liveSeq, message: liveMsg)
        XCTAssertEqual(watermark, 12)

        for seq in UInt64(8)...10 {
            let (replaySeq, replayMsg) = turnEvent(seq)
            watermark = EdgeClient.advanceWatermark(
                watermark, envelopeSeq: replaySeq, message: replayMsg)
        }
        XCTAssertEqual(watermark, 12, "replayed lower seqs must not regress the watermark")

        let (nextSeq, nextMsg) = turnEvent(13)
        watermark = EdgeClient.advanceWatermark(watermark, envelopeSeq: nextSeq, message: nextMsg)
        XCTAssertEqual(watermark, 13)
    }

    /// Control frames interleaved with pushes leave the watermark at the last
    /// processed push seq — a keepalive Pong at seq 50 must not let the
    /// server prune retained frames 45...49 that are still in flight.
    func testControlFramesLeaveWatermarkAtLastProcessedPush() {
        var watermark: UInt64?
        let (seq, msg) = turnEvent(44)
        watermark = EdgeClient.advanceWatermark(watermark, envelopeSeq: seq, message: msg)
        watermark = EdgeClient.advanceWatermark(
            watermark, envelopeSeq: 50, message: .pong(nonce: 9))
        XCTAssertEqual(watermark, 44)
    }
}
