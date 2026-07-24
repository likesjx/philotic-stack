// StubURLProtocol.swift
// A registerable `URLProtocol` stand-in so tests can exercise
// `EndpointSelector` and `EnrollmentClient` without a live server.

import Foundation

/// Intercepts every request through a `URLSession` configured with this
/// protocol class and answers it according to `StubURLProtocol.responder`,
/// keyed by the request's URL.
final class StubURLProtocol: URLProtocol, @unchecked Sendable {
    /// A canned response: either a delayed/instant success, or a failure.
    struct Stub: Sendable {
        var statusCode: Int
        var body: Data
        /// Artificial delay before responding, to exercise racing/timeouts.
        var delay: TimeInterval

        init(statusCode: Int = 200, body: Data = Data(), delay: TimeInterval = 0) {
            self.statusCode = statusCode
            self.body = body
            self.delay = delay
        }

        static func fail() -> Stub? { nil }
    }

    /// Maps a request's absolute URL string to a canned stub. Return `nil`
    /// (or omit the key) to simulate a network failure for that URL.
    nonisolated(unsafe) static var responder: (@Sendable (URLRequest) -> Stub?)?

    override class func canInit(with request: URLRequest) -> Bool { true }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let request = self.request
        let stub = StubURLProtocol.responder?(request)

        guard let stub else {
            client?.urlProtocol(
                self,
                didFailWithError: URLError(.cannotConnectToHost)
            )
            return
        }

        let work: @Sendable () -> Void = { [weak self] in
            guard let self else { return }
            guard let url = request.url,
                let response = HTTPURLResponse(
                    url: url,
                    statusCode: stub.statusCode,
                    httpVersion: "HTTP/1.1",
                    headerFields: nil
                )
            else {
                self.client?.urlProtocol(self, didFailWithError: URLError(.badURL))
                return
            }
            self.client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            self.client?.urlProtocol(self, didLoad: stub.body)
            self.client?.urlProtocolDidFinishLoading(self)
        }

        if stub.delay > 0 {
            DispatchQueue.global().asyncAfter(deadline: .now() + stub.delay, execute: work)
        } else {
            work()
        }
    }

    override func stopLoading() {}

    /// Resolves a request's outgoing body. `URLSession` sometimes delivers
    /// small bodies as `httpBodyStream` rather than `httpBody` by the time a
    /// custom `URLProtocol` sees the request, so check both.
    static func resolvedBody(for request: URLRequest) -> Data? {
        if let body = request.httpBody {
            return body
        }
        guard let stream = request.httpBodyStream else { return nil }
        stream.open()
        defer { stream.close() }
        var data = Data()
        let bufferSize = 4096
        var buffer = [UInt8](repeating: 0, count: bufferSize)
        while stream.hasBytesAvailable {
            let read = stream.read(&buffer, maxLength: bufferSize)
            guard read > 0 else { break }
            data.append(buffer, count: read)
        }
        return data
    }
}

extension URLSession {
    /// A session that routes all requests through `StubURLProtocol`.
    static func stubbed() -> URLSession {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [StubURLProtocol.self]
        return URLSession(configuration: config)
    }
}

/// A trivial lock-protected box so test closures that must be `@Sendable`
/// (e.g. `StubURLProtocol.responder`) can accumulate state without tripping
/// Swift 6's captured-var concurrency checks.
final class Locked<Value>: @unchecked Sendable {
    private let lock = NSLock()
    private var storage: Value

    init(_ value: Value) {
        storage = value
    }

    var value: Value {
        get {
            lock.lock()
            defer { lock.unlock() }
            return storage
        }
        set {
            lock.lock()
            defer { lock.unlock() }
            storage = newValue
        }
    }
}
