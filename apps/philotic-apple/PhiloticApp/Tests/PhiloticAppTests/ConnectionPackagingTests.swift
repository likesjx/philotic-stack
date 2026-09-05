import Foundation
import XCTest

#if os(iOS)
final class ConnectionPackagingTests: XCTestCase {
    func testInstalledAppLimitsHTTPExceptionToPrivateHotel() throws {
        // Inspect the host app, not the test bundle or the project source:
        // the previous build setting silently omitted ATS from Info.plist.
        let ats = try XCTUnwrap(
            Bundle.main.object(forInfoDictionaryKey: "NSAppTransportSecurity") as? [String: Any])
        XCTAssertNil(ats["NSAllowsArbitraryLoads"])
        XCTAssertNil(ats["NSAllowsLocalNetworking"])
        let domains = try XCTUnwrap(ats["NSExceptionDomains"] as? [String: [String: Any]])
        XCTAssertEqual(Set(domains.keys), ["100.64.212.8"])
        XCTAssertEqual(domains["100.64.212.8"]?["NSExceptionAllowsInsecureHTTPLoads"] as? Bool, true)
        XCTAssertNil(domains["100.64.212.8"]?["NSIncludesSubdomains"])
        XCTAssertNotNil(Bundle.main.object(forInfoDictionaryKey: "NSLocalNetworkUsageDescription"))
    }
}
#endif
