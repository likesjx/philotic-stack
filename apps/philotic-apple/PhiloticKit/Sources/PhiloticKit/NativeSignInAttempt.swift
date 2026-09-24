import CryptoKit
import Foundation
import Security

/// Local-only handoff primitive, not a login client. No browser registration,
/// network request or gateway credential is introduced by this type.
@MainActor
public final class NativeSignInAttempt {
    public enum Client: String, Sendable {
        case mac = "com.philotic.apple.mac"
        case iOS = "com.philotic.apple.ios"
        public var callbackURL: URL { URL(string: "\(rawValue):/oauth/callback")! }
    }
    public enum Failure: Error { case entropyUnavailable, expiredOrConsumed, invalidCallback, cancelled }
    public struct Redemption: Sendable {
        public let code: String
        public let verifier: String
        public let redirectURI: String
        public let clientID: String
    }
    public let client: Client
    public let state: String
    public let challenge: String
    private let expiresAt: Date
    private var verifier: String?

    public init(client: Client, now: Date = Date()) throws {
        self.client = client
        let verifier = try Self.randomSecret()
        self.verifier = verifier
        state = try Self.randomSecret()
        challenge = Self.s256(verifier)
        expiresAt = now.addingTimeInterval(120)
    }

    public func cancel() { verifier = nil }

    /// Invalid callbacks also end the attempt. A caller must start fresh rather
    /// than repeatedly feeding an active attempt attacker-controlled responses.
    public func consume(_ url: URL, now: Date = Date()) throws -> Redemption {
        guard let verifier, now < expiresAt else {
            self.verifier = nil
            throw Failure.expiredOrConsumed
        }
        self.verifier = nil
        guard url.absoluteString.utf8.count <= 4096,
              let parts = URLComponents(url: url, resolvingAgainstBaseURL: false),
              parts.scheme == client.rawValue, parts.host == nil,
              parts.user == nil, parts.password == nil, parts.port == nil,
              parts.percentEncodedPath == "/oauth/callback", parts.fragment == nil,
              let items = parts.queryItems,
              items.count == 2, Set(items.map(\.name)).count == 2,
              items.first(where: { $0.name == "state" })?.value == state else {
            throw Failure.invalidCallback
        }
        if items.first(where: { $0.name == "error" })?.value == "access_denied" {
            throw Failure.cancelled
        }
        guard let code = items.first(where: { $0.name == "code" })?.value,
              (32...512).contains(code.utf8.count),
              code.utf8.allSatisfy({ (65...90).contains($0) || (97...122).contains($0)
                  || (48...57).contains($0) || $0 == 45 || $0 == 95 }) else {
            throw Failure.invalidCallback
        }
        return Redemption(code: code, verifier: verifier,
            redirectURI: client.callbackURL.absoluteString, clientID: client.rawValue)
    }

    static func s256(_ verifier: String) -> String {
        base64URL(Data(SHA256.hash(data: Data(verifier.utf8))))
    }
    private static func randomSecret() throws -> String {
        var bytes = [UInt8](repeating: 0, count: 32)
        guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else {
            throw Failure.entropyUnavailable
        }
        return base64URL(Data(bytes))
    }
    private static func base64URL(_ data: Data) -> String {
        data.base64EncodedString().replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
    }
}
