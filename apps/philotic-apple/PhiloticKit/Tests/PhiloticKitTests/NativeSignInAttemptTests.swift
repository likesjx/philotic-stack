import Foundation
import XCTest
@testable import PhiloticKit

final class NativeSignInAttemptTests: XCTestCase {
    @MainActor func testRFC7636Challenge() {
        XCTAssertEqual(NativeSignInAttempt.s256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM")
    }

    @MainActor func testFreshSecretsAndSingleConsumption() throws {
        for client in [NativeSignInAttempt.Client.mac, .iOS] {
            let attempt = try NativeSignInAttempt(client: client)
            let other = try NativeSignInAttempt(client: client)
            XCTAssertEqual(attempt.state.count, 43)
            XCTAssertEqual(attempt.challenge.count, 43)
            XCTAssertNotEqual(attempt.state, other.state)
            XCTAssertNotEqual(attempt.challenge, other.challenge)
            let callback = URL(string: "\(client.callbackURL)?state=\(attempt.state)&code=\(String(repeating: "a", count: 43))")!
            let result = try attempt.consume(callback)
            XCTAssertEqual(NativeSignInAttempt.s256(result.verifier), attempt.challenge)
            XCTAssertEqual(result.clientID, client.rawValue)
            XCTAssertThrowsError(try attempt.consume(callback))
        }
    }

    @MainActor func testRejectsWrongOriginStateAndAmbiguousCallbacks() throws {
        for suffix in ["&code=extra", "#fragment", "&access_token=secret"] {
            let attempt = try NativeSignInAttempt(client: .mac)
            let url = URL(string: "com.philotic.apple.mac:/oauth/callback?state=\(attempt.state)&code=\(String(repeating: "a", count: 43))\(suffix)")!
            XCTAssertThrowsError(try attempt.consume(url))
        }
        for prefix in ["https://evil.example/oauth/callback", "com.philotic.apple.mac://host/oauth/callback",
                       "com.philotic.apple.ios:/oauth/callback", "com.philotic.apple.mac:/oauth/%63allback"] {
            let attempt = try NativeSignInAttempt(client: .mac)
            XCTAssertThrowsError(try attempt.consume(URL(string: "\(prefix)?state=\(attempt.state)&code=\(String(repeating: "a", count: 43))")!))
        }
        let attempt = try NativeSignInAttempt(client: .mac)
        XCTAssertThrowsError(try attempt.consume(URL(string: "com.philotic.apple.mac:/oauth/callback?state=wrong&code=\(String(repeating: "a", count: 43))")!))
    }

    @MainActor func testExpiryAndCancellation() throws {
        let start = Date(timeIntervalSince1970: 1_000)
        let attempt = try NativeSignInAttempt(client: .mac, now: start)
        let callback = URL(string: "com.philotic.apple.mac:/oauth/callback?state=\(attempt.state)&code=\(String(repeating: "a", count: 43))")!
        XCTAssertThrowsError(try attempt.consume(callback, now: start.addingTimeInterval(120)))
        let cancelled = try NativeSignInAttempt(client: .mac)
        cancelled.cancel()
        XCTAssertThrowsError(try cancelled.consume(callback))
        let denied = try NativeSignInAttempt(client: .mac)
        XCTAssertThrowsError(try denied.consume(URL(string: "com.philotic.apple.mac:/oauth/callback?state=\(denied.state)&error=access_denied")!)) { error in
            guard case NativeSignInAttempt.Failure.cancelled = error else { return XCTFail("Wrong error") }
        }
    }
}
