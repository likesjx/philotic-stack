// LifeGraphClientTests.swift
// Exercises LifeGraphClient against StubURLProtocol with fixtures mirroring
// the real server shapes: the life.recall RetrievalContextPacket envelope and
// the life.view.node / life.view.neighborhood read-only surfaces.

import XCTest

@testable import PhiloticKit

final class LifeGraphClientTests: XCTestCase {
    override func tearDown() {
        StubURLProtocol.responder = nil
        super.tearDown()
    }

    private let baseURL = URL(string: "https://hotel.example")!

    func testFetchLensDecodesContextPacketAndSendsQuery() async throws {
        // Verbatim server shape: RetrievalContextPacket with one ranked
        // EvidencePacket (snake_case keys, extra fields the client ignores).
        let body = Data(
            """
            {"lens":"open_loops_by_context","data":{
              "status":"ok","named_strategy":"open_loops_by_context","fallback_used":false,
              "context_packet":{
                "context_id":"ctx-1","query_id":"edge-lens-open_loops_by_context",
                "strategy":"semantic_pivot_bounded_expansion",
                "token_budget":2048,"generated_at":"2026-07-11T18:00:00Z",
                "ranked_packets":[{
                  "score":0.91,
                  "matched_policy_filters":["exclude_retired"],
                  "evidence_path":[{"id":"openloop-1","label":"OpenLoop"}],
                  "packet":{
                    "packet_id":"proj:openloop-1:2026-07-11",
                    "claim_ref":{"id":"openloop-1","label":"OpenLoop","datasource":"life-graph"},
                    "claim_summary":"call the pharmacy about refill",
                    "source_refs":[],"passage_refs":[],
                    "confidence":0.8,"validation_state":"proposed",
                    "observed_at":"2026-07-10T15:00:00Z",
                    "source_reliability":0.7,"conflict_ids":[],
                    "adjudication_status":"not_needed",
                    "metadata":{"from_vector_search":true,"similarity":0.91}
                  }
                }]
              }
            }}
            """.utf8)

        let capturedURL = Locked<URL?>(nil)
        StubURLProtocol.responder = { request in
            capturedURL.value = request.url
            return .init(statusCode: 200, body: body)
        }

        let client = LifeGraphClient(session: .stubbed())
        let response = try await client.fetchLens(
            baseURL: baseURL,
            bearerToken: "tok-1",
            lens: "open_loops_by_context",
            context: "porch project",
            limit: 5
        )

        XCTAssertEqual(capturedURL.value?.path, "/api/edge/lifegraph/lens/open_loops_by_context")
        let query = capturedURL.value?.query ?? ""
        XCTAssertTrue(query.contains("context=porch%20project"), "query was: \(query)")
        XCTAssertTrue(query.contains("limit=5"), "query was: \(query)")

        XCTAssertEqual(response.lens, "open_loops_by_context")
        XCTAssertEqual(response.data.status, "ok")
        XCTAssertEqual(response.data.namedStrategy, "open_loops_by_context")
        let packets = response.data.contextPacket?.rankedPackets ?? []
        XCTAssertEqual(packets.count, 1)
        XCTAssertEqual(packets[0].score, 0.91, accuracy: 0.0001)
        XCTAssertEqual(packets[0].packet.claimRef.id, "openloop-1")
        XCTAssertEqual(packets[0].packet.claimRef.label, "OpenLoop")
        XCTAssertEqual(packets[0].packet.claimSummary, "call the pharmacy about refill")
        XCTAssertEqual(packets[0].packet.validationState, "proposed")
        XCTAssertEqual(packets[0].packet.confidence, 0.8, accuracy: 0.0001)
    }

    func testFetchNodeDecodesProvenanceAndNeighbors() async throws {
        let body = Data(
            """
            {"status":"ok","read_only":true,"id":"goal-1",
             "node":{"kind":"node","id":42,"labels":["Goal"],
               "properties":{"id":"goal-1","title":"Ship the app","validation_state":"confirmed",
                 "confidence":0.9,"source_membrane":"edge:ios","observed_at":"2026-07-01T00:00:00Z"}},
             "edge_limit":50,"neighbor_count":1,
             "neighbors":[{"rel_type":"CONTAINS","from_id":"goal-1","to_id":"nextaction-7",
               "node":{"kind":"node","id":43,"labels":["NextAction"],
                 "properties":{"id":"nextaction-7","title":"file the TestFlight build"}}}]}
            """.utf8)
        StubURLProtocol.responder = { _ in .init(statusCode: 200, body: body) }

        let client = LifeGraphClient(session: .stubbed())
        let detail = try await client.fetchNode(
            baseURL: baseURL, bearerToken: "tok-1", nodeId: "goal-1")

        XCTAssertEqual(detail.status, "ok")
        XCTAssertEqual(detail.node?.canonicalId, "goal-1")
        XCTAssertEqual(detail.node?.primaryLabel, "Goal")
        XCTAssertEqual(detail.node?.string("validation_state"), "confirmed")
        XCTAssertEqual(detail.node?.string("source_membrane"), "edge:ios")
        XCTAssertEqual(detail.neighbors.count, 1)
        XCTAssertEqual(detail.neighbors[0].relType, "CONTAINS")
        XCTAssertEqual(detail.neighbors[0].node?.canonicalId, "nextaction-7")
    }

    func testFetchNeighborhoodDecodesNodesAndEdges() async throws {
        let body = Data(
            """
            {"status":"ok","read_only":true,"origin_id":"goal-1","depth":2,"max_nodes":50,
             "node_count":2,
             "nodes":[
               {"kind":"node","labels":["Goal"],"properties":{"id":"goal-1"}},
               {"kind":"node","labels":["OpenLoop"],"properties":{"id":"openloop-1"}}],
             "edges":[{"from":"goal-1","rel_type":"SPAWNS","to":"openloop-1"}],
             "truncated":false}
            """.utf8)
        StubURLProtocol.responder = { _ in .init(statusCode: 200, body: body) }

        let client = LifeGraphClient(session: .stubbed())
        let hood = try await client.fetchNeighborhood(
            baseURL: baseURL, bearerToken: "tok-1", nodeId: "goal-1", depth: 2)

        XCTAssertEqual(hood.originId, "goal-1")
        XCTAssertEqual(hood.nodes.count, 2)
        XCTAssertEqual(hood.edges.count, 1)
        XCTAssertEqual(hood.edges[0].relType, "SPAWNS")
        XCTAssertEqual(hood.truncated, false)
    }

    func testNon200Throws() async {
        StubURLProtocol.responder = { _ in .init(statusCode: 502, body: Data()) }
        let client = LifeGraphClient(session: .stubbed())
        do {
            _ = try await client.fetchLens(
                baseURL: baseURL, bearerToken: "tok", lens: "open_loops_by_context")
            XCTFail("expected fetchLens to throw")
        } catch let LifeGraphClient.LifeGraphError.badResponse(status) {
            XCTAssertEqual(status, 502)
        } catch {
            XCTFail("unexpected error: \(error)")
        }
    }
}
