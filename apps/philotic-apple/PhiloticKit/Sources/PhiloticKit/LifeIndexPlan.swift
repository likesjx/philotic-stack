// LifeIndexPlan.swift
// Pure mapping + governance policy for the Spotlight entity index plane
// (seam: apple-entity-index-plane).
//
// This file deliberately contains NO AppIntents / CoreSpotlight imports. The
// decision of *what may be donated to a system index* is governance, not UI,
// so it lives here as pure value types that unit-test under `swift test`
// without Xcode, a simulator, or an Apple Intelligence-capable device.
//
// Donation rule (from APPLE_INTELLIGENCE_PLANE_PROPOSAL.md, Slice A):
// only content already permitted through the `life.view.*` / `life.recall`
// projections is eligible, and the provenance envelope travels with it.
// Nothing is synthesised here that the server did not already release.

import Foundation

/// Validation states the LifeGraph assigns to a claim.
///
/// Mirrors `ValidationState` in `crates/data-memorygraphrag/src/cypher.rs`.
/// Kept as a string-backed enum with an `unknown` escape hatch so a new
/// server-side variant degrades to "do not index" rather than crashing or,
/// worse, silently indexing something the server considers unsafe.
public enum LifeValidationState: String, Codable, Equatable, Sendable {
    case inferred
    case proposed
    case confirmed
    case retired
    case conflicted

    public init(rawValueOrUnknown raw: String?) {
        guard let raw, let parsed = LifeValidationState(rawValue: raw.lowercased()) else {
            self = .unknownFallback
            return
        }
        self = parsed
    }

    /// Unrecognised states map here. Treated as non-indexable *and*
    /// purge-worthy: if we cannot reason about a state we must not leave a
    /// stale copy sitting in the system index.
    public static let unknownFallback: LifeValidationState = .conflicted

    /// May this claim be donated to the Spotlight semantic index?
    ///
    /// `retired` and `conflicted` are excluded for different reasons:
    /// retired content is no longer true, and conflicted content is contested
    /// — surfacing a contested claim to Siri as fact is worse than not
    /// answering, because the operator cannot see the conflict from Spotlight.
    public var isIndexable: Bool {
        switch self {
        case .confirmed, .proposed, .inferred: return true
        case .retired, .conflicted: return false
        }
    }

    /// Human-readable provenance chip carried into the index entry, so
    /// attribution survives into Spotlight rather than being flattened away.
    public var chip: String {
        switch self {
        case .confirmed: return "Confirmed"
        case .proposed: return "Proposed"
        case .inferred: return "Inferred"
        case .retired: return "Retired"
        case .conflicted: return "Conflicted"
        }
    }
}

/// One LifeGraph claim, flattened to exactly what the system index needs.
///
/// Keyed by the **graph node id** (`claimRef.id`), not the packet id: the same
/// node legitimately appears in several lenses and several evidence packets,
/// and donating it once per packet would produce duplicate Spotlight hits for
/// one real-world fact.
public struct LifeIndexSnapshot: Codable, Equatable, Sendable, Identifiable {
    public let id: String
    public let label: String
    public let summary: String
    public let validationState: LifeValidationState
    public let confidence: Double
    public let observedAt: String?
    /// Retrieval score of the best packet that carried this node. Used to
    /// break ties when the same node arrives from two lenses.
    public let score: Double

    public init(
        id: String,
        label: String,
        summary: String,
        validationState: LifeValidationState,
        confidence: Double,
        observedAt: String?,
        score: Double
    ) {
        self.id = id
        self.label = label
        self.summary = summary
        self.validationState = validationState
        self.confidence = confidence
        self.observedAt = observedAt
        self.score = score
    }

    /// Provenance line shown under the title in Spotlight results.
    public var provenanceLine: String {
        let pct = Int((confidence * 100).rounded())
        return "\(label) · \(validationState.chip) · \(pct)% confidence"
    }
}

/// What the donor should do on this refresh.
///
/// `purge` is as load-bearing as `donate`: a node that transitions to
/// `retired` must be actively removed from the index, not merely skipped,
/// or Spotlight keeps answering with content the graph has retracted.
public struct LifeIndexPlan: Equatable, Sendable {
    public let donate: [LifeIndexSnapshot]
    public let purge: [String]

    public init(donate: [LifeIndexSnapshot], purge: [String]) {
        self.donate = donate
        self.purge = purge
    }

    public var isEmpty: Bool { donate.isEmpty && purge.isEmpty }
}

/// Builds donation plans from lens results. Pure and deterministic.
public enum LifeIndexMapper {
    /// Map lens packets into a donate/purge plan.
    ///
    /// - Deduplicates by graph node id, keeping the highest-scoring packet.
    /// - Splits on `LifeValidationState.isIndexable`.
    /// - Drops packets with a blank summary: an untitled Spotlight entry is
    ///   noise the operator cannot act on.
    public static func plan(from packets: [LifeRankedPacket]) -> LifeIndexPlan {
        var best: [String: LifeIndexSnapshot] = [:]
        var purge: Set<String> = []

        for ranked in packets {
            let packet = ranked.packet
            let id = packet.claimRef.id
            guard !id.isEmpty else { continue }

            let state = LifeValidationState(rawValueOrUnknown: packet.validationState)
            let summary = packet.claimSummary.trimmingCharacters(in: .whitespacesAndNewlines)

            guard state.isIndexable, !summary.isEmpty else {
                // Non-indexable (or unusable) wins over any indexable sibling:
                // when the graph says retired/conflicted anywhere, the safe
                // resolution is to remove the node from the index entirely.
                purge.insert(id)
                best.removeValue(forKey: id)
                continue
            }

            // A node already marked for purge is not resurrected by a
            // higher-scoring indexable packet in the same batch.
            guard !purge.contains(id) else { continue }

            let candidate = LifeIndexSnapshot(
                id: id,
                label: packet.claimRef.label,
                summary: summary,
                validationState: state,
                confidence: packet.confidence,
                observedAt: packet.observedAt,
                score: ranked.score
            )

            if let existing = best[id], existing.score >= candidate.score { continue }
            best[id] = candidate
        }

        // Stable ordering so tests and donation batches are reproducible.
        let donate = best.values.sorted {
            $0.score == $1.score ? $0.id < $1.id : $0.score > $1.score
        }
        return LifeIndexPlan(donate: donate, purge: purge.sorted())
    }

    /// Map a `LifeGraphChange` frame into an incremental plan.
    ///
    /// Live changes carry only `change_kind` / `node_id` / `label` / `summary`
    /// with no validation state, so a change can only ever *remove* from the
    /// index or refresh a title. Anything else waits for the next lens
    /// refresh, which carries the full provenance envelope.
    public static func plan(
        forChangeKind changeKind: String,
        nodeId: String,
        label: String?,
        summary: String?
    ) -> LifeIndexPlan {
        guard !nodeId.isEmpty else { return LifeIndexPlan(donate: [], purge: []) }

        switch changeKind.lowercased() {
        case "retired", "deleted", "removed", "retracted":
            return LifeIndexPlan(donate: [], purge: [nodeId])
        default:
            // Not a removal. We lack provenance for a safe donation, so do
            // nothing and let the next authoritative refresh reconcile.
            return LifeIndexPlan(donate: [], purge: [])
        }
    }
}
