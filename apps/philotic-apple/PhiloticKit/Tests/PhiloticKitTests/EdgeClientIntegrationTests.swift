// EdgeClientIntegrationTests.swift
// Optional live e2e hook: drives a real `EdgeClient` against a running
// edge-mesh server (philotic-web's `GET /api/edge/ws`) when the environment
// points at one. This is the seam the next phase's e2e driver builds on.
//
// Skips (does not fail) when the environment isn't configured — this must
// stay true in ordinary `swift test` runs, which have neither variable set.

import XCTest

@testable import PhiloticKit

final class EdgeClientIntegrationTests: XCTestCase {
    func testConnectAndReceiveHelloAckAgainstLiveEdgeServer() async throws {
        guard
            let urlString = ProcessInfo.processInfo.environment["PHILOTIC_EDGE_URL"],
            let url = URL(string: urlString),
            let token = ProcessInfo.processInfo.environment["PHILOTIC_EDGE_TOKEN"]
        else {
            throw XCTSkip(
                "PHILOTIC_EDGE_URL / PHILOTIC_EDGE_TOKEN not set; skipping live edge-mesh e2e test."
            )
        }

        let nodeId = ProcessInfo.processInfo.environment["PHILOTIC_EDGE_NODE_ID"] ?? "e2e-driver"
        let capabilities = EdgeCapabilities(deviceName: "PhiloticKit e2e driver", platform: "ci")

        let client = EdgeClient()
        let stream = try await client.connect(
            url: url,
            bearerToken: token,
            nodeId: nodeId,
            capabilities: capabilities
        )

        var iterator = stream.makeAsyncIterator()
        guard let first = await iterator.next() else {
            XCTFail("edge stream ended before yielding HelloAck")
            await client.disconnect()
            return
        }

        guard case .helloAck(let sessionId, _) = first else {
            XCTFail("expected HelloAck as the first message, got \(first)")
            await client.disconnect()
            return
        }
        XCTAssertFalse(sessionId.isEmpty)

        await client.disconnect()
    }
}
