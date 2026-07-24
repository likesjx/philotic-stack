// EdgeProtocol.swift
// Swift Codable mirrors of `philotic-edge-protocol` (crates/philotic-edge-protocol).
//
// These types must decode/encode byte-compatibly with the Rust wire types.
// `EdgeMessage` is internally tagged on `"type"` in Rust (serde
// `#[serde(tag = "type", rename_all = "snake_case")]`); newtype variants
// (like `Hello(EdgeHello)`) are *flattened* into the same JSON object as the
// tag, not nested under an extra key. The custom `Codable` conformance below
// replicates that flattening by hand.
//
// Golden JSON fixtures below are copy-pasted verbatim from
// `crates/philotic-edge-protocol/src/lib.rs` (`golden_json_hello`,
// `golden_json_turn_event`) and are asserted against in
// `EdgeProtocolTests.swift`. Do not change without bumping
// `protocolVersion` in lockstep with `PROTOCOL_VERSION` on the Rust side.

import Foundation

/// Current wire protocol version, carried in every ``EdgeEnvelope`` as `"v"`.
public let protocolVersion: UInt16 = 1

// MARK: - EdgeEnvelope

/// Top-level frame exchanged over the edge WebSocket.
public struct EdgeEnvelope: Codable, Equatable, Sendable {
    /// Protocol version (``protocolVersion``).
    public var v: UInt16
    /// Sender-local monotonically increasing sequence number.
    public var seq: UInt64
    /// Highest contiguous peer sequence number received, if any.
    public var ack: UInt64?
    /// The message payload.
    public var msg: EdgeMessage

    public init(v: UInt16 = protocolVersion, seq: UInt64, ack: UInt64? = nil, msg: EdgeMessage) {
        self.v = v
        self.seq = seq
        self.ack = ack
        self.msg = msg
    }
}

// MARK: - EdgeHello

/// Payload of ``EdgeMessage/hello(_:)``.
public struct EdgeHello: Codable, Equatable, Sendable {
    /// The edge node id assigned at enrollment.
    public var nodeId: String
    /// What this device offers the mesh.
    public var capabilities: EdgeCapabilities
    /// Last durable resume cursor (opaque), or `nil` for a fresh session.
    public var cursor: String?

    public init(nodeId: String, capabilities: EdgeCapabilities, cursor: String? = nil) {
        self.nodeId = nodeId
        self.capabilities = capabilities
        self.cursor = cursor
    }

    private enum CodingKeys: String, CodingKey {
        case nodeId = "node_id"
        case capabilities
        case cursor
    }
}

// MARK: - EdgeCapabilities

/// What an edge device offers the mesh.
public struct EdgeCapabilities: Codable, Equatable, Sendable {
    /// Human-friendly device name (e.g. "Jared's iPhone").
    public var deviceName: String
    /// Platform identifier (e.g. "ios", "macos").
    public var platform: String
    /// Mesh roles this device can play (e.g. "ClientNode", "ModelNode").
    public var roles: [String]
    /// Tool refs the device exposes (e.g. "os.ios.healthkit.read@1").
    public var tools: [String]
    /// Model refs the device can run locally.
    public var models: [String]

    public init(
        deviceName: String,
        platform: String,
        roles: [String] = [],
        tools: [String] = [],
        models: [String] = []
    ) {
        self.deviceName = deviceName
        self.platform = platform
        self.roles = roles
        self.tools = tools
        self.models = models
    }

    private enum CodingKeys: String, CodingKey {
        case deviceName = "device_name"
        case platform
        case roles
        case tools
        case models
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        deviceName = try container.decode(String.self, forKey: .deviceName)
        platform = try container.decode(String.self, forKey: .platform)
        roles = try container.decodeIfPresent([String].self, forKey: .roles) ?? []
        tools = try container.decodeIfPresent([String].self, forKey: .tools) ?? []
        models = try container.decodeIfPresent([String].self, forKey: .models) ?? []
    }
}

// MARK: - BlobRef

/// Reference to a blob in the hotel blob store, attachable to a turn.
public struct BlobRef: Codable, Equatable, Sendable {
    /// Blob identifier.
    public var blobId: String
    /// URL the blob can be fetched from, if already resolvable.
    public var downloadUrl: String?
    /// MIME type of the blob.
    public var mime: String?

    public init(blobId: String, downloadUrl: String? = nil, mime: String? = nil) {
        self.blobId = blobId
        self.downloadUrl = downloadUrl
        self.mime = mime
    }

    private enum CodingKeys: String, CodingKey {
        case blobId = "blob_id"
        case downloadUrl = "download_url"
        case mime
    }
}

// MARK: - TurnEventKind

/// Kind of a ``EdgeMessage/turnEvent(conversationId:eventKind:content:turnId:)``.
public enum TurnEventKind: String, Codable, Equatable, Sendable {
    /// Incremental streamed token(s).
    case token
    /// Final complete turn output.
    case final
    /// Status update (e.g. "thinking", "running tool").
    case status
    /// The turn failed; `content` carries the error.
    case error
}

// MARK: - EnrollmentRequest / EnrollmentResponse

/// One-time device enrollment request (HTTP, not WebSocket).
public struct EnrollmentRequest: Codable, Equatable, Sendable {
    /// Operator-issued one-time invite code.
    public var inviteCode: String
    /// Device public key, base64-encoded.
    public var devicePubkeyB64: String
    /// Human-friendly device name.
    public var deviceName: String
    /// Platform identifier (e.g. "ios", "macos").
    public var platform: String

    public init(inviteCode: String, devicePubkeyB64: String, deviceName: String, platform: String) {
        self.inviteCode = inviteCode
        self.devicePubkeyB64 = devicePubkeyB64
        self.deviceName = deviceName
        self.platform = platform
    }

    private enum CodingKeys: String, CodingKey {
        case inviteCode = "invite_code"
        case devicePubkeyB64 = "device_pubkey_b64"
        case deviceName = "device_name"
        case platform
    }
}

/// Successful enrollment response (HTTP, not WebSocket).
public struct EnrollmentResponse: Codable, Equatable, Sendable {
    /// Edge node id assigned to this device.
    public var nodeId: String
    /// Bearer token the device presents on every connection. This is the
    /// only credential an edge device ever holds — never a backbone PSK.
    public var edgeToken: String

    public init(nodeId: String, edgeToken: String) {
        self.nodeId = nodeId
        self.edgeToken = edgeToken
    }

    private enum CodingKeys: String, CodingKey {
        case nodeId = "node_id"
        case edgeToken = "edge_token"
    }
}

// MARK: - EdgeMessage

/// All messages that flow over the edge WebSocket, in both directions.
///
/// Mirrors the Rust `EdgeMessage`, which is internally tagged with `"type"`
/// (`{"type": "hello", ...fields...}`). Newtype variants over a struct (only
/// ``hello(_:)``) are flattened by serde into the same JSON object as the
/// tag; this conformance replicates that by encoding/decoding the inner
/// struct's fields directly rather than nesting them.
public enum EdgeMessage: Equatable, Sendable {
    /// Client -> server: first frame after connecting; identifies the device
    /// and presents the resume cursor.
    case hello(EdgeHello)
    /// Server -> client: accepts the hello and opens the session.
    case helloAck(sessionId: String, replayFrom: String?)
    /// Client -> server: submit an operator/agent turn to a target agent.
    case turnSubmit(
        targetNodeId: String,
        targetAgentId: String,
        conversationId: String?,
        content: String,
        blobRefs: [BlobRef],
        messageKind: String? = nil
    )
    /// Server -> client: streamed turn output and status.
    case turnEvent(
        conversationId: String,
        eventKind: TurnEventKind,
        content: String,
        turnId: String?
    )
    /// Server -> client: an action awaits operator approval.
    case approvalRequest(approvalId: String, description: String, risk: String?)
    /// Client -> server: resolve a pending approval.
    case approvalResolve(approvalId: String, approved: Bool, note: String?)
    /// Server -> client: a LifeGraph node changed.
    case lifeGraphChange(changeKind: String, nodeId: String, label: String?, summary: String?)
    /// Server -> client: a voice blob is ready for download/playback.
    case voiceBlob(blobId: String, downloadUrl: String, mime: String?, transcript: String?)
    /// Server -> client: a synthesized voice reply for a turn, carried inline
    /// as base64 audio (as opposed to ``voiceBlob(blobId:downloadUrl:mime:transcript:)``,
    /// which references a downloadable blob). When the server streams TTS
    /// per sentence, `chunkSeq` (0-based) and `isFinal` are present; both
    /// are omitted on the wire for whole-reply voice messages.
    case voiceReply(
        conversationId: String,
        turnId: String?,
        audioBase64: String,
        mimeType: String,
        transcript: String?,
        chunkSeq: UInt64? = nil,
        isFinal: Bool? = nil
    )
    /// Client -> server: open a live audio input stream to a target agent.
    /// The server assembles the chunks and submits the voice turn itself on
    /// ``audioStreamEnd(streamId:cancel:)`` with `cancel: false`.
    case audioStreamStart(
        streamId: String,
        targetNodeId: String,
        targetAgentId: String,
        conversationId: String?,
        mimeType: String
    )
    /// Client -> server: one chunk of a live audio stream. `chunkSeq` starts
    /// at 0 and must increment by 1; a violation discards the stream.
    case audioChunk(streamId: String, chunkSeq: UInt64, dataBase64: String)
    /// Client -> server: close a live audio stream. `cancel: false` makes the
    /// server assemble + store the audio and submit the voice turn;
    /// `cancel: true` discards it. A dropped WebSocket also discards any
    /// partial stream (re-record, don't resume).
    case audioStreamEnd(streamId: String, cancel: Bool)
    /// Server -> client: a realtime partial transcript of an in-flight audio
    /// stream. Ephemeral live UI feedback — never retained in the server's
    /// replay ring, so it must NOT advance the resume cursor. Sending it
    /// client -> server is invalid.
    case transcriptPartial(streamId: String, conversationId: String?, text: String, isFinal: Bool)
    /// Server -> client: invoke a tool the device advertised in its
    /// capabilities (e.g. HealthKit read, Shortcuts run).
    case toolInvoke(invocationId: String, toolRef: String, argsJson: String)
    /// Client -> server: result of a ``toolInvoke(invocationId:toolRef:argsJson:)``.
    case toolResult(invocationId: String, ok: Bool, resultJson: String)
    /// Client -> server: the device's capabilities changed mid-session.
    case capabilitiesUpdate(capabilities: EdgeCapabilities)
    /// Either direction: keepalive probe.
    case ping(nonce: UInt64)
    /// Either direction: keepalive reply.
    case pong(nonce: UInt64)
    /// Either direction: protocol or application error.
    case error(code: String, message: String, fatal: Bool)
}

extension EdgeMessage: Codable {
    private enum CodingKeys: String, CodingKey {
        case type
        case nodeId = "node_id"
        case capabilities
        case cursor
        case sessionId = "session_id"
        case replayFrom = "replay_from"
        case targetNodeId = "target_node_id"
        case targetAgentId = "target_agent_id"
        case conversationId = "conversation_id"
        case content
        case blobRefs = "blob_refs"
        case messageKind = "message_kind"
        case eventKind = "event_kind"
        case turnId = "turn_id"
        case approvalId = "approval_id"
        case description
        case risk
        case approved
        case note
        case changeKind = "change_kind"
        case label
        case summary
        case blobId = "blob_id"
        case downloadUrl = "download_url"
        case mime
        case transcript
        case audioBase64 = "audio_base64"
        case mimeType = "mime_type"
        case streamId = "stream_id"
        case chunkSeq = "chunk_seq"
        case dataBase64 = "data_base64"
        case cancel
        case isFinal = "is_final"
        case text
        case invocationId = "invocation_id"
        case toolRef = "tool_ref"
        case argsJson = "args_json"
        case ok
        case resultJson = "result_json"
        case nonce
        case code
        case message
        case fatal
    }

    private enum Tag: String {
        case hello
        case helloAck = "hello_ack"
        case turnSubmit = "turn_submit"
        case turnEvent = "turn_event"
        case approvalRequest = "approval_request"
        case approvalResolve = "approval_resolve"
        case lifeGraphChange = "life_graph_change"
        case voiceBlob = "voice_blob"
        case voiceReply = "voice_reply"
        case audioStreamStart = "audio_stream_start"
        case audioChunk = "audio_chunk"
        case audioStreamEnd = "audio_stream_end"
        case transcriptPartial = "transcript_partial"
        case toolInvoke = "tool_invoke"
        case toolResult = "tool_result"
        case capabilitiesUpdate = "capabilities_update"
        case ping
        case pong
        case error
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let rawTag = try container.decode(String.self, forKey: .type)
        guard let tag = Tag(rawValue: rawTag) else {
            throw DecodingError.dataCorruptedError(
                forKey: .type,
                in: container,
                debugDescription: "unknown EdgeMessage tag \"\(rawTag)\""
            )
        }

        switch tag {
        case .hello:
            let nodeId = try container.decode(String.self, forKey: .nodeId)
            let capabilities = try container.decode(EdgeCapabilities.self, forKey: .capabilities)
            let cursor = try container.decodeIfPresent(String.self, forKey: .cursor)
            self = .hello(EdgeHello(nodeId: nodeId, capabilities: capabilities, cursor: cursor))

        case .helloAck:
            let sessionId = try container.decode(String.self, forKey: .sessionId)
            let replayFrom = try container.decodeIfPresent(String.self, forKey: .replayFrom)
            self = .helloAck(sessionId: sessionId, replayFrom: replayFrom)

        case .turnSubmit:
            let targetNodeId = try container.decode(String.self, forKey: .targetNodeId)
            let targetAgentId = try container.decode(String.self, forKey: .targetAgentId)
            let conversationId = try container.decodeIfPresent(String.self, forKey: .conversationId)
            let content = try container.decode(String.self, forKey: .content)
            let blobRefs = try container.decodeIfPresent([BlobRef].self, forKey: .blobRefs) ?? []
            let messageKind = try container.decodeIfPresent(String.self, forKey: .messageKind)
            self = .turnSubmit(
                targetNodeId: targetNodeId,
                targetAgentId: targetAgentId,
                conversationId: conversationId,
                content: content,
                blobRefs: blobRefs,
                messageKind: messageKind
            )

        case .turnEvent:
            let conversationId = try container.decode(String.self, forKey: .conversationId)
            let eventKind = try container.decode(TurnEventKind.self, forKey: .eventKind)
            let content = try container.decode(String.self, forKey: .content)
            let turnId = try container.decodeIfPresent(String.self, forKey: .turnId)
            self = .turnEvent(
                conversationId: conversationId,
                eventKind: eventKind,
                content: content,
                turnId: turnId
            )

        case .approvalRequest:
            let approvalId = try container.decode(String.self, forKey: .approvalId)
            let description = try container.decode(String.self, forKey: .description)
            let risk = try container.decodeIfPresent(String.self, forKey: .risk)
            self = .approvalRequest(approvalId: approvalId, description: description, risk: risk)

        case .approvalResolve:
            let approvalId = try container.decode(String.self, forKey: .approvalId)
            let approved = try container.decode(Bool.self, forKey: .approved)
            let note = try container.decodeIfPresent(String.self, forKey: .note)
            self = .approvalResolve(approvalId: approvalId, approved: approved, note: note)

        case .lifeGraphChange:
            let changeKind = try container.decode(String.self, forKey: .changeKind)
            let nodeId = try container.decode(String.self, forKey: .nodeId)
            let label = try container.decodeIfPresent(String.self, forKey: .label)
            let summary = try container.decodeIfPresent(String.self, forKey: .summary)
            self = .lifeGraphChange(changeKind: changeKind, nodeId: nodeId, label: label, summary: summary)

        case .voiceBlob:
            let blobId = try container.decode(String.self, forKey: .blobId)
            let downloadUrl = try container.decode(String.self, forKey: .downloadUrl)
            let mime = try container.decodeIfPresent(String.self, forKey: .mime)
            let transcript = try container.decodeIfPresent(String.self, forKey: .transcript)
            self = .voiceBlob(blobId: blobId, downloadUrl: downloadUrl, mime: mime, transcript: transcript)

        case .voiceReply:
            let conversationId = try container.decode(String.self, forKey: .conversationId)
            let turnId = try container.decodeIfPresent(String.self, forKey: .turnId)
            let audioBase64 = try container.decode(String.self, forKey: .audioBase64)
            let mimeType = try container.decode(String.self, forKey: .mimeType)
            let transcript = try container.decodeIfPresent(String.self, forKey: .transcript)
            let chunkSeq = try container.decodeIfPresent(UInt64.self, forKey: .chunkSeq)
            let isFinal = try container.decodeIfPresent(Bool.self, forKey: .isFinal)
            self = .voiceReply(
                conversationId: conversationId,
                turnId: turnId,
                audioBase64: audioBase64,
                mimeType: mimeType,
                transcript: transcript,
                chunkSeq: chunkSeq,
                isFinal: isFinal
            )

        case .audioStreamStart:
            let streamId = try container.decode(String.self, forKey: .streamId)
            let targetNodeId = try container.decode(String.self, forKey: .targetNodeId)
            let targetAgentId = try container.decode(String.self, forKey: .targetAgentId)
            let conversationId = try container.decodeIfPresent(String.self, forKey: .conversationId)
            let mimeType = try container.decode(String.self, forKey: .mimeType)
            self = .audioStreamStart(
                streamId: streamId,
                targetNodeId: targetNodeId,
                targetAgentId: targetAgentId,
                conversationId: conversationId,
                mimeType: mimeType
            )

        case .audioChunk:
            let streamId = try container.decode(String.self, forKey: .streamId)
            let chunkSeq = try container.decode(UInt64.self, forKey: .chunkSeq)
            let dataBase64 = try container.decode(String.self, forKey: .dataBase64)
            self = .audioChunk(streamId: streamId, chunkSeq: chunkSeq, dataBase64: dataBase64)

        case .audioStreamEnd:
            let streamId = try container.decode(String.self, forKey: .streamId)
            let cancel = try container.decode(Bool.self, forKey: .cancel)
            self = .audioStreamEnd(streamId: streamId, cancel: cancel)

        case .transcriptPartial:
            let streamId = try container.decode(String.self, forKey: .streamId)
            let conversationId = try container.decodeIfPresent(String.self, forKey: .conversationId)
            let text = try container.decode(String.self, forKey: .text)
            let isFinal = try container.decode(Bool.self, forKey: .isFinal)
            self = .transcriptPartial(
                streamId: streamId,
                conversationId: conversationId,
                text: text,
                isFinal: isFinal
            )

        case .toolInvoke:
            let invocationId = try container.decode(String.self, forKey: .invocationId)
            let toolRef = try container.decode(String.self, forKey: .toolRef)
            let argsJson = try container.decode(String.self, forKey: .argsJson)
            self = .toolInvoke(invocationId: invocationId, toolRef: toolRef, argsJson: argsJson)

        case .toolResult:
            let invocationId = try container.decode(String.self, forKey: .invocationId)
            let ok = try container.decode(Bool.self, forKey: .ok)
            let resultJson = try container.decode(String.self, forKey: .resultJson)
            self = .toolResult(invocationId: invocationId, ok: ok, resultJson: resultJson)

        case .capabilitiesUpdate:
            let capabilities = try container.decode(EdgeCapabilities.self, forKey: .capabilities)
            self = .capabilitiesUpdate(capabilities: capabilities)

        case .ping:
            let nonce = try container.decode(UInt64.self, forKey: .nonce)
            self = .ping(nonce: nonce)

        case .pong:
            let nonce = try container.decode(UInt64.self, forKey: .nonce)
            self = .pong(nonce: nonce)

        case .error:
            let code = try container.decode(String.self, forKey: .code)
            let message = try container.decode(String.self, forKey: .message)
            let fatal = try container.decode(Bool.self, forKey: .fatal)
            self = .error(code: code, message: message, fatal: fatal)
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)

        switch self {
        case .hello(let hello):
            try container.encode(Tag.hello.rawValue, forKey: .type)
            try container.encode(hello.nodeId, forKey: .nodeId)
            try container.encode(hello.capabilities, forKey: .capabilities)
            try container.encodeIfPresent(hello.cursor, forKey: .cursor)

        case .helloAck(let sessionId, let replayFrom):
            try container.encode(Tag.helloAck.rawValue, forKey: .type)
            try container.encode(sessionId, forKey: .sessionId)
            try container.encodeIfPresent(replayFrom, forKey: .replayFrom)

        case .turnSubmit(
            let targetNodeId, let targetAgentId, let conversationId, let content, let blobRefs,
            let messageKind):
            try container.encode(Tag.turnSubmit.rawValue, forKey: .type)
            try container.encode(targetNodeId, forKey: .targetNodeId)
            try container.encode(targetAgentId, forKey: .targetAgentId)
            try container.encodeIfPresent(conversationId, forKey: .conversationId)
            try container.encode(content, forKey: .content)
            if !blobRefs.isEmpty {
                try container.encode(blobRefs, forKey: .blobRefs)
            }
            try container.encodeIfPresent(messageKind, forKey: .messageKind)

        case .turnEvent(let conversationId, let eventKind, let content, let turnId):
            try container.encode(Tag.turnEvent.rawValue, forKey: .type)
            try container.encode(conversationId, forKey: .conversationId)
            try container.encode(eventKind, forKey: .eventKind)
            try container.encode(content, forKey: .content)
            try container.encodeIfPresent(turnId, forKey: .turnId)

        case .approvalRequest(let approvalId, let description, let risk):
            try container.encode(Tag.approvalRequest.rawValue, forKey: .type)
            try container.encode(approvalId, forKey: .approvalId)
            try container.encode(description, forKey: .description)
            try container.encodeIfPresent(risk, forKey: .risk)

        case .approvalResolve(let approvalId, let approved, let note):
            try container.encode(Tag.approvalResolve.rawValue, forKey: .type)
            try container.encode(approvalId, forKey: .approvalId)
            try container.encode(approved, forKey: .approved)
            try container.encodeIfPresent(note, forKey: .note)

        case .lifeGraphChange(let changeKind, let nodeId, let label, let summary):
            try container.encode(Tag.lifeGraphChange.rawValue, forKey: .type)
            try container.encode(changeKind, forKey: .changeKind)
            try container.encode(nodeId, forKey: .nodeId)
            try container.encodeIfPresent(label, forKey: .label)
            try container.encodeIfPresent(summary, forKey: .summary)

        case .voiceBlob(let blobId, let downloadUrl, let mime, let transcript):
            try container.encode(Tag.voiceBlob.rawValue, forKey: .type)
            try container.encode(blobId, forKey: .blobId)
            try container.encode(downloadUrl, forKey: .downloadUrl)
            try container.encodeIfPresent(mime, forKey: .mime)
            try container.encodeIfPresent(transcript, forKey: .transcript)

        case .voiceReply(
            let conversationId, let turnId, let audioBase64, let mimeType, let transcript,
            let chunkSeq, let isFinal):
            try container.encode(Tag.voiceReply.rawValue, forKey: .type)
            try container.encode(conversationId, forKey: .conversationId)
            try container.encodeIfPresent(turnId, forKey: .turnId)
            try container.encode(audioBase64, forKey: .audioBase64)
            try container.encode(mimeType, forKey: .mimeType)
            try container.encodeIfPresent(transcript, forKey: .transcript)
            try container.encodeIfPresent(chunkSeq, forKey: .chunkSeq)
            try container.encodeIfPresent(isFinal, forKey: .isFinal)

        case .audioStreamStart(let streamId, let targetNodeId, let targetAgentId, let conversationId, let mimeType):
            try container.encode(Tag.audioStreamStart.rawValue, forKey: .type)
            try container.encode(streamId, forKey: .streamId)
            try container.encode(targetNodeId, forKey: .targetNodeId)
            try container.encode(targetAgentId, forKey: .targetAgentId)
            try container.encodeIfPresent(conversationId, forKey: .conversationId)
            try container.encode(mimeType, forKey: .mimeType)

        case .audioChunk(let streamId, let chunkSeq, let dataBase64):
            try container.encode(Tag.audioChunk.rawValue, forKey: .type)
            try container.encode(streamId, forKey: .streamId)
            try container.encode(chunkSeq, forKey: .chunkSeq)
            try container.encode(dataBase64, forKey: .dataBase64)

        case .audioStreamEnd(let streamId, let cancel):
            try container.encode(Tag.audioStreamEnd.rawValue, forKey: .type)
            try container.encode(streamId, forKey: .streamId)
            // `cancel` is always encoded, matching the Rust wire form.
            try container.encode(cancel, forKey: .cancel)

        case .transcriptPartial(let streamId, let conversationId, let text, let isFinal):
            try container.encode(Tag.transcriptPartial.rawValue, forKey: .type)
            try container.encode(streamId, forKey: .streamId)
            try container.encodeIfPresent(conversationId, forKey: .conversationId)
            try container.encode(text, forKey: .text)
            // `is_final` is always encoded, matching the Rust wire form.
            try container.encode(isFinal, forKey: .isFinal)

        case .toolInvoke(let invocationId, let toolRef, let argsJson):
            try container.encode(Tag.toolInvoke.rawValue, forKey: .type)
            try container.encode(invocationId, forKey: .invocationId)
            try container.encode(toolRef, forKey: .toolRef)
            try container.encode(argsJson, forKey: .argsJson)

        case .toolResult(let invocationId, let ok, let resultJson):
            try container.encode(Tag.toolResult.rawValue, forKey: .type)
            try container.encode(invocationId, forKey: .invocationId)
            try container.encode(ok, forKey: .ok)
            try container.encode(resultJson, forKey: .resultJson)

        case .capabilitiesUpdate(let capabilities):
            try container.encode(Tag.capabilitiesUpdate.rawValue, forKey: .type)
            try container.encode(capabilities, forKey: .capabilities)

        case .ping(let nonce):
            try container.encode(Tag.ping.rawValue, forKey: .type)
            try container.encode(nonce, forKey: .nonce)

        case .pong(let nonce):
            try container.encode(Tag.pong.rawValue, forKey: .type)
            try container.encode(nonce, forKey: .nonce)

        case .error(let code, let message, let fatal):
            try container.encode(Tag.error.rawValue, forKey: .type)
            try container.encode(code, forKey: .code)
            try container.encode(message, forKey: .message)
            try container.encode(fatal, forKey: .fatal)
        }
    }
}
