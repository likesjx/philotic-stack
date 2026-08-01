// LifeIndexPlanTests.swift
// Governance tests for the Spotlight entity index plane
// (seam: apple-entity-index-plane).
//
// These assert the rules that decide what philotic content is allowed to
// leave the app and enter a *system* index. Getting this wrong is not a
// cosmetic bug: retracted or contested claims answered by Siri as fact are
// hard to notice and hard to walk back once Spotlight has cached them.

import XCTest

@testable import PhiloticKit

final class LifeIndexPlanTests: XCTestCase {

    // MARK: - Helpers

    private func packet(
        id: String,
        label: String = "Commitment",
        summary: String = "Ship the entity index plane",
        state: String = "confirmed",
        confidence: Double = 0.9,
        observedAt: String? = "2026-07-26T12:00:00Z",
        score: Double = 1.0,
        packetId: String? = nil
    ) -> LifeRankedPacket {
        let json = """
            {
              "packet": {
                "packet_id": "\(packetId ?? "pkt-\(id)")",
                "claim_ref": { "id": "\(id)", "label": "\(label)" },
                "claim_summary": "\(summary)",
                "confidence": \(confidence),
                "validation_state": "\(state)",
                "observed_at": \(observedAt.map { "\"\($0)\"" } ?? "null")
              },
              "score": \(score)
            }
            """
        // Decoding (rather than memberwise init) keeps these tests honest
        // against the real wire shape the server sends.
        return try! JSONDecoder().decode(LifeRankedPacket.self, from: Data(json.utf8))
    }

    // MARK: - Validation state policy

    func testIndexableStatesAreConfirmedProposedInferred() {
        XCTAssertTrue(LifeValidationState.confirmed.isIndexable)
        XCTAssertTrue(LifeValidationState.proposed.isIndexable)
        XCTAssertTrue(LifeValidationState.inferred.isIndexable)
    }

    func testRetiredAndConflictedAreNotIndexable() {
        XCTAssertFalse(LifeValidationState.retired.isIndexable)
        XCTAssertFalse(LifeValidationState.conflicted.isIndexable)
    }

    func testUnknownValidationStateIsNotIndexable() {
        // A server-side variant we have never seen must fail closed.
        let state = LifeValidationState(rawValueOrUnknown: "some_future_state")
        XCTAssertFalse(state.isIndexable, "unrecognised validation state must not be indexed")
    }

    func testNilValidationStateIsNotIndexable() {
        XCTAssertFalse(LifeValidationState(rawValueOrUnknown: nil).isIndexable)
    }

    func testValidationStateParsingIsCaseInsensitive() {
        XCTAssertEqual(LifeValidationState(rawValueOrUnknown: "CONFIRMED"), .confirmed)
    }

    // MARK: - Donation planning

    func testConfirmedClaimIsDonated() {
        let plan = LifeIndexMapper.plan(from: [packet(id: "node-1")])
        XCTAssertEqual(plan.donate.map(\.id), ["node-1"])
        XCTAssertTrue(plan.purge.isEmpty)
    }

    func testRetiredClaimIsPurgedNotSilentlySkipped() {
        // The critical case: skipping would leave a retracted claim answerable
        // by Siri forever. It must be actively removed.
        let plan = LifeIndexMapper.plan(from: [packet(id: "node-1", state: "retired")])
        XCTAssertTrue(plan.donate.isEmpty)
        XCTAssertEqual(plan.purge, ["node-1"])
    }

    func testConflictedClaimIsPurged() {
        let plan = LifeIndexMapper.plan(from: [packet(id: "node-1", state: "conflicted")])
        XCTAssertTrue(plan.donate.isEmpty)
        XCTAssertEqual(plan.purge, ["node-1"])
    }

    func testBlankSummaryIsNotDonated() {
        let plan = LifeIndexMapper.plan(from: [packet(id: "node-1", summary: "   ")])
        XCTAssertTrue(plan.donate.isEmpty, "an untitled Spotlight entry is unactionable noise")
        XCTAssertEqual(plan.purge, ["node-1"])
    }

    func testEmptyNodeIdIsIgnoredEntirely() {
        let plan = LifeIndexMapper.plan(from: [packet(id: "")])
        XCTAssertTrue(plan.isEmpty)
    }

    // MARK: - Deduplication

    func testSameNodeFromTwoLensesIsDonatedOnce() {
        // One real-world fact must not produce two Spotlight hits.
        let plan = LifeIndexMapper.plan(from: [
            packet(id: "node-1", score: 0.4, packetId: "pkt-a"),
            packet(id: "node-1", score: 0.9, packetId: "pkt-b"),
        ])
        XCTAssertEqual(plan.donate.count, 1)
        XCTAssertEqual(plan.donate.first?.score, 0.9, "highest-scoring packet should win")
    }

    func testRetiredPacketWinsOverIndexableSiblingRegardlessOfOrder() {
        // Whichever order the batch arrives in, retirement must dominate.
        let retiredFirst = LifeIndexMapper.plan(from: [
            packet(id: "node-1", state: "retired", score: 0.1),
            packet(id: "node-1", state: "confirmed", score: 0.9),
        ])
        XCTAssertTrue(retiredFirst.donate.isEmpty)
        XCTAssertEqual(retiredFirst.purge, ["node-1"])

        let retiredSecond = LifeIndexMapper.plan(from: [
            packet(id: "node-1", state: "confirmed", score: 0.9),
            packet(id: "node-1", state: "retired", score: 0.1),
        ])
        XCTAssertTrue(
            retiredSecond.donate.isEmpty,
            "a later retirement must evict an already-accepted donation")
        XCTAssertEqual(retiredSecond.purge, ["node-1"])
    }

    func testDonationOrderIsDeterministic() {
        let plan = LifeIndexMapper.plan(from: [
            packet(id: "b", score: 0.5),
            packet(id: "a", score: 0.5),
            packet(id: "c", score: 0.9),
        ])
        XCTAssertEqual(plan.donate.map(\.id), ["c", "a", "b"])
    }

    // MARK: - Provenance survives into the index

    func testProvenanceLineCarriesLabelStateAndConfidence() {
        let plan = LifeIndexMapper.plan(from: [
            packet(id: "node-1", label: "Goal", state: "proposed", confidence: 0.75)
        ])
        let line = plan.donate.first?.provenanceLine
        XCTAssertEqual(line, "Goal · Proposed · 75% confidence")
    }

    func testSnapshotPreservesObservedAt() {
        let plan = LifeIndexMapper.plan(from: [
            packet(id: "node-1", observedAt: "2026-01-02T03:04:05Z")
        ])
        XCTAssertEqual(plan.donate.first?.observedAt, "2026-01-02T03:04:05Z")
    }

    // MARK: - Incremental change frames

    func testRetiredChangeFramePurges() {
        for kind in ["retired", "deleted", "removed", "retracted", "RETIRED"] {
            let plan = LifeIndexMapper.plan(
                forChangeKind: kind, nodeId: "node-1", label: "Goal", summary: "x")
            XCTAssertEqual(plan.purge, ["node-1"], "change kind \(kind) should purge")
        }
    }

    func testNonRemovalChangeFrameDonatesNothing() {
        // Change frames carry no validation state, so they must never be
        // treated as authorisation to index.
        let plan = LifeIndexMapper.plan(
            forChangeKind: "updated", nodeId: "node-1", label: "Goal", summary: "new title")
        XCTAssertTrue(
            plan.isEmpty,
            "a change frame lacks provenance and must not authorise a donation")
    }

    func testChangeFrameWithEmptyNodeIdIsIgnored() {
        let plan = LifeIndexMapper.plan(
            forChangeKind: "retired", nodeId: "", label: nil, summary: nil)
        XCTAssertTrue(plan.isEmpty)
    }
}
