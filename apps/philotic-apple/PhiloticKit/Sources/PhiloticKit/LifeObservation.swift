// LifeObservation.swift
// Write plane for the edge LifeGraph: the observation/evidence contract the
// device POSTs to `/api/edge/lifegraph/observe`. These mirror the server's
// `LifeObserveInput` / `EvidencePacket` byte-for-byte (snake_case). Built
// fresh (and separate from the read-plane `LifeEvidencePacket`, which has a
// larger, decode-only shape) because the observe payload is smaller and is
// *encoded* by the app, so every type carries a public memberwise init.

import Foundation

/// A record reference (the claim this evidence is about).
public struct GraphRecordRef: Codable, Equatable, Sendable {
    public let id: String
    public let label: String
    /// Optional datasource hint (e.g. "memgraph"); omitted on the wire when nil.
    public let datasource: String?

    public init(id: String, label: String, datasource: String? = nil) {
        self.id = id
        self.label = label
        self.datasource = datasource
    }
}

/// Reliability of a single source: a score with the basis that justifies it.
public struct Reliability: Codable, Equatable, Sendable {
    public let score: Double
    /// One of: operator_confirmed | direct_observation | muninn_trust |
    /// imported_authority | agent_inferred | unknown.
    public let basis: String

    public init(score: Double, basis: String) {
        self.score = score
        self.basis = basis
    }
}

/// One source backing a claim.
public struct SourceRef: Codable, Equatable, Sendable {
    public let sourceId: String
    /// One of: operator_confirmation | membrane_event | muninn_engram |
    /// graph_passage | imported_record | agent_inference | runtime_observation.
    public let sourceKind: String
    public let reliability: Reliability

    enum CodingKeys: String, CodingKey {
        case sourceId = "source_id"
        case sourceKind = "source_kind"
        case reliability
    }

    public init(sourceId: String, sourceKind: String, reliability: Reliability) {
        self.sourceId = sourceId
        self.sourceKind = sourceKind
        self.reliability = reliability
    }
}

/// The evidence packet describing one claim and its provenance.
public struct EvidencePacket: Codable, Equatable, Sendable {
    public let packetId: String
    public let claimRef: GraphRecordRef
    public let claimSummary: String
    public let sourceRefs: [SourceRef]
    public let confidence: Double
    /// One of: inferred | proposed | confirmed | retired | conflicted.
    public let validationState: String
    /// RFC3339 timestamp.
    public let observedAt: String
    public let sourceReliability: Double
    /// One of: not_needed | pending | muninn_first | graph_review |
    /// operator_required | resolved | rejected.
    public let adjudicationStatus: String
    /// Free-form structured payload (metric/unit/value for health metrics).
    public let metadata: [String: LifeJSONValue]

    enum CodingKeys: String, CodingKey {
        case packetId = "packet_id"
        case claimRef = "claim_ref"
        case claimSummary = "claim_summary"
        case sourceRefs = "source_refs"
        case confidence
        case validationState = "validation_state"
        case observedAt = "observed_at"
        case sourceReliability = "source_reliability"
        case adjudicationStatus = "adjudication_status"
        case metadata
    }

    public init(
        packetId: String,
        claimRef: GraphRecordRef,
        claimSummary: String,
        sourceRefs: [SourceRef],
        confidence: Double,
        validationState: String,
        observedAt: String,
        sourceReliability: Double,
        adjudicationStatus: String,
        metadata: [String: LifeJSONValue]
    ) {
        self.packetId = packetId
        self.claimRef = claimRef
        self.claimSummary = claimSummary
        self.sourceRefs = sourceRefs
        self.confidence = confidence
        self.validationState = validationState
        self.observedAt = observedAt
        self.sourceReliability = sourceReliability
        self.adjudicationStatus = adjudicationStatus
        self.metadata = metadata
    }
}

/// One observation the device offers the LifeGraph.
public struct LifeObservation: Codable, Equatable, Sendable {
    public let observationId: String
    public let evidence: EvidencePacket
    public let observedBy: String
    public let observedRole: String

    enum CodingKeys: String, CodingKey {
        case observationId = "observation_id"
        case evidence
        case observedBy = "observed_by"
        case observedRole = "observed_role"
    }

    public init(
        observationId: String,
        evidence: EvidencePacket,
        observedBy: String,
        observedRole: String
    ) {
        self.observationId = observationId
        self.evidence = evidence
        self.observedBy = observedBy
        self.observedRole = observedRole
    }
}

// MARK: - Observe response

/// One per-observation result from the observe endpoint. Shape is decoded
/// leniently (all fields optional) so a valid 200 never throws on a result
/// item whose exact keys differ from what we model here.
public struct ObserveResultItem: Codable, Equatable, Sendable {
    public let observationId: String?
    public let status: String?
    public let message: String?
    public let nodeId: String?

    enum CodingKeys: String, CodingKey {
        case observationId = "observation_id"
        case status
        case message
        case nodeId = "node_id"
    }

    private enum EnvelopeKeys: String, CodingKey { case result }

    public init(from decoder: Decoder) throws {
        let outer = try decoder.container(keyedBy: EnvelopeKeys.self)
        // life.observe.batch wraps each attempted write as {index, result}.
        // Retain decoding support for the older flat response fixtures.
        let c =
            outer.contains(.result)
            ? try outer.nestedContainer(keyedBy: CodingKeys.self, forKey: .result)
            : try decoder.container(keyedBy: CodingKeys.self)
        observationId = try c.decodeIfPresent(String.self, forKey: .observationId)
        status = try c.decodeIfPresent(String.self, forKey: .status)
        message = try c.decodeIfPresent(String.self, forKey: .message)
        nodeId = try c.decodeIfPresent(String.self, forKey: .nodeId)
    }

    public init(observationId: String?, status: String?, message: String?, nodeId: String?) {
        self.observationId = observationId
        self.status = status
        self.message = message
        self.nodeId = nodeId
    }
}

/// `POST /api/edge/lifegraph/observe` response.
public struct ObserveResult: Codable, Equatable, Sendable {
    /// "ok" | "partial" | "error".
    public let status: String
    public let results: [ObserveResultItem]

    public init(status: String, results: [ObserveResultItem]) {
        self.status = status
        self.results = results
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        status = (try? c.decode(String.self, forKey: .status)) ?? "unknown"
        results = (try? c.decodeIfPresent([ObserveResultItem].self, forKey: .results)) ?? []
    }

    enum CodingKeys: String, CodingKey {
        case status
        case results
    }
}
