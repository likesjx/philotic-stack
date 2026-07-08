// EnrollmentClientTests.swift
// Exercises EnrollmentClient's POST /api/edge/enroll against
// StubURLProtocol — no live server involved.

import XCTest

@testable import PhiloticKit

final class EnrollmentClientTests: XCTestCase {
    override func tearDown() {
        StubURLProtocol.responder = nil
        super.tearDown()
    }

    func testSuccessfulEnrollmentDecodesResponse() async throws {
        let expected = EnrollmentResponse(nodeId: "edge-abc123", edgeToken: "tok-secret")
        let body = try JSONEncoder().encode(expected)

        let capturedPath = Locked<String?>(nil)
        let capturedBody = Locked<Data?>(nil)
        StubURLProtocol.responder = { request in
            capturedPath.value = request.url?.path
            capturedBody.value = StubURLProtocol.resolvedBody(for: request)
            return .init(statusCode: 200, body: body)
        }

        let client = EnrollmentClient(baseURL: URL(string: "https://hotel.example")!, session: .stubbed())
        let request = EnrollmentRequest(
            inviteCode: "INV-1234",
            devicePubkeyB64: "cHVia2V5",
            deviceName: "Jared's iPhone",
            platform: "ios"
        )
        let response = try await client.enroll(request)

        XCTAssertEqual(response, expected)
        XCTAssertEqual(capturedPath.value, "/api/edge/enroll")

        let sentRequest = try JSONDecoder().decode(EnrollmentRequest.self, from: capturedBody.value ?? Data())
        XCTAssertEqual(sentRequest, request)
    }

    func testHttpErrorStatusThrows() async {
        StubURLProtocol.responder = { _ in
            .init(statusCode: 403, body: Data("invalid invite code".utf8))
        }

        let client = EnrollmentClient(baseURL: URL(string: "https://hotel.example")!, session: .stubbed())
        let request = EnrollmentRequest(
            inviteCode: "bad",
            devicePubkeyB64: "cHVia2V5",
            deviceName: "device",
            platform: "ios"
        )

        do {
            _ = try await client.enroll(request)
            XCTFail("expected enroll to throw")
        } catch let EnrollmentError.httpStatus(status, body) {
            XCTAssertEqual(status, 403)
            XCTAssertEqual(body, "invalid invite code")
        } catch {
            XCTFail("unexpected error: \(error)")
        }
    }

    func testMalformedResponseBodyThrows() async {
        StubURLProtocol.responder = { _ in .init(statusCode: 200, body: Data("not json".utf8)) }

        let client = EnrollmentClient(baseURL: URL(string: "https://hotel.example")!, session: .stubbed())
        let request = EnrollmentRequest(
            inviteCode: "INV-1234",
            devicePubkeyB64: "cHVia2V5",
            deviceName: "device",
            platform: "ios"
        )

        do {
            _ = try await client.enroll(request)
            XCTFail("expected enroll to throw")
        } catch EnrollmentError.malformedResponse {
            // expected
        } catch {
            XCTFail("unexpected error: \(error)")
        }
    }
}
