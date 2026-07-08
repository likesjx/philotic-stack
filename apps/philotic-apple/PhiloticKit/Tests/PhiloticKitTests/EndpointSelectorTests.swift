// EndpointSelectorTests.swift
// Exercises EndpointSelector's health-probe race, timeout handling, and
// per-network-path stickiness, entirely against `StubURLProtocol` — no live
// server involved.

import XCTest

@testable import PhiloticKit

final class EndpointSelectorTests: XCTestCase {
    override func tearDown() {
        StubURLProtocol.responder = nil
        super.tearDown()
    }

    private func candidate(_ name: String) -> EndpointCandidate {
        EndpointCandidate(name: name, baseURL: URL(string: "https://\(name).example")!)
    }

    func testSelectsFastestHealthyCandidate() async {
        let slow = candidate("slow")
        let fast = candidate("fast")
        let unreachable = candidate("down")

        StubURLProtocol.responder = { request in
            guard let host = request.url?.host else { return nil }
            switch host {
            case "slow.example":
                return .init(statusCode: 200, delay: 0.3)
            case "fast.example":
                return .init(statusCode: 200, delay: 0.02)
            default:
                return nil
            }
        }

        let selector = EndpointSelector(session: .stubbed(), probeTimeout: 1.0)
        let winner = await selector.selectEndpoint(from: [slow, fast, unreachable])
        XCTAssertEqual(winner, fast)
    }

    func testUnhealthyCandidatesAreSkipped() async {
        let unhealthy = candidate("unhealthy")
        let healthy = candidate("healthy")

        StubURLProtocol.responder = { request in
            guard let host = request.url?.host else { return nil }
            switch host {
            case "unhealthy.example":
                return .init(statusCode: 503)
            case "healthy.example":
                return .init(statusCode: 200)
            default:
                return nil
            }
        }

        let selector = EndpointSelector(session: .stubbed(), probeTimeout: 1.0)
        let winner = await selector.selectEndpoint(from: [unhealthy, healthy])
        XCTAssertEqual(winner, healthy)
    }

    func testNoHealthyCandidateFallsBackToAnchor() async {
        let unreachable = candidate("down")
        let anchor = candidate("anchor")

        StubURLProtocol.responder = { request in
            guard let host = request.url?.host else { return nil }
            return host == "anchor.example" ? .init(statusCode: 200) : nil
        }

        let selector = EndpointSelector(session: .stubbed(), probeTimeout: 0.5, anchor: anchor)
        let winner = await selector.selectEndpoint(from: [unreachable])
        XCTAssertEqual(winner, anchor)
    }

    func testReturnsNilWhenNothingIsReachable() async {
        let unreachable = candidate("down")
        StubURLProtocol.responder = { _ in nil }

        let selector = EndpointSelector(session: .stubbed(), probeTimeout: 0.3)
        let winner = await selector.selectEndpoint(from: [unreachable])
        XCTAssertNil(winner)
    }

    func testStickyPerNetworkPathAvoidsRaceOnSubsequentCalls() async {
        let a = candidate("a")
        let b = candidate("b")
        let probeCount = Locked(0)

        StubURLProtocol.responder = { request in
            probeCount.value += 1
            guard let host = request.url?.host else { return nil }
            return (host == "a.example" || host == "b.example") ? .init(statusCode: 200) : nil
        }

        let selector = EndpointSelector(session: .stubbed(), probeTimeout: 1.0)
        let first = await selector.selectEndpoint(from: [a, b], networkPathId: "wifi-1")
        XCTAssertNotNil(first)

        let countAfterFirst = probeCount.value
        XCTAssertGreaterThan(countAfterFirst, 0)

        let second = await selector.selectEndpoint(from: [a, b], networkPathId: "wifi-1")
        XCTAssertEqual(second, first, "sticky endpoint should be reused for the same network path")

        let countAfterSecond = probeCount.value
        // The sticky path re-probes only the sticky candidate (1 more call),
        // not a fresh race across all candidates.
        XCTAssertEqual(countAfterSecond, countAfterFirst + 1)
    }

    func testForgetStickyEndpointClearsMemory() async {
        let a = candidate("a")
        StubURLProtocol.responder = { _ in .init(statusCode: 200) }

        let selector = EndpointSelector(session: .stubbed(), probeTimeout: 1.0)
        _ = await selector.selectEndpoint(from: [a], networkPathId: "wifi-1")
        await selector.forgetStickyEndpoint(forPath: "wifi-1")

        let probedAfterForget = Locked(false)
        StubURLProtocol.responder = { _ in
            probedAfterForget.value = true
            return .init(statusCode: 200)
        }
        _ = await selector.selectEndpoint(from: [a], networkPathId: "wifi-1")
        XCTAssertTrue(probedAfterForget.value)
    }
}
