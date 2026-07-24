// BlobClientTests.swift
// Exercises BlobClient's POST /api/edge/blob against StubURLProtocol —
// no live server involved.

import XCTest

@testable import PhiloticKit

final class BlobClientTests: XCTestCase {
    override func tearDown() {
        StubURLProtocol.responder = nil
        super.tearDown()
    }

    func testSuccessfulUploadDecodesBlobRef() async throws {
        let responseJSON = """
            {"blob_id":"sha256-9f2a","download_url":"http://127.0.0.1:9001/download/sha256-9f2a","mime":"audio/mp4"}
            """

        let capturedPath = Locked<String?>(nil)
        let capturedMethod = Locked<String?>(nil)
        let capturedAuth = Locked<String?>(nil)
        let capturedContentType = Locked<String?>(nil)
        let capturedBody = Locked<Data?>(nil)
        StubURLProtocol.responder = { request in
            capturedPath.value = request.url?.path
            capturedMethod.value = request.httpMethod
            capturedAuth.value = request.value(forHTTPHeaderField: "Authorization")
            capturedContentType.value = request.value(forHTTPHeaderField: "Content-Type")
            capturedBody.value = StubURLProtocol.resolvedBody(for: request)
            return .init(statusCode: 200, body: Data(responseJSON.utf8))
        }

        let audioBytes = Data([0x00, 0x01, 0x02, 0xff, 0xfe])
        let client = BlobClient(session: .stubbed())
        let ref = try await client.upload(
            baseURL: URL(string: "https://hotel.example")!,
            bearerToken: "tok-edge",
            data: audioBytes,
            mimeType: "audio/mp4"
        )

        XCTAssertEqual(ref.blobId, "sha256-9f2a")
        XCTAssertEqual(ref.downloadUrl, "http://127.0.0.1:9001/download/sha256-9f2a")
        XCTAssertEqual(ref.mime, "audio/mp4")

        XCTAssertEqual(capturedPath.value, "/api/edge/blob")
        XCTAssertEqual(capturedMethod.value, "POST")
        XCTAssertEqual(capturedAuth.value, "Bearer tok-edge")
        XCTAssertEqual(capturedContentType.value, "audio/mp4")
        XCTAssertEqual(capturedBody.value, audioBytes)
    }

    func testHttpErrorStatusThrows() async {
        StubURLProtocol.responder = { _ in .init(statusCode: 413, body: Data("too large".utf8)) }

        let client = BlobClient(session: .stubbed())
        do {
            _ = try await client.upload(
                baseURL: URL(string: "https://hotel.example")!,
                bearerToken: "tok-edge",
                data: Data([0x01]),
                mimeType: "audio/mp4"
            )
            XCTFail("expected upload to throw")
        } catch let BlobClient.BlobUploadError.badResponse(status) {
            XCTAssertEqual(status, 413)
        } catch {
            XCTFail("unexpected error: \(error)")
        }
    }

    func testMalformedResponseBodyThrows() async {
        StubURLProtocol.responder = { _ in .init(statusCode: 200, body: Data("not json".utf8)) }

        let client = BlobClient(session: .stubbed())
        do {
            _ = try await client.upload(
                baseURL: URL(string: "https://hotel.example")!,
                bearerToken: "tok-edge",
                data: Data([0x01]),
                mimeType: "audio/mp4"
            )
            XCTFail("expected upload to throw")
        } catch BlobClient.BlobUploadError.malformedResponse {
            // expected
        } catch {
            XCTFail("unexpected error: \(error)")
        }
    }
}
