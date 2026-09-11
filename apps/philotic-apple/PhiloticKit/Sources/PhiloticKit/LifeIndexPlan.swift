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

/// Which memory plane a snapshot came from.
///
/// The index spans two planes with genuinely different provenance
/// vocabularies — LifeGraph has `validation_state`, Muninn has a trust tier —
/// so the source travels with each snapshot rather than being inferred from
/// the id. It also namespaces ids: a LifeGraph node and a Muninn engram could
/// otherwise collide in the index.
public enum LifeIndexSource: String, Codable, Equatable, Sendable {
    case lifeGraph = "life"
    case muninn = "muninn"

    /// Prefix applied to the index identifier so the two planes cannot
    /// collide, and so a purge targets exactly one plane.
    public var idPrefix: String { "\(rawValue):" }

    public var displayName: String {
        switch self {
        case .lifeGraph: return "Life Graph"
        case .muninn: return "Memory"
        }
    }
}

/// Muninn's trust tiers, mirroring `TrustTier` in ansible-mesh-core.
///
/// `unknown` is not a tier — it means the stored memory carries no provenance
/// envelope. Per Standing Rule 2 it is never silently upgraded, and per the
/// donation rule below it is never indexed.
public enum LifeTrustTier: String, Codable, Equatable, Sendable {
    case observed
    case inferred
    case told
    case unknown

    public init(rawValueOrUnknown raw: String?) {
        guard let raw, let parsed = LifeTrustTier(rawValue: raw.lowercased()) else {
            self = .unknown
            return
        }
        self = parsed
    }

    /// May a memory at this tier be donated to the system index?
    ///
    /// Fail-closed on `unknown`: putting un-provenanced memory in front of
    /// Siri means the operator cannot tell where an answer came from and has
    /// no reversal path. Withholding is the conservative failure, and the
    /// count of withheld memories is surfaced on the plan so the gap is
    /// visible rather than silent (Memory Transparency, Standing Rule 1).
    public var isIndexable: Bool {
        switch self {
        case .observed, .told, .inferred: return true
        case .unknown: return false
        }
    }

    public var chip: String {
        switch self {
        case .observed: return "Observed"
        case .inferred: return "Inferred"
        case .told: return "Told"
        case .unknown: return "Unattributed"
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
    public let source: LifeIndexSource
    /// Muninn trust tier. `nil` for LifeGraph snapshots, which express
    /// provenance through `validationState` instead.
    public let trust: LifeTrustTier?

    public init(
        id: String,
        label: String,
        summary: String,
        validationState: LifeValidationState,
        confidence: Double,
        observedAt: String?,
        score: Double,
        source: LifeIndexSource = .lifeGraph,
        trust: LifeTrustTier? = nil
    ) {
        self.id = id
        self.label = label
        self.summary = summary
        self.validationState = validationState
        self.confidence = confidence
        self.observedAt = observedAt
        self.score = score
        self.source = source
        self.trust = trust
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        label = try c.decode(String.self, forKey: .label)
        summary = try c.decode(String.self, forKey: .summary)
        validationState = try c.decode(LifeValidationState.self, forKey: .validationState)
        confidence = try c.decode(Double.self, forKey: .confidence)
        observedAt = try c.decodeIfPresent(String.self, forKey: .observedAt)
        score = try c.decode(Double.self, forKey: .score)
        // Caches written before the Muninn extension carry neither field.
        source = (try? c.decodeIfPresent(LifeIndexSource.self, forKey: .source)) ?? .lifeGraph
        trust = try? c.decodeIfPresent(LifeTrustTier.self, forKey: .trust)
    }

    /// Identifier used in the system index — namespaced by plane so a
    /// LifeGraph node and a Muninn engram can never collide.
    public var indexId: String { source.idPrefix + id }

    /// Provenance line shown under the title in Spotlight results.
    public var provenanceLine: String {
        let pct = Int((confidence * 100).rounded())
        switch source {
        case .lifeGraph:
            return "\(label) · \(validationState.chip) · \(pct)% confidence"
        case .muninn:
            let tier = (trust ?? .unknown).chip
            return "Memory · \(tier) · \(pct)% confidence"
        }
    }
}

/// What the donor should do on this refresh.
///
/// `purge` is as load-bearing as `donate`: a node that transitions to
/// `retired` must be actively removed from the index, not merely skipped,
/// or Spotlight keeps answering with content the graph has retracted.
public struct LifeIndexPlan: Equatable, Sendable {
    public let donate: [LifeIndexSnapshot]
    /// Namespaced index identifiers (`LifeIndexSnapshot.indexId`) to remove.
    public let purge: [String]
    /// How many records were withheld for lack of usable provenance.
    ///
    /// Surfaced rather than swallowed: a silently-empty index looks identical
    /// to a working one, and the whole point of the fail-closed rule is that
    /// the operator can see the gap (Memory Transparency, Standing Rule 1).
    public let withheld: Int

    public init(donate: [LifeIndexSnapshot], purge: [String], withheld: Int = 0) {
        self.donate = donate
        self.purge = purge
        self.withheld = withheld
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
                purge.insert(LifeIndexSource.lifeGraph.idPrefix + id)
                best.removeValue(forKey: id)
                continue
            }

            // A node already marked for purge is not resurrected by a
            // higher-scoring indexable packet in the same batch.
            guard !purge.contains(LifeIndexSource.lifeGraph.idPrefix + id) else { continue }

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

    /// Map structured Muninn recall results into a donate/purge plan.
    ///
    /// Governance differs from the LifeGraph path because the provenance
    /// vocabulary differs:
    ///
    /// - `deleted` (Muninn soft-delete) → purge, never donate.
    /// - `trust == unknown` → withhold. A memory with no provenance envelope
    ///   must not become a Siri answer the operator cannot trace or reverse.
    ///   These are counted in `withheld` rather than dropped silently, so the
    ///   adoption gap is observable instead of looking like an empty index.
    /// - blank concept *and* content → unusable, withhold.
    ///
    /// Note this is intentionally stricter than the LifeGraph rule, where
    /// `inferred` is indexable: LifeGraph's `inferred` is a positive assertion
    /// by the graph, whereas Muninn's `unknown` is the *absence* of any
    /// assertion. Those are not the same claim.
    public static func plan(fromMemories memories: [MuninnMemory]) -> LifeIndexPlan {
        var best: [String: LifeIndexSnapshot] = [:]
        var purge: Set<String> = []
        var withheld = 0

        for memory in memories {
            let id = memory.id.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !id.isEmpty else { continue }
            let indexId = LifeIndexSource.muninn.idPrefix + id

            if memory.deleted {
                purge.insert(indexId)
                best.removeValue(forKey: id)
                continue
            }

            let tier = LifeTrustTier(rawValueOrUnknown: memory.trust)
            let concept = memory.concept.trimmingCharacters(in: .whitespacesAndNewlines)
            let content = memory.content.trimmingCharacters(in: .whitespacesAndNewlines)
            let summary = concept.isEmpty ? content : concept

            guard tier.isIndexable, !summary.isEmpty else {
                withheld += 1
                continue
            }
            guard !purge.contains(indexId) else { continue }

            let snapshot = LifeIndexSnapshot(
                id: id,
                label: content.isEmpty ? "Memory" : content,
                summary: summary,
                // Muninn has no validation state; `confirmed` records that the
                // memory is live (not soft-deleted). Trust carries the real
                // provenance signal and is rendered in `provenanceLine`.
                validationState: .confirmed,
                confidence: memory.confidence,
                observedAt: memory.updatedAt.map(Self.iso8601(fromEpochSeconds:)),
                score: memory.confidence,
                source: .muninn,
                trust: tier
            )
            best[id] = snapshot
        }

        let donate = best.values.sorted {
            $0.score == $1.score ? $0.id < $1.id : $0.score > $1.score
        }
        return LifeIndexPlan(donate: donate, purge: purge.sorted(), withheld: withheld)
    }

    /// Muninn stores epoch seconds; the index wants ISO-8601.
    static func iso8601(fromEpochSeconds seconds: UInt64) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        return formatter.string(from: Date(timeIntervalSince1970: TimeInterval(seconds)))
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
            return LifeIndexPlan(
                donate: [], purge: [LifeIndexSource.lifeGraph.idPrefix + nodeId])
        default:
            // Not a removal. We lack provenance for a safe donation, so do
            // nothing and let the next authoritative refresh reconcile.
            return LifeIndexPlan(donate: [], purge: [])
        }
    }
}
