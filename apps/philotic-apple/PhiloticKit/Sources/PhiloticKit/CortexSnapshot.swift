import Foundation

/// Proposed read-only Cortex viewer contract. Not a projection of agent recall:
/// the server must inventory the operator-authorized vaults explicitly.
/// No transport is enabled until the operator-authorized gateway is implemented.
public struct CortexSnapshot: Decodable, Sendable {
    public let cortexID: String
    public let observedAt: String
    public let catalogComplete: Bool
    public let vaults: [CortexVault]
    public let exclusions: [String]

    enum CodingKeys: String, CodingKey {
        case cortexID = "cortex_id", observedAt = "observed_at"
        case catalogComplete = "catalog_complete"
        case vaults, exclusions
    }

    /// Only describes the authorized Cortex inventory, never all Philotic state.
    /// Missing counts, duplicate vault identities or exclusions cannot become green.
    public var hasCompleteInventory: Bool {
        !cortexID.isEmpty && !observedAt.isEmpty && catalogComplete
            && exclusions.isEmpty
            && Set(vaults.map(\.id)).count == vaults.count
            && vaults.allSatisfy {
                !$0.id.isEmpty && $0.status == .available
                    && $0.memoryCount.map { $0 >= 0 } == true
            }
    }
}

public struct CortexVault: Decodable, Identifiable, Sendable {
    public enum Status: String, Decodable, Sendable {
        case available, unavailable, denied
    }

    public let id: String
    public let status: Status
    public let memoryCount: Int?
    public let reason: String?

    enum CodingKeys: String, CodingKey {
        case id, status, reason
        case memoryCount = "memory_count"
    }
}

/// Bounded pages retain vault identity. Recall scores are not inventory counts,
/// and an exhausted page is not evidence that every vault was queried.
public struct CortexMemoryPage: Decodable, Sendable {
    public let cortexID: String
    public let vaultID: String
    public let observedAt: String
    public let memories: [CortexMemory]
    public let nextCursor: String?

    enum CodingKeys: String, CodingKey {
        case cortexID = "cortex_id", vaultID = "vault_id", observedAt = "observed_at"
        case memories, nextCursor = "next_cursor"
    }
}

public struct CortexMemory: Decodable, Identifiable, Sendable {
    /// Composite identity prevents accidentally merging equal IDs across vaults.
    public struct Identity: Hashable, Sendable {
        public let vaultID: String
        public let memoryID: String
    }
    public var id: Identity { Identity(vaultID: vaultID, memoryID: memoryID) }
    public let vaultID: String
    public let memoryID: String
    public let concept: String
    public let content: String
    public let state: String
    public let tags: [String]
    public let source: String?
    public let createdAt: String?
    public let updatedAt: String?

    enum CodingKeys: String, CodingKey {
        case vaultID = "vault_id", memoryID = "memory_id"
        case concept, content, state, tags, source
        case createdAt = "created_at", updatedAt = "updated_at"
    }
}
