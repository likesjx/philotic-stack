// BlobClient.swift
// Uploads raw blob bytes (e.g. a recorded voice message) to the hotel's
// edge blob endpoint (`POST /api/edge/blob`). The response decodes directly
// into the protocol-level `BlobRef` — the same shape `.turnSubmit` carries
// in `blob_refs` — so the returned ref rides the next turn submit as-is.
// Note: the returned `downloadUrl` is hotel-local; the app must never try
// to fetch it, it exists only so the hotel can resolve the blob server-side.

import Foundation

public struct BlobClient: Sendable {
    public enum BlobUploadError: Error, Equatable {
        case badResponse(status: Int)
        case malformedResponse
    }

    private let session: URLSession

    public init(session: URLSession = .shared) {
        self.session = session
    }

    /// Upload raw bytes as a blob using the edge bearer token. Returns the
    /// hotel's blob reference, ready to attach to a `.turnSubmit`.
    public func upload(
        baseURL: URL,
        bearerToken: String,
        data: Data,
        mimeType: String
    ) async throws -> BlobRef {
        var request = URLRequest(url: baseURL.appending(path: "api/edge/blob"))
        request.httpMethod = "POST"
        request.setValue("Bearer \(bearerToken)", forHTTPHeaderField: "Authorization")
        request.setValue(mimeType, forHTTPHeaderField: "Content-Type")

        let (body, response) = try await session.upload(for: request, from: data)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
            let status = (response as? HTTPURLResponse)?.statusCode ?? -1
            throw BlobUploadError.badResponse(status: status)
        }
        guard let ref = try? JSONDecoder().decode(BlobRef.self, from: body) else {
            throw BlobUploadError.malformedResponse
        }
        return ref
    }
}
