import Foundation
import XCTest
@testable import PhiloticKit

final class NativeCortexClientTests: XCTestCase {
    func testNativeAudienceAndPinnedOrigin() throws {
        let token = "native-cortex-" + String(repeating: "a", count: 43)
        let request = try NativeCortexClient.request(token: token, vault: "a&b", memory: "m", cursor: "50")
        XCTAssertEqual(request.url?.scheme, "https")
        XCTAssertEqual(request.url?.host, "desktop.jaredlikes.com")
        XCTAssertEqual(request.url?.path, "/native/cortex")
        XCTAssertEqual(URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems?.first?.value, "a&b")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer \(token)")
        for invalid in ["", "operator-token-secret", "edge-tok-secret", token + "\n", String(repeating: "a", count: 43)] {
            XCTAssertThrowsError(try NativeCortexClient.request(token: invalid))
        }
    }
    @MainActor func testAuthorizationURLContainsOnlyPublicChallenge() throws {
        let attempt = try NativeSignInAttempt(client: .iOS)
        let url = URLComponents(url: attempt.authorizationURL, resolvingAgainstBaseURL: false)!
        XCTAssertEqual(url.host, "desktop.jaredlikes.com")
        XCTAssertEqual(url.path, "/native-auth/start")
        XCTAssertEqual(Set(url.queryItems!.map(\.name)), ["client_id", "redirect_uri", "state", "code_challenge"])
        XCTAssertEqual(url.queryItems?.first(where: { $0.name == "code_challenge" })?.value, attempt.challenge)
    }
}
