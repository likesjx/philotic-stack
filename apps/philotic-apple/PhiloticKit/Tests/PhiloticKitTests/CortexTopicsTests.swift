import Foundation
import XCTest
@testable import PhiloticKit

final class CortexTopicsTests: XCTestCase {
    private func memory(_ vault: String, _ id: String, tags: [String]) throws -> CortexMemory {
        let data = try JSONSerialization.data(withJSONObject: ["vault_id": vault, "memory_id": id,
            "concept": "Example", "content": "Fixture", "state": "active", "tags": tags])
        return try JSONDecoder().decode(CortexMemory.self, from: data)
    }

    func testExplicitMembershipDeduplicatesWithoutMergingVaults() throws {
        let a = try memory("a", "same", tags: ["music", "music", "  "])
        let b = try memory("b", "same", tags: ["music", "piano"])
        let topics = CortexTopic.project([a, a, b])
        XCTAssertEqual(topics.map(\.tag), ["music", "piano"])
        XCTAssertEqual(topics[0].members, [a.id, b.id])
        XCTAssertEqual(topics[1].members, [b.id])
    }

    func testStableOrderingAndNoInventedConnections() throws {
        let a = try memory("a", "1", tags: ["z", "a"])
        let b = try memory("a", "2", tags: [])
        XCTAssertEqual(CortexTopic.project([a, b]).map(\.tag), ["a", "z"])
        XCTAssertTrue(CortexTopic.project([b]).isEmpty)
    }
}
