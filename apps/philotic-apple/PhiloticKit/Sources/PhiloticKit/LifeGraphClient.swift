// LifeGraphClient.swift
// Fetches the edge-scoped LifeGraph read plane (`/api/edge/lifegraph/*`):
// governed projections served by the life-graph-runner — named `life.recall`
// lenses, `life.view.node` detail, and `life.view.neighborhood` expansion.
// Devices never send Cypher; every response is policy-filtered server-side
// and every node carries its provenance envelope in `properties`.

import Foundation

/// Minimal JSON value for arbitrary node `properties` (the provenance
/// envelope rides here: validation_state, confidence, source_membrane,
/// observed_at, observed_by, provenance, …).
public enum LifeJSONValue: Codable, Equatable, Sendable {
    case string(String)
    case number(Double)
    case bool(Bool)
    case null
    case array([LifeJSONValue])
    case object([String: LifeJSONValue])

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode(Double.self) {
            self = .number(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode([LifeJSONValue].self) {
            self = .array(value)
        } else if let value = try? container.decode([String: LifeJSONValue].self) {
            self = .object(value)
        } else {
            throw DecodingError.dataCorruptedError(
                in: container, debugDescription: "unsupported JSON value")
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .string(let value): try container.encode(value)
        case .number(let value): try container.encode(value)
        case .bool(let value): try container.encode(value)
        case .null: try container.encodeNil()
        case .array(let value): try container.encode(value)
        case .object(let value): try container.encode(value)
        }
    }

    /// String rendering for display (numbers trimmed, bools spelled out).
    public var displayString: String? {
        switch self {
        case .string(let value): return value
        case .number(let value):
            return value == value.rounded() ? String(Int(value)) : String(value)
        case .bool(let value): return value ? "true" : "false"
        case .null, .array, .object: return nil
        }
    }
}

// MARK: - Lens (life.recall) shapes

/// Mirrors `GraphRecordRef`.
public struct LifeRecordRef: Codable, Equatable, Sendable {
    public let id: String
    public let label: String
}

/// Mirrors `EvidencePacket` — only the fields the UI renders.
public struct LifeEvidencePacket: Codable, Equatable, Sendable {
    public let packetId: String
    public let claimRef: LifeRecordRef
    public let claimSummary: String
    public let confidence: Double
    public let validationState: String
    public let observedAt: String?

    enum CodingKeys: String, CodingKey {
        case packetId = "packet_id"
        case claimRef = "claim_ref"
        case claimSummary = "claim_summary"
        case confidence
        case validationState = "validation_state"
        case observedAt = "observed_at"
    }
}

/// Mirrors `RankedEvidencePacket`.
public struct LifeRankedPacket: Codable, Equatable, Sendable, Identifiable {
    public let packet: LifeEvidencePacket
    public let score: Double

    public var id: String { packet.packetId }
}

/// Mirrors `RetrievalContextPacket` — only what the lens list needs.
public struct LifeContextPacket: Codable, Equatable, Sendable {
    public let contextId: String?
    public let generatedAt: String?
    public let rankedPackets: [LifeRankedPacket]

    enum CodingKeys: String, CodingKey {
        case contextId = "context_id"
        case generatedAt = "generated_at"
        case rankedPackets = "ranked_packets"
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        contextId = try container.decodeIfPresent(String.self, forKey: .contextId)
        generatedAt = try container.decodeIfPresent(String.self, forKey: .generatedAt)
        rankedPackets =
            (try? container.decodeIfPresent([LifeRankedPacket].self, forKey: .rankedPackets)) ?? []
    }
}

/// `data` of the lens response (the `life.recall` handler envelope).
public struct LifeLensData: Codable, Equatable, Sendable {
    public let status: String
    public let namedStrategy: String?
    public let fallbackUsed: Bool?
    public let contextPacket: LifeContextPacket?

    enum CodingKeys: String, CodingKey {
        case status
        case namedStrategy = "named_strategy"
        case fallbackUsed = "fallback_used"
        case contextPacket = "context_packet"
    }
}

/// `GET /api/edge/lifegraph/lens/:lens` response.
public struct LifeLensResponse: Codable, Equatable, Sendable {
    public let lens: String
    public let data: LifeLensData
}

// MARK: - Node / neighborhood (life.view.*) shapes

/// One graph node in Bolt row-JSON shape: labels + raw properties (the
/// provenance envelope lives in `properties`).
public struct LifeGraphNode: Codable, Equatable, Sendable {
    public let labels: [String]
    public let properties: [String: LifeJSONValue]

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        labels = (try? container.decodeIfPresent([String].self, forKey: .labels)) ?? []
        properties =
            (try? container.decodeIfPresent([String: LifeJSONValue].self, forKey: .properties))
            ?? [:]
    }

    enum CodingKeys: String, CodingKey {
        case labels
        case properties
    }

    public var canonicalId: String? { string("id") }
    public var primaryLabel: String? { labels.first }

    /// Convenience string property accessor.
    public func string(_ key: String) -> String? {
        properties[key]?.displayString
    }
}

/// One `life.view.node` neighbor row: typed edge + the neighbour node.
public struct LifeNodeNeighbor: Codable, Equatable, Sendable {
    public let relType: String
    public let fromId: String?
    public let toId: String?
    public let node: LifeGraphNode?

    enum CodingKeys: String, CodingKey {
        case relType = "rel_type"
        case fromId = "from_id"
        case toId = "to_id"
        case node
    }
}

/// `GET /api/edge/lifegraph/node/:id` response (`life.view.node` data).
public struct LifeNodeDetail: Codable, Equatable, Sendable {
    public let status: String
    public let id: String?
    public let node: LifeGraphNode?
    public let neighbors: [LifeNodeNeighbor]

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        status = (try? container.decode(String.self, forKey: .status)) ?? "unknown"
        id = try container.decodeIfPresent(String.self, forKey: .id)
        node = try? container.decodeIfPresent(LifeGraphNode.self, forKey: .node)
        neighbors =
            (try? container.decodeIfPresent([LifeNodeNeighbor].self, forKey: .neighbors)) ?? []
    }

    enum CodingKeys: String, CodingKey {
        case status
        case id
        case node
        case neighbors
    }
}

/// One undirected adjacency edge from `life.view.neighborhood`.
public struct LifeNeighborhoodEdge: Codable, Equatable, Sendable {
    public let from: String
    public let relType: String
    public let to: String

    enum CodingKeys: String, CodingKey {
        case from
        case relType = "rel_type"
        case to
    }
}

/// `GET /api/edge/lifegraph/neighborhood/:id` response.
public struct LifeNeighborhood: Codable, Equatable, Sendable {
    public let status: String
    public let originId: String?
    public let nodes: [LifeGraphNode]
    public let edges: [LifeNeighborhoodEdge]
    public let truncated: Bool?

    enum CodingKeys: String, CodingKey {
        case status
        case originId = "origin_id"
        case nodes
        case edges
        case truncated
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        status = (try? container.decode(String.self, forKey: .status)) ?? "unknown"
        originId = try container.decodeIfPresent(String.self, forKey: .originId)
        nodes = (try? container.decodeIfPresent([LifeGraphNode].self, forKey: .nodes)) ?? []
        edges = (try? container.decodeIfPresent([LifeNeighborhoodEdge].self, forKey: .edges)) ?? []
        truncated = try container.decodeIfPresent(Bool.self, forKey: .truncated)
    }
}

// MARK: - Client

public struct LifeGraphClient: Sendable {
    public enum LifeGraphError: Error, Equatable {
        case badResponse(status: Int)
    }

    private let session: URLSession

    public init(session: URLSession = .shared) {
        self.session = session
    }

    /// Fetch one named lens (`life.recall` retrieval strategy).
    public func fetchLens(
        baseURL: URL,
        bearerToken: String,
        lens: String,
        context: String? = nil,
        limit: UInt32? = nil
    ) async throws -> LifeLensResponse {
        var query: [URLQueryItem] = []
        if let context, !context.trimmingCharacters(in: .whitespaces).isEmpty {
            query.append(URLQueryItem(name: "context", value: context))
        }
        if let limit { query.append(URLQueryItem(name: "limit", value: String(limit))) }
        return try await get(
            baseURL: baseURL, path: "api/edge/lifegraph/lens/\(lens)",
            query: query, bearerToken: bearerToken)
    }

    /// Fetch one node's detail (`life.view.node`).
    public func fetchNode(
        baseURL: URL,
        bearerToken: String,
        nodeId: String,
        edgeLimit: UInt32? = nil
    ) async throws -> LifeNodeDetail {
        var query: [URLQueryItem] = []
        if let edgeLimit { query.append(URLQueryItem(name: "edge_limit", value: String(edgeLimit))) }
        return try await get(
            baseURL: baseURL, path: "api/edge/lifegraph/node/\(nodeId)",
            query: query, bearerToken: bearerToken)
    }

    /// Fetch a bounded neighborhood (`life.view.neighborhood`).
    public func fetchNeighborhood(
        baseURL: URL,
        bearerToken: String,
        nodeId: String,
        depth: UInt32? = nil,
        maxNodes: UInt32? = nil
    ) async throws -> LifeNeighborhood {
        var query: [URLQueryItem] = []
        if let depth { query.append(URLQueryItem(name: "depth", value: String(depth))) }
        if let maxNodes { query.append(URLQueryItem(name: "max_nodes", value: String(maxNodes))) }
        return try await get(
            baseURL: baseURL, path: "api/edge/lifegraph/neighborhood/\(nodeId)",
            query: query, bearerToken: bearerToken)
    }

    /// Maximum observations the server accepts in one observe batch.
    public static let maxObserveBatch = 25

    /// Push observations to the LifeGraph write plane
    /// (`POST /api/edge/lifegraph/observe`). Batches larger than
    /// ``maxObserveBatch`` are split across multiple POSTs and their results
    /// merged; the aggregate `status` is "error" if any batch errored, else
    /// "partial" if any batch was partial, else "ok". An empty input is a
    /// no-op that returns an "ok" result without hitting the network.
    ///
    /// Note: unlike the spec's bare `postObservations(_:)`, this mirrors the
    /// stateless read methods and takes `baseURL`/`bearerToken` explicitly
    /// (the client holds no connection state).
    @discardableResult
    public func postObservations(
        _ observations: [LifeObservation],
        baseURL: URL,
        bearerToken: String
    ) async throws -> ObserveResult {
        guard !observations.isEmpty else {
            return ObserveResult(status: "ok", results: [])
        }

        var merged: [ObserveResultItem] = []
        var sawError = false
        var sawPartial = false

        for start in stride(from: 0, to: observations.count, by: Self.maxObserveBatch) {
            let batch = Array(
                observations[start..<min(start + Self.maxObserveBatch, observations.count)])
            let result: ObserveResult = try await post(
                baseURL: baseURL,
                path: "api/edge/lifegraph/observe",
                body: ObserveRequest(observations: batch),
                bearerToken: bearerToken
            )
            merged.append(contentsOf: result.results)
            switch result.status {
            case "ok": break
            case "partial": sawPartial = true
            // Failed, invalid_request, unknown, and future statuses must not
            // become a successful health/location upload by default.
            default: sawError = true
            }
        }

        let status = sawError ? "error" : (sawPartial ? "partial" : "ok")
        return ObserveResult(status: status, results: merged)
    }

    /// Request envelope for the observe endpoint: `{ "observations": [...] }`.
    private struct ObserveRequest: Encodable {
        let observations: [LifeObservation]
    }

    private func post<Body: Encodable, T: Decodable>(
        baseURL: URL,
        path: String,
        body: Body,
        bearerToken: String
    ) async throws -> T {
        var request = URLRequest(url: baseURL.appending(path: path))
        request.httpMethod = "POST"
        request.setValue("Bearer \(bearerToken)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONEncoder().encode(body)
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
            let status = (response as? HTTPURLResponse)?.statusCode ?? -1
            throw LifeGraphError.badResponse(status: status)
        }
        return try JSONDecoder().decode(T.self, from: data)
    }

    private func get<T: Decodable>(
        baseURL: URL,
        path: String,
        query: [URLQueryItem],
        bearerToken: String
    ) async throws -> T {
        var components = URLComponents(
            url: baseURL.appending(path: path),
            resolvingAgainstBaseURL: false
        )
        if !query.isEmpty { components?.queryItems = query }
        guard let url = components?.url else {
            throw LifeGraphError.badResponse(status: -1)
        }
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.setValue("Bearer \(bearerToken)", forHTTPHeaderField: "Authorization")
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
            let status = (response as? HTTPURLResponse)?.statusCode ?? -1
            throw LifeGraphError.badResponse(status: status)
        }
        return try JSONDecoder().decode(T.self, from: data)
    }
}
