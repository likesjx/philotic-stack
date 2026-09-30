import Foundation
import XCTest
@testable import PhiloticKit

final class CortexClientTests: XCTestCase {
    func testTokenGoesOnlyToPinnedPrivateOrigin() throws {
        let request = try CortexClient.request(token: "test-secret")
        XCTAssertEqual(request.url?.absoluteString, "http://100.64.212.8:7700/api/cortex")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer test-secret")
        XCTAssertFalse(request.url!.absoluteString.contains("test-secret"))
        XCTAssertEqual(request.cachePolicy, .reloadIgnoringLocalCacheData)
    }

    func testQueryCannotChangeOriginOrInjectParameters() throws {
        let value = "../vault?host=evil.example&token=other#fragment"
        let request = try CortexClient.request(token: "test", vault: value, memory: "id", cursor: "50")
        let parts = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)!
        XCTAssertEqual(parts.host, "100.64.212.8")
        XCTAssertEqual(parts.path, "/api/cortex")
        XCTAssertNil(parts.fragment)
        XCTAssertEqual(parts.queryItems?.count, 3)
        XCTAssertEqual(parts.queryItems?.first?.value, value)
    }

    func testMissingCredentialFailsBeforeRequest() {
        XCTAssertThrowsError(try CortexClient.request(token: ""))
    }
}
