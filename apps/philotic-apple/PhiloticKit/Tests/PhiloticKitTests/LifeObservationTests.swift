// LifeObservationTests.swift
// Encode-golden test for the LifeGraph observe write-plane contract: a
// constructed LifeObservation must encode byte-equivalently (key-insensitive,
// deep) to the server's canonical JSON, and postObservations must POST it to
// the observe endpoint with edge-bearer auth.

import XCTest

@testable import PhiloticKit

final class LifeObservationTests: XCTestCase {
    override func tearDown() {
        StubURLProtocol.responder = nil
        super.tearDown()
    }

    private let baseURL = URL(string: "https://hotel.example")!

    /// Byte-for-byte from the server's `LifeObserveInput` / `EvidencePacket`
    /// contract.
    private let goldenObservation =
        """
        {
          "observation_id": "obs-uuid",
          "evidence": {
            "packet_id": "pkt-uuid",
            "claim_ref": { "id": "healthmetric:resting-hr:2026-07-14", "label": "Signal", "datasource": "memgraph" },
            "claim_summary": "Resting heart rate 58 bpm (daily avg, 2026-07-14)",
            "source_refs": [{ "source_id": "edge:ios-healthkit", "source_kind": "runtime_observation", "reliability": { "score": 0.95, "basis": "direct_observation" } }],
            "confidence": 0.95,
            "validation_state": "proposed",
            "observed_at": "2026-07-14T23:59:00Z",
            "source_reliability": 0.95,
            "adjudication_status": "not_needed",
            "metadata": { "metric": "resting_heart_rate", "unit": "count/min", "value": 58 }
          },
          "observed_by": "edge:ios-healthkit",
          "observed_role": "sensor"
        }
        """

    private func makeGoldenObservation() -> LifeObservation {
        LifeObservation(
            observationId: "obs-uuid",
            evidence: EvidencePacket(
                packetId: "pkt-uuid",
                claimRef: GraphRecordRef(
                    id: "healthmetric:resting-hr:2026-07-14",
                    label: "Signal",
                    datasource: "memgraph"
                ),
                claimSummary: "Resting heart rate 58 bpm (daily avg, 2026-07-14)",
                sourceRefs: [
                    SourceRef(
                        sourceId: "edge:ios-healthkit",
                        sourceKind: "runtime_observation",
                        reliability: Reliability(score: 0.95, basis: "direct_observation")
                    )
                ],
                confidence: 0.95,
                validationState: "proposed",
                observedAt: "2026-07-14T23:59:00Z",
                sourceReliability: 0.95,
                adjudicationStatus: "not_needed",
                metadata: [
                    "metric": .string("resting_heart_rate"),
                    "unit": .string("count/min"),
                    "value": .number(58),
                ]
            ),
            observedBy: "edge:ios-healthkit",
            observedRole: "sensor"
        )
    }

    func testObservationEncodesToGoldenJSON() throws {
        let encoded = try JSONEncoder().encode(makeGoldenObservation())
        try assertJSONEquivalent(encoded, Data(goldenObservation.utf8))
    }

    func testObservationRoundTrips() throws {
        let original = makeGoldenObservation()
        let encoded = try JSONEncoder().encode(original)
        let decoded = try JSONDecoder().decode(LifeObservation.self, from: encoded)
        XCTAssertEqual(decoded, original)
    }

    func testDatasourceOmittedWhenNil() throws {
        let ref = GraphRecordRef(id: "x", label: "Y")
        let encoded = try JSONEncoder().encode(ref)
        let object = try JSONSerialization.jsonObject(with: encoded) as? [String: Any]
        XCTAssertNil(object?["datasource"], "datasource must be omitted when nil")
        XCTAssertEqual(object?["id"] as? String, "x")
        XCTAssertEqual(object?["label"] as? String, "Y")
    }

    func testPostObservationsSendsBatchToObserveEndpoint() async throws {
        let capturedURL = Locked<URL?>(nil)
        let capturedAuth = Locked<String?>(nil)
        let capturedBody = Locked<Data?>(nil)
        StubURLProtocol.responder = { request in
            capturedURL.value = request.url
            capturedAuth.value = request.value(forHTTPHeaderField: "Authorization")
            capturedBody.value = StubURLProtocol.resolvedBody(for: request)
            return .init(
                statusCode: 200,
                body: Data(
                    #"{"status":"ok","results":[{"observation_id":"obs-uuid","status":"ok"}]}"#.utf8
                )
            )
        }

        let client = LifeGraphClient(session: .stubbed())
        let result = try await client.postObservations(
            [makeGoldenObservation()], baseURL: baseURL, bearerToken: "tok-1")

        XCTAssertEqual(capturedURL.value?.path, "/api/edge/lifegraph/observe")
        XCTAssertEqual(capturedAuth.value, "Bearer tok-1")
        XCTAssertEqual(result.status, "ok")
        XCTAssertEqual(result.results.count, 1)
        XCTAssertEqual(result.results.first?.observationId, "obs-uuid")

        // Body is { "observations": [ <the golden observation> ] }.
        let body = try XCTUnwrap(capturedBody.value)
        let root = try JSONSerialization.jsonObject(with: body) as? [String: Any]
        let observations = try XCTUnwrap(root?["observations"] as? [[String: Any]])
        XCTAssertEqual(observations.count, 1)
        XCTAssertEqual(observations.first?["observation_id"] as? String, "obs-uuid")
    }

    func testPostEmptyObservationsIsNoOp() async throws {
        let hit = Locked<Bool>(false)
        StubURLProtocol.responder = { _ in
            hit.value = true
            return .init(statusCode: 200, body: Data(#"{"status":"ok","results":[]}"#.utf8))
        }
        let client = LifeGraphClient(session: .stubbed())
        let result = try await client.postObservations([], baseURL: baseURL, bearerToken: "tok")
        XCTAssertEqual(result.status, "ok")
        XCTAssertFalse(hit.value, "empty input must not hit the network")
    }

    func testPostObservationsChunksAbove25() async throws {
        let batchSizes = Locked<[Int]>([])
        StubURLProtocol.responder = { request in
            if let body = StubURLProtocol.resolvedBody(for: request),
                let root = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
                let obs = root["observations"] as? [[String: Any]]
            {
                batchSizes.value.append(obs.count)
            }
            return .init(statusCode: 200, body: Data(#"{"status":"ok","results":[]}"#.utf8))
        }

        // 60 observations => 25 + 25 + 10 across three POSTs.
        let observations = (0..<60).map { i -> LifeObservation in
            var obs = makeGoldenObservation()
            obs = LifeObservation(
                observationId: "obs-\(i)",
                evidence: obs.evidence,
                observedBy: obs.observedBy,
                observedRole: obs.observedRole
            )
            return obs
        }
        let client = LifeGraphClient(session: .stubbed())
        let result = try await client.postObservations(
            observations, baseURL: baseURL, bearerToken: "tok")

        XCTAssertEqual(batchSizes.value.sorted(by: >), [25, 25, 10])
        XCTAssertEqual(result.status, "ok")
    }

    func testPostNon200Throws() async {
        StubURLProtocol.responder = { _ in .init(statusCode: 422, body: Data()) }
        let client = LifeGraphClient(session: .stubbed())
        do {
            _ = try await client.postObservations(
                [makeGoldenObservation()], baseURL: baseURL, bearerToken: "tok")
            XCTFail("expected postObservations to throw")
        } catch let LifeGraphClient.LifeGraphError.badResponse(status) {
            XCTAssertEqual(status, 422)
        } catch {
            XCTFail("unexpected error: \(error)")
        }
    }

    func testNestedBatchAcknowledgmentDecodesRealRunnerShape() async throws {
        StubURLProtocol.responder = { _ in
            .init(
                statusCode: 200,
                body: Data(
                    #"{"status":"ok","results":[{"index":0,"result":{"observation_id":"obs-uuid","status":"proposed","node_id":"signal:health"}}]}"#
                        .utf8))
        }
        let result = try await LifeGraphClient(session: .stubbed()).postObservations(
            [makeGoldenObservation()], baseURL: baseURL, bearerToken: "test")
        XCTAssertEqual(result.status, "ok")
        XCTAssertEqual(result.results.first?.status, "proposed")
        XCTAssertEqual(result.results.first?.observationId, "obs-uuid")
        XCTAssertEqual(result.results.first?.nodeId, "signal:health")
    }

    func testFailedInvalidAndUnknownBatchStatusesCannotBecomeOK() async throws {
        for status in ["failed", "invalid_request", "unknown", "blocked"] {
            let response = Data("{\"status\":\"\(status)\",\"results\":[]}".utf8)
            StubURLProtocol.responder = { _ in .init(statusCode: 200, body: response) }
            let result = try await LifeGraphClient(session: .stubbed()).postObservations(
                [makeGoldenObservation()], baseURL: baseURL, bearerToken: "test")
            XCTAssertEqual(result.status, "error", status)
        }
    }

    func testPartialBatchRemainsPartial() async throws {
        StubURLProtocol.responder = { _ in
            .init(statusCode: 200, body: Data(#"{"status":"partial","results":[]}"#.utf8))
        }
        let result = try await LifeGraphClient(session: .stubbed()).postObservations(
            [makeGoldenObservation()], baseURL: baseURL, bearerToken: "test")
        XCTAssertEqual(result.status, "partial")
    }

    /// Key-order-insensitive deep structural equality via JSONSerialization.
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
