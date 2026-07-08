// EnrollmentClient.swift
// One-time device enrollment over plain HTTPS: POST /api/edge/enroll with an
// operator-issued invite code, receive back a node id + bearer edge token.

import Foundation

/// Errors surfaced by `EnrollmentClient`.
public enum EnrollmentError: Error, Equatable, Sendable {
    /// The server responded with a non-2xx status.
    case httpStatus(Int, body: String)
    /// The response body could not be decoded as `EnrollmentResponse`.
    case malformedResponse(String)
    /// The response was not an `HTTPURLResponse`.
    case notHTTPResponse
}

/// Performs one-time device enrollment against a hotel's `philotic-web`
/// endpoint.
public struct EnrollmentClient: Sendable {
    private let session: URLSession
    private let baseURL: URL

    /// - Parameters:
    ///   - baseURL: The hotel's `philotic-web` base URL (e.g.
    ///     "https://100.79.239.64:7700").
    ///   - session: `URLSession` to issue the enrollment request on. Inject a
    ///     session configured with a stub `URLProtocol` for tests.
    public init(baseURL: URL, session: URLSession = .shared) {
        self.baseURL = baseURL
        self.session = session
    }

    /// POSTs `/api/edge/enroll` with the given request and decodes the
    /// resulting bearer token + assigned node id.
    public func enroll(_ request: EnrollmentRequest) async throws -> EnrollmentResponse {
        var urlRequest = URLRequest(url: baseURL.appendingPathComponent("api/edge/enroll"))
        urlRequest.httpMethod = "POST"
        urlRequest.setValue("application/json", forHTTPHeaderField: "Content-Type")
        urlRequest.httpBody = try JSONEncoder().encode(request)

        let (data, response) = try await session.data(for: urlRequest)

        guard let http = response as? HTTPURLResponse else {
            throw EnrollmentError.notHTTPResponse
        }
        guard (200..<300).contains(http.statusCode) else {
            throw EnrollmentError.httpStatus(http.statusCode, body: String(data: data, encoding: .utf8) ?? "")
        }

        do {
            return try JSONDecoder().decode(EnrollmentResponse.self, from: data)
        } catch {
            throw EnrollmentError.malformedResponse(String(describing: error))
        }
    }
}
