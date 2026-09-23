import Foundation

/// Transitional private Cortex origin. Credentials cannot be sent to arbitrary
/// URLs, redirected hosts, or the public desktop. Replace this pin only with an
/// explicit trusted-origin operator sign-in design.
public final class CortexClient: Sendable {
    public static let origin = URL(string: "http://100.64.212.8:7700")!
    private final class NoRedirect: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
        func urlSession(_ session: URLSession, task: URLSessionTask,
            willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
            completionHandler: @escaping @Sendable (URLRequest?) -> Void) { completionHandler(nil) }
    }
    private let session: URLSession
    public init() {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.urlCache = nil
        configuration.httpCookieStorage = nil
        configuration.timeoutIntervalForRequest = 45
        session = URLSession(configuration: configuration, delegate: NoRedirect(), delegateQueue: nil)
    }
    public enum Failure: Error, LocalizedError {
        case authorizationRequired, unavailable, invalidResponse
        public var errorDescription: String? {
            switch self {
            case .authorizationRequired: "An active administrator operator session is required. Device enrollment tokens do not grant Cortex access."
            case .unavailable: "Cortex is unavailable. Connect to Tailscale and check the hotel's memory configuration."
            case .invalidResponse: "The hotel returned an unsupported Cortex response."
            }
        }
    }
    public static func request(token: String, vault: String? = nil,
        memory: String? = nil, cursor: String? = nil) throws -> URLRequest {
        guard !token.isEmpty else { throw Failure.authorizationRequired }
        var parts = URLComponents(url: origin, resolvingAgainstBaseURL: false)!
        parts.path = "/api/cortex"
        var query: [URLQueryItem] = []
        if let vault { query.append(URLQueryItem(name: "vault", value: vault)) }
        if let memory { query.append(URLQueryItem(name: "id", value: memory)) }
        if let cursor { query.append(URLQueryItem(name: "offset", value: cursor)) }
        parts.queryItems = query.isEmpty ? nil : query
        var request = URLRequest(url: parts.url!, cachePolicy: .reloadIgnoringLocalCacheData)
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        return request
    }
    public func read<T: Decodable & Sendable>(_ type: T.Type, token: String,
        vault: String? = nil, memory: String? = nil, cursor: String? = nil) async throws -> T {
        let (bytes, response) = try await session.bytes(for: Self.request(token: token, vault: vault, memory: memory, cursor: cursor))
        guard let response = response as? HTTPURLResponse else { throw Failure.invalidResponse }
        if [401, 403].contains(response.statusCode) { throw Failure.authorizationRequired }
        guard response.statusCode == 200 else { throw Failure.unavailable }
        var data = Data()
        for try await byte in bytes {
            guard data.count < 4 * 1024 * 1024 else { throw Failure.invalidResponse }
            data.append(byte)
        }
        do { return try JSONDecoder().decode(type, from: data) }
        catch { throw Failure.invalidResponse }
    }
}
