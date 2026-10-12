// MemoryClient.swift
// Edge-scoped structured Muninn recall (`GET /api/edge/memory/recall`),
// the read side of extending the Spotlight entity index to Muninn
// (seam: apple-entity-index-plane, Muninn extension).
//
// Deliberately backed by `memory.recall.structured`, not `memory.recall`:
// the latter renders engrams to "[id] concept: content" for humans, which
// discards the trust tier and soft-delete marker. Those are exactly the
// fields that decide whether a memory may be donated to a *system* index,
// so the human rendering is unusable here.

import Foundation

public enum MemoryError: Error, Equatable, Sendable {
    case badResponse(status: Int)
    case unavailable(String)
}

/// One engram as returned by `memory.recall.structured`.
///
/// `trust` mirrors `TrustTier` in ansible-mesh-core (`observed` / `inferred` /
/// `told`), or the literal `"unknown"` when the stored memory predates the
/// provenance envelope. Standing Rule 2 of the Memory Transparency proposal:
/// a tier is never silently upgraded, so `unknown` stays `unknown` here and is
/// resolved by policy, not by guessing.
public struct MuninnMemory: Codable, Equatable, Sendable, Identifiable {
    public let id: String
    public let vaultId: String?
    public let concept: String
    public let content: String
    public let tags: [String]
    public let confidence: Double
    public let createdAt: UInt64?
    public let updatedAt: UInt64?
    public let trust: String
    public let deleted: Bool

    enum CodingKeys: String, CodingKey {
        case id
        case vaultId = "vault_id"
        case concept
        case content
        case tags
        case confidence
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case trust
        case deleted
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = (try? c.decode(String.self, forKey: .id)) ?? ""
        vaultId = try? c.decodeIfPresent(String.self, forKey: .vaultId)
        concept = (try? c.decode(String.self, forKey: .concept)) ?? ""
        content = (try? c.decode(String.self, forKey: .content)) ?? ""
        tags = (try? c.decodeIfPresent([String].self, forKey: .tags)) ?? []
        confidence = (try? c.decode(Double.self, forKey: .confidence)) ?? 0
        createdAt = try? c.decodeIfPresent(UInt64.self, forKey: .createdAt)
        updatedAt = try? c.decodeIfPresent(UInt64.self, forKey: .updatedAt)
        // A missing trust field is not an error — it is the gap this plane
        // exists to make visible. Default to "unknown" so policy withholds it.
        trust = (try? c.decodeIfPresent(String.self, forKey: .trust)) ?? "unknown"
        deleted = (try? c.decodeIfPresent(Bool.self, forKey: .deleted)) ?? false
    }
}

/// `GET /api/edge/memory/recall` response.
public struct MuninnRecallResponse: Codable, Equatable, Sendable {
    public let status: String
    public let total: Int?
    public let memories: [MuninnMemory]

    enum CodingKeys: String, CodingKey {
        case status
        case total
        case memories
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        status = (try? c.decode(String.self, forKey: .status)) ?? "unknown"
        total = try? c.decodeIfPresent(Int.self, forKey: .total)
        memories = (try? c.decodeIfPresent([MuninnMemory].self, forKey: .memories)) ?? []
    }
}

public struct MemoryClient: Sendable {
    private let session: URLSession

    public init(session: URLSession = .shared) {
        self.session = session
    }

    public func recall(
        baseURL: URL,
        bearerToken: String,
        context: String? = nil,
        maxResults: UInt32? = nil
    ) async throws -> MuninnRecallResponse {
        var query: [URLQueryItem] = []
        if let context, !context.trimmingCharacters(in: .whitespaces).isEmpty {
            query.append(URLQueryItem(name: "context", value: context))
        }
        if let maxResults {
            query.append(URLQueryItem(name: "max_results", value: String(maxResults)))
        }

        var components = URLComponents(
            url: baseURL.appending(path: "api/edge/memory/recall"),
            resolvingAgainstBaseURL: false
        )
        if !query.isEmpty { components?.queryItems = query }
        guard let url = components?.url else {
            throw MemoryError.badResponse(status: -1)
        }
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.setValue("Bearer \(bearerToken)", forHTTPHeaderField: "Authorization")
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
            let status = (response as? HTTPURLResponse)?.statusCode ?? -1
            throw MemoryError.badResponse(status: status)
        }
        return try JSONDecoder().decode(MuninnRecallResponse.self, from: data)
    }
}
