// EndpointSelector.swift
// Races `GET /health` probes across candidate mesh endpoints and returns the
// fastest healthy one, remembering a sticky choice per network path
// (identified by e.g. an `NWPathMonitor` path identifier string) with a
// fallback anchor endpoint if the sticky/raced candidates are unreachable.

import Foundation

/// A named endpoint the client can reach the mesh through.
public struct EndpointCandidate: Sendable, Equatable, Hashable {
    /// Human-friendly identifier (e.g. a hotel node id).
    public var name: String
    /// Base URL to probe and connect through (e.g. "http://100.79.239.64:7700").
    public var baseURL: URL

    public init(name: String, baseURL: URL) {
        self.name = name
        self.baseURL = baseURL
    }
}

/// Races `/health` probes across candidates and picks the fastest healthy
/// endpoint, sticking to the winner per network path until it stops
/// responding.
public actor EndpointSelector {
    private let session: URLSession
    private let probeTimeout: TimeInterval
    private let anchor: EndpointCandidate?
    private var stickyByPath: [String: EndpointCandidate] = [:]

    /// - Parameters:
    ///   - session: `URLSession` to issue health probes on. Inject a session
    ///     configured with a stub `URLProtocol` for tests.
    ///   - probeTimeout: Per-candidate timeout for the `/health` race.
    ///   - anchor: A last-resort endpoint (e.g. the operator's home hotel)
    ///     tried if no candidate answers in time.
    public init(
        session: URLSession = .shared,
        probeTimeout: TimeInterval = 2.0,
        anchor: EndpointCandidate? = nil
    ) {
        self.session = session
        self.probeTimeout = probeTimeout
        self.anchor = anchor
    }

    /// Selects the best endpoint for the given network path.
    ///
    /// - `networkPathId`: an opaque identifier for the current network path
    ///   (e.g. derived from `NWPath.description` or a Wi-Fi SSID/interface
    ///   name change signal). Pass `nil` to disable stickiness.
    public func selectEndpoint(
        from candidates: [EndpointCandidate],
        networkPathId: String? = nil
    ) async -> EndpointCandidate? {
        if let path = networkPathId,
            let sticky = stickyByPath[path],
            candidates.contains(sticky),
            await probeHealthy(sticky)
        {
            return sticky
        }

        if let winner = await raceHealthProbes(candidates) {
            if let path = networkPathId {
                stickyByPath[path] = winner
            }
            return winner
        }

        if let anchor, await probeHealthy(anchor) {
            if let path = networkPathId {
                stickyByPath[path] = anchor
            }
            return anchor
        }

        return nil
    }

    /// Clears any remembered sticky endpoint for a network path (e.g. call
    /// this when a path is torn down).
    public func forgetStickyEndpoint(forPath networkPathId: String) {
        stickyByPath.removeValue(forKey: networkPathId)
    }

    /// Races `/health` GETs across all candidates concurrently and returns
    /// the first one that answers healthy within `probeTimeout`. Returns
    /// `nil` if none do.
    private func raceHealthProbes(_ candidates: [EndpointCandidate]) async -> EndpointCandidate? {
        guard !candidates.isEmpty else { return nil }

        return await withTaskGroup(of: EndpointCandidate?.self) { group in
            for candidate in candidates {
                group.addTask {
                    await self.probeHealthy(candidate) ? candidate : nil
                }
            }
            for await result in group {
                if let winner = result {
                    group.cancelAll()
                    return winner
                }
            }
            return nil
        }
    }

    /// Probes a single candidate's `/health` endpoint, returning `true` if it
    /// answers with a 2xx status within `probeTimeout`.
    private func probeHealthy(_ candidate: EndpointCandidate) async -> Bool {
        let healthURL = candidate.baseURL.appendingPathComponent("health")
        let timeout = probeTimeout
        var builtRequest = URLRequest(url: healthURL)
        builtRequest.httpMethod = "GET"
        builtRequest.timeoutInterval = timeout
        let request = builtRequest

        do {
            let (_, response) = try await withTimeout(timeout) { [session] in
                try await session.data(for: request)
            }
            guard let http = response as? HTTPURLResponse else { return false }
            return (200..<300).contains(http.statusCode)
        } catch {
            return false
        }
    }
}

/// Races an async operation against a timeout, throwing `CancellationError`
/// if the timeout wins.
func withTimeout<T: Sendable>(
    _ seconds: TimeInterval,
    operation: @escaping @Sendable () async throws -> T
) async throws -> T {
    try await withThrowingTaskGroup(of: T.self) { group in
        group.addTask { try await operation() }
        group.addTask {
            try await Task.sleep(nanoseconds: UInt64(max(0, seconds) * 1_000_000_000))
            throw CancellationError()
        }
        let result = try await group.next()!
        group.cancelAll()
        return result
    }
}
