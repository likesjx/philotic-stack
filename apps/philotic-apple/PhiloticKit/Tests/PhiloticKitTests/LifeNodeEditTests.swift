import XCTest
@testable import PhiloticKit

final class LifeNodeEditTests: XCTestCase {
    override func tearDown() { StubURLProtocol.responder = nil; super.tearDown() }
    private func node() throws -> LifeGraphNode {
        try JSONDecoder().decode(LifeGraphNode.self, from: Data(#"{"labels":["Goal"],"properties":{"id":"life:goal:test","title":"Old","confidence":0.8}}"#.utf8))
    }
    func testDraftIncludesOnlyChangedEditableFieldsAndNullOriginals() throws {
        let edit = LifeNodeEdit(node: try node(), values: ["title": "New", "description": "Details", "confidence": "1"])
        XCTAssertEqual(edit.changes, ["title": "New", "description": "Details"])
        XCTAssertEqual(edit.before["title"], .string("Old"))
        XCTAssertEqual(edit.before["description"], .null)
        let encoded = try JSONSerialization.jsonObject(with: JSONEncoder().encode(edit)) as! [String: Any]
        XCTAssertTrue((encoded["before"] as! [String: Any])["description"] is NSNull)
    }
    func testUnchangedDraftIsEmpty() throws {
        XCTAssertTrue(LifeNodeEdit(node: try node(), values: ["title": "Old", "description": ""]).changes.isEmpty)
    }
    func testPatchRequiresMatchingAuditReceipt() async throws {
        StubURLProtocol.responder = { request in
            XCTAssertEqual(request.httpMethod, "PATCH")
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer device-token")
            XCTAssertEqual(request.url?.path, "/api/edge/lifegraph/node/life:goal:test")
            return .init(body: Data(#"{"status":"saved","node_id":"life:goal:test","audit_id":"node-edit:one"}"#.utf8))
        }
        let receipt = try await LifeGraphClient(session: .stubbed()).editNode(
            baseURL: URL(string: "https://hotel.example")!, bearerToken: "device-token", nodeId: "life:goal:test",
            edit: LifeNodeEdit(node: try node(), values: ["title": "New"]))
        XCTAssertEqual(receipt.auditId, "node-edit:one")
    }
    func testConflictAndMissingReceiptAreNotSuccess() async throws {
        for status in [409, 200, 405] {
            StubURLProtocol.responder = { _ in .init(statusCode: status, body: Data("{}".utf8)) }
            do {
                _ = try await LifeGraphClient(session: .stubbed()).editNode(
                    baseURL: URL(string: "https://hotel.example")!, bearerToken: "token", nodeId: "life:goal:test",
                    edit: LifeNodeEdit(node: try node(), values: ["title": "New"]))
                XCTFail("HTTP \(status) without valid receipt must not succeed")
            } catch { XCTAssertTrue(error is LifeGraphClient.EditError) }
        }
    }
}
