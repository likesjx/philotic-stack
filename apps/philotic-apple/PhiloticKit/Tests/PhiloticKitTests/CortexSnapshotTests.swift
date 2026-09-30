import Foundation
import XCTest
@testable import PhiloticKit

final class CortexSnapshotTests: XCTestCase {
    private func snapshot(_ vaults: String, complete: Bool = true, exclusions: String = "[]") throws -> CortexSnapshot {
        let json = """
        {"cortex_id":"vps-jane","observed_at":"2026-09-19T15:00:00Z",
         "catalog_complete":\(complete),"vaults":\(vaults),"exclusions":\(exclusions)}
        """
        return try JSONDecoder().decode(CortexSnapshot.self, from: Data(json.utf8))
    }

    func testCompleteInventoryAllowsRealZero() throws {
        XCTAssertTrue(try snapshot(#"[{"id":"default","status":"available","memory_count":0}]"#).hasCompleteInventory)
    }

    func testUnavailableDeniedAndMissingCountsAreNotEmptyVaults() throws {
        for status in ["available", "unavailable", "denied"] {
            XCTAssertFalse(try snapshot(#"[{"id":"default","status":"\#(status)"}]"#).hasCompleteInventory)
        }
    }

    func testPartialCatalogAndExplicitExclusionsPreventCompleteClaim() throws {
        XCTAssertFalse(try snapshot("[]", complete: false).hasCompleteInventory)
        XCTAssertFalse(try snapshot("[]", exclusions: "[\"local session scratch\"]").hasCompleteInventory)
    }

    func testDuplicatesAndInvalidCountsPreventCompleteClaim() throws {
        let vault = #"{"id":"default","status":"available","memory_count":1}"#
        XCTAssertFalse(try snapshot("[\(vault),\(vault)]").hasCompleteInventory)
        XCTAssertFalse(try snapshot(#"[{"id":"default","status":"available","memory_count":-1}]"#).hasCompleteInventory)
    }

    func testUnknownStatusFailsClosed() {
        XCTAssertThrowsError(try snapshot(#"[{"id":"default","status":"maybe"}]"#))
    }

    func testMemoryIdentityIncludesVaultAndPreservesProvenance() throws {
        func memory(_ vault: String) throws -> CortexMemory {
            let json = """
            {"vault_id":"\(vault)","memory_id":"same-id","concept":"Test",
             "content":"Example only","state":"active","tags":["test"],"source":"operator"}
            """
            return try JSONDecoder().decode(CortexMemory.self, from: Data(json.utf8))
        }
        let first = try memory("default")
        XCTAssertNotEqual(first.id, try memory("self_agent-beacon").id)
        XCTAssertEqual(first.source, "operator")
        XCTAssertNil(first.createdAt)
    }
}
