import Foundation

/// Native Cortex credentials have a distinct audience from hotel and device
/// credentials. Only this fixed HTTPS gateway receives them; redirects fail.
public final class NativeCortexClient: Sendable {
    public static let origin = URL(string: "https://desktop.jaredlikes.com")!
    private final class NoRedirect: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
        func urlSession(_ session: URLSession, task: URLSessionTask,
            willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
            completionHandler: @escaping @Sendable (URLRequest?) -> Void) { completionHandler(nil) }
    }
    private let session: URLSession
    public init() {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.urlCache = nil; configuration.httpCookieStorage = nil
        configuration.timeoutIntervalForRequest = 50
        session = URLSession(configuration: configuration, delegate: NoRedirect(), delegateQueue: nil)
    }
    public enum Failure: Error, LocalizedError {
        case signInRequired, unavailable, invalidResponse
        public var errorDescription: String? {
            switch self {
            case .signInRequired: "Sign in again to view Cortex. Your account needs administrator access and admission to Philotic."
            case .unavailable: "Cortex sign-in is unavailable. Please try again shortly."
            case .invalidResponse: "Cortex returned an unexpected response. Please try signing in again."
            }
        }
    }
    public static func validToken(_ token: String) -> Bool {
        token.hasPrefix("native-cortex-") && token.count == 57 && token.dropFirst(14).utf8.allSatisfy {
            (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95
        }
    }
    public static func request(token: String, vault: String? = nil, memory: String? = nil, cursor: String? = nil) throws -> URLRequest {
        guard validToken(token) else { throw Failure.signInRequired }
        var url = URLComponents(url: origin, resolvingAgainstBaseURL: false)!
        url.path = "/native/cortex"
        var query: [URLQueryItem] = []
        if let vault { query.append(.init(name: "vault", value: vault)) }
        if let memory { query.append(.init(name: "id", value: memory)) }
        if let cursor { query.append(.init(name: "offset", value: cursor)) }
        url.queryItems = query.isEmpty ? nil : query
        var request = URLRequest(url: url.url!, cachePolicy: .reloadIgnoringLocalCacheData)
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        return request
    }
    private func data(for request: URLRequest, limit: Int) async throws -> Data {
        let (bytes, response) = try await session.bytes(for: request)
        guard let http = response as? HTTPURLResponse else { throw Failure.invalidResponse }
        if [401, 403].contains(http.statusCode) { throw Failure.signInRequired }
        guard http.statusCode == 200 else { throw Failure.unavailable }
        var data = Data()
        for try await byte in bytes {
            guard data.count < limit else { throw Failure.invalidResponse }
            data.append(byte)
        }
        return data
    }
    public func exchange(_ redemption: NativeSignInAttempt.Redemption) async throws -> String {
        var request = URLRequest(url: Self.origin.appendingPathComponent("native-auth/exchange"))
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: ["code": redemption.code,
            "verifier": redemption.verifier, "client_id": redemption.clientID, "redirect_uri": redemption.redirectURI])
        struct Response: Decodable { let access_token: String; let token_type: String; let scope: String; let expires_in: Int }
        let response = try JSONDecoder().decode(Response.self, from: await data(for: request, limit: 4096))
        guard Self.validToken(response.access_token), response.token_type == "Bearer", response.scope == "cortex:read",
              (1...900).contains(response.expires_in) else { throw Failure.invalidResponse }
        return response.access_token
    }
    public func logout(token: String) async {
        guard Self.validToken(token) else { return }
        var request = URLRequest(url: Self.origin.appendingPathComponent("native-auth/logout"))
        request.httpMethod = "POST"; request.timeoutInterval = 5
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        _ = try? await data(for: request, limit: 4096)
    }
    public func read<T: Decodable & Sendable>(_ type: T.Type, token: String,
        vault: String? = nil, memory: String? = nil, cursor: String? = nil) async throws -> T {
        let data = try await data(for: Self.request(token: token, vault: vault, memory: memory, cursor: cursor), limit: 4 * 1024 * 1024)
        do { return try JSONDecoder().decode(type, from: data) }
        catch { throw Failure.invalidResponse }
    }
}
