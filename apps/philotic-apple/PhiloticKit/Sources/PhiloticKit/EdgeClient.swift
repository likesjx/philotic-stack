// EdgeClient.swift
// WebSocket transport for the edge-mesh protocol: connects, performs the
// Hello/HelloAck handshake, exposes inbound `EdgeMessage`s as an
// `AsyncStream`, and reconnects automatically with exponential backoff,
// resuming from the highest durably processed server-push seq (presented as
// the cursor on every reconnect Hello).

import Foundation

/// Errors surfaced by `EdgeClient`.
public enum EdgeClientError: Error, Equatable, Sendable {
    /// `connect` was never called (or was called without required fields).
    case notConfigured
    /// An operation was attempted while no socket is open.
    case notConnected
    /// The server's first frame after Hello was not a HelloAck.
    case unexpectedFirstMessage
    /// The server rejected the handshake with a fatal `Error` frame (e.g.
    /// `not_enrolled`, `node_mismatch`, `bad_version`). Permanent — retrying
    /// with the same credentials cannot succeed, so reconnection stops.
    case handshakeRejected(code: String, message: String)
    /// An inbound WebSocket message was neither text nor binary JSON.
    case unsupportedMessageType
    /// An outbound envelope could not be encoded as UTF-8.
    case encodingFailed
}

/// Connection lifecycle state of an `EdgeClient`.
public enum EdgeConnectionState: Sendable, Equatable {
    case disconnected
    case connecting
    case connected(sessionId: String)
    case reconnecting(attempt: Int)
    /// The server fatally rejected the handshake; reconnection has stopped
    /// and operator action (re-enrollment / new token) is required.
    case failed(code: String, message: String)
}

/// Manages a single edge WebSocket connection: handshake, keepalive,
/// store-and-forward cursor resume, and automatic reconnect.
public actor EdgeClient {
    private let session: URLSession
    private let backoff: ReconnectBackoff
    private let keepaliveInterval: TimeInterval

    private var task: URLSessionWebSocketTask?
    private var url: URL?
    private var bearerToken: String?
    private var nodeId: String?
    private var capabilities: EdgeCapabilities?
    /// Durable cursor handed to `connect` (e.g. persisted from a previous
    /// process). Superseded by `highestPeerSeqSeen` once any server-push
    /// frame has been processed in this process.
    private var initialCursor: String?

    private var localSeq: UInt64 = 0
    /// Highest seq of a *durably processed server-push frame* (the retained,
    /// replayable kinds: turn events, approvals, LifeGraph changes, voice
    /// blobs, tool invokes). Piggybacked as `ack` on outbound envelopes —
    /// the server prunes its replay ring up to this value — and presented
    /// (as a decimal string) as the resume cursor on reconnect. Control
    /// frames (HelloAck / Pong / Error) never advance it: their seqs are
    /// higher than retained frames still in flight during replay, and acking
    /// them would let the server prune frames we have not processed.
    private var highestPeerSeqSeen: UInt64?

    private var continuation: AsyncStream<EdgeMessage>.Continuation?
    private var receiveLoopTask: Task<Void, Never>?
    private var keepaliveTask: Task<Void, Never>?
    private var reconnectTask: Task<Void, Never>?

    private var isRunning = false
    private var reconnectAttempt = 0

    /// Current connection lifecycle state.
    public private(set) var state: EdgeConnectionState = .disconnected

    /// - Parameters:
    ///   - session: `URLSession` the WebSocket task is created on.
    ///   - backoff: Reconnect backoff schedule.
    ///   - keepaliveInterval: Seconds between outbound `Ping`s.
    public init(
        session: URLSession = .shared,
        backoff: ReconnectBackoff = ReconnectBackoff(),
        keepaliveInterval: TimeInterval = 20
    ) {
        self.session = session
        self.backoff = backoff
        self.keepaliveInterval = keepaliveInterval
    }

    /// Opens the WebSocket, sends `Hello`, awaits `HelloAck`, and returns a
    /// stream of every subsequent inbound `EdgeMessage` (the `HelloAck`
    /// itself is yielded as the stream's first element). On unexpected
    /// disconnect, reconnects automatically with exponential backoff,
    /// presenting the seq of the last durably processed server-push frame as
    /// the resume cursor so the server replays anything sent during the gap.
    /// A fatal handshake rejection (`not_enrolled`, `node_mismatch`,
    /// `bad_version`) stops reconnection and surfaces as
    /// ``EdgeConnectionState/failed(code:message:)``.
    public func connect(
        url: URL,
        bearerToken: String,
        nodeId: String,
        capabilities: EdgeCapabilities,
        cursor: String? = nil
    ) async throws -> AsyncStream<EdgeMessage> {
        self.url = url
        self.bearerToken = bearerToken
        self.nodeId = nodeId
        self.capabilities = capabilities
        self.initialCursor = cursor
        self.highestPeerSeqSeen = nil
        self.isRunning = true
        self.reconnectAttempt = 0

        let stream = AsyncStream<EdgeMessage> { continuation in
            self.continuation = continuation
            continuation.onTermination = { [weak self] _ in
                guard let self else { return }
                Task { await self.disconnect() }
            }
        }

        try await openSocketAndHandshake()

        return stream
    }

    /// Sends a message on the current connection, tagging it with the next
    /// local sequence number and the highest peer sequence seen so far.
    public func send(_ message: EdgeMessage) async throws {
        let envelope = EdgeEnvelope(seq: nextSeq(), ack: highestPeerSeqSeen, msg: message)
        try await sendEnvelope(envelope)
    }

    /// Tears the connection down and stops reconnecting. Finishes the
    /// inbound message stream.
    public func disconnect() async {
        isRunning = false
        reconnectTask?.cancel()
        reconnectTask = nil
        receiveLoopTask?.cancel()
        receiveLoopTask = nil
        keepaliveTask?.cancel()
        keepaliveTask = nil
        task?.cancel(with: .normalClosure, reason: nil)
        task = nil
        // A fatal handshake rejection is a terminal diagnosis — keep it
        // visible instead of regressing to a generic "disconnected".
        if case .failed = state {} else {
            state = .disconnected
        }
        continuation?.finish()
        continuation = nil
    }

    /// The resume cursor to present on the next `Hello`: the highest durably
    /// processed server-push seq from this process, else the cursor handed
    /// to `connect`.
    private var resumeCursor: String? {
        highestPeerSeqSeen.map(String.init) ?? initialCursor
    }

    /// True for the message kinds the server retains in its replay ring
    /// (server-push traffic). Only these advance the ack/resume cursor —
    /// see `highestPeerSeqSeen`.
    static func advancesResumeCursor(_ message: EdgeMessage) -> Bool {
        switch message {
        case .turnEvent, .approvalRequest, .lifeGraphChange, .voiceBlob, .toolInvoke:
            return true
        case .hello, .helloAck, .turnSubmit, .approvalResolve, .toolResult,
            .capabilitiesUpdate, .ping, .pong, .error:
            return false
        }
    }

    /// Pure ack-watermark step: advances `current` to `envelopeSeq` only for
    /// server-push (retained) message kinds, and never backwards — replayed
    /// frames re-arrive with lower seqs than live frames already processed.
    static func advanceWatermark(
        _ current: UInt64?,
        envelopeSeq: UInt64,
        message: EdgeMessage
    ) -> UInt64? {
        guard advancesResumeCursor(message) else { return current }
        return max(current ?? 0, envelopeSeq)
    }

    // MARK: - Handshake

    private func openSocketAndHandshake() async throws {
        guard let url, let bearerToken, let nodeId, let capabilities else {
            throw EdgeClientError.notConfigured
        }

        state = .connecting

        var request = URLRequest(url: url)
        request.setValue("Bearer \(bearerToken)", forHTTPHeaderField: "Authorization")
        let newTask = session.webSocketTask(with: request)
        task = newTask
        newTask.resume()

        let hello = EdgeMessage.hello(
            EdgeHello(nodeId: nodeId, capabilities: capabilities, cursor: resumeCursor))
        try await sendEnvelope(EdgeEnvelope(seq: nextSeq(), ack: highestPeerSeqSeen, msg: hello))

        let ackEnvelope = try await receiveEnvelope(on: newTask)
        switch ackEnvelope.msg {
        case .helloAck(let sessionId, _):
            // Note: HelloAck neither seeds the ack watermark nor moves the
            // cursor — `replayFrom` is an echo of the cursor we presented,
            // and the HelloAck's own seq is minted AFTER the retained frames
            // about to be replayed, so acking it would prune them server-side
            // before we processed them.
            state = .connected(sessionId: sessionId)
            continuation?.yield(ackEnvelope.msg)
        case .error(let code, let message, _):
            // Handshake-phase Error frames are always fatal server-side
            // (not_enrolled / node_mismatch / bad_version): stop reconnecting
            // and surface the diagnosis instead of hammering the server.
            isRunning = false
            state = .failed(code: code, message: message)
            throw EdgeClientError.handshakeRejected(code: code, message: message)
        default:
            throw EdgeClientError.unexpectedFirstMessage
        }

        startReceiveLoop()
        startKeepalive()
    }

    // MARK: - Receive loop

    private func startReceiveLoop() {
        receiveLoopTask?.cancel()
        receiveLoopTask = Task {
            await self.receiveLoop()
        }
    }

    private func receiveLoop() async {
        while isRunning, let currentTask = task {
            do {
                let envelope = try await receiveEnvelope(on: currentTask)
                highestPeerSeqSeen = Self.advanceWatermark(
                    highestPeerSeqSeen, envelopeSeq: envelope.seq, message: envelope.msg)
                if case .ping(let nonce) = envelope.msg {
                    try? await sendEnvelope(
                        EdgeEnvelope(seq: nextSeq(), ack: highestPeerSeqSeen, msg: .pong(nonce: nonce))
                    )
                }
                continuation?.yield(envelope.msg)
            } catch {
                guard isRunning else { return }
                await handleDisconnect()
                return
            }
        }
    }

    // MARK: - Keepalive

    private func startKeepalive() {
        keepaliveTask?.cancel()
        let interval = keepaliveInterval
        keepaliveTask = Task {
            while self.isRunning, !Task.isCancelled {
                try? await Task.sleep(nanoseconds: UInt64(max(0, interval) * 1_000_000_000))
                guard self.isRunning, !Task.isCancelled else { return }
                let nonce = UInt64.random(in: 0...UInt64.max)
                try? await self.send(.ping(nonce: nonce))
            }
        }
    }

    // MARK: - Reconnect

    private func handleDisconnect() async {
        task?.cancel(with: .goingAway, reason: nil)
        task = nil
        keepaliveTask?.cancel()
        keepaliveTask = nil
        receiveLoopTask = nil

        guard isRunning else { return }

        state = .reconnecting(attempt: reconnectAttempt)
        reconnectTask?.cancel()
        reconnectTask = Task {
            await self.reconnectLoop()
        }
    }

    private func reconnectLoop() async {
        while isRunning {
            let delay = backoff.delay(forAttempt: reconnectAttempt)
            reconnectAttempt += 1
            try? await Task.sleep(nanoseconds: UInt64(max(0, delay) * 1_000_000_000))
            guard isRunning else { return }

            do {
                try await openSocketAndHandshake()
                reconnectAttempt = 0
                return
            } catch EdgeClientError.handshakeRejected(let code, let message) {
                // Permanent rejection (openSocketAndHandshake already set
                // state = .failed and stopped the run loop): surface the
                // reason on the message stream, then end it.
                continuation?.yield(.error(code: code, message: message, fatal: true))
                continuation?.finish()
                continuation = nil
                return
            } catch {
                state = .reconnecting(attempt: reconnectAttempt)
                continue
            }
        }
    }

    // MARK: - Wire I/O

    private func nextSeq() -> UInt64 {
        localSeq += 1
        return localSeq
    }

    private func sendEnvelope(_ envelope: EdgeEnvelope) async throws {
        guard let task else { throw EdgeClientError.notConnected }
        let data = try JSONEncoder().encode(envelope)
        guard let text = String(data: data, encoding: .utf8) else {
            throw EdgeClientError.encodingFailed
        }
        try await task.send(.string(text))
    }

    private func receiveEnvelope(on task: URLSessionWebSocketTask) async throws -> EdgeEnvelope {
        let message = try await task.receive()
        let data: Data
        switch message {
        case .data(let raw):
            data = raw
        case .string(let text):
            data = Data(text.utf8)
        @unknown default:
            throw EdgeClientError.unsupportedMessageType
        }
        return try JSONDecoder().decode(EdgeEnvelope.self, from: data)
    }
}
