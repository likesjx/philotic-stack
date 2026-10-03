import Foundation

/// A projection of explicit tag membership, NOT Muninn association edges or
/// semantic search. Counts and members describe only the supplied loaded page.
public struct CortexTopic: Identifiable, Sendable {
    public var id: String { tag }
    public let tag: String
    public let members: [CortexMemory.Identity]

    public static func project(_ memories: [CortexMemory]) -> [CortexTopic] {
        var membership: [String: Set<CortexMemory.Identity>] = [:]
        for memory in memories {
            for tag in Set(memory.tags.map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }) where !tag.isEmpty {
                membership[tag, default: []].insert(memory.id)
            }
        }
        return membership.map { tag, ids in
            CortexTopic(tag: tag, members: ids.sorted {
                ($0.vaultID, $0.memoryID) < ($1.vaultID, $1.memoryID)
            })
        }.sorted {
            $0.members.count == $1.members.count ? $0.tag < $1.tag : $0.members.count > $1.members.count
        }
    }
}
