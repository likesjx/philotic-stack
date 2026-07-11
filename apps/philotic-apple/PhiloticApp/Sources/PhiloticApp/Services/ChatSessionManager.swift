// ChatSessionManager.swift
// App-level orchestrator: owns connection settings, the EdgeClient
// connection, local conversation history, and per-agent chat state. Wires
// PhiloticKit's edge-mesh transport to the SwiftUI views.

import Foundation
import Network
import Observation
import PhiloticKit

#if os(macOS)
    import AppKit
#else
    import UIKit
#endif

@MainActor
@Observable
public final class ChatSessionManager {
    public var settings: ConnectionSettings {
        didSet { ConnectionSettingsStore.save(settings) }
    }

    public private(set) var connectionState: EdgeConnectionState = .disconnected
    public private(set) var selectedEndpointName: String?
    /// Live agent directory from the hotel; falls back to the built-in
    /// catalog until `refreshAgents()` succeeds.
    public private(set) var agents: [AgentTarget] = AgentTarget.builtIn
    public private(set) var conversations: [Conversation] = []
    public private(set) var currentConversation: Conversation?
    public var currentAgent: AgentTarget? {
        didSet { if currentAgent != oldValue { Task { await selectAgent(currentAgent) } } }
    }
    public var lastError: String?

    /// Owns dictation capture, voice-reply playback, and fallback TTS. Views
    /// bind directly to this for mic/speaker UI state.
    public let voiceController = VoiceController()
    /// User preference: speak every agent reply via fallback/serverside
    /// audio, even for turns the operator typed rather than spoke.
    /// Persisted directly to `UserDefaults` (not part of `ConnectionSettings`
    /// — it's a local UI preference, not connection config).
    public var speakAllReplies: Bool {
        didSet { UserDefaults.standard.set(speakAllReplies, forKey: Self.speakAllRepliesDefaultsKey) }
    }
    /// User preference: transcribe speech on-device (SFSpeechRecognizer
    /// dictation) and submit text, instead of the default — uploading the
    /// raw recording for hotel-side transcription. Default OFF.
    public var transcribeOnDevice: Bool {
        didSet { UserDefaults.standard.set(transcribeOnDevice, forKey: Self.transcribeOnDeviceDefaultsKey) }
    }
    /// True while a recorded voice message is being uploaded + submitted
    /// (HTTP blob fallback path only — used when the WS is not connected).
    public private(set) var isSendingVoice = false
    /// True while voice audio is streaming live over the edge WebSocket.
    public private(set) var isStreamingVoice = false
    /// Realtime partial transcript of the in-flight voice stream (server
    /// STT), rendered live in the input field. Stays nil when the server
    /// sends no `transcript_partial` frames (older hotel) — the UI degrades
    /// to the plain "Streaming…" state.
    public private(set) var liveTranscript: String?
    /// True while hands-free conversation mode is running (continuous PCM
    /// capture + VAD-segmented utterance cycles).
    public private(set) var isConversationActive = false

    private static let speakAllRepliesDefaultsKey = "com.philotic.apple.speakAllReplies"
    private static let transcribeOnDeviceDefaultsKey = "com.philotic.apple.transcribeOnDevice"

    private let edgeClient: EdgeClient
    private let endpointSelector: EndpointSelector
    private let conversationStore: ConversationStore
    private var streamTask: Task<Void, Never>?
    private var statePollTask: Task<Void, Never>?
    private var isConnectPending = false

    /// Conversation ids for which the operator's last turn was submitted as
    /// voice — used to decide whether a Final `TurnEvent` (which always
    /// arrives, voice or not) should schedule a fallback TTS read-aloud if
    /// no `VoiceReply` shows up in time.
    private var voiceExpectedConversations: Set<String> = []
    /// Pending fallback-TTS timers, keyed by conversation id, so a
    /// `VoiceReply` (or a new turn) can cancel a stale one.
    private var fallbackTasks: [String: Task<Void, Never>] = [:]

    /// Forwards captured audio chunks over the WS while streaming; resolves
    /// `true` when every chunk (including the final drain) was sent.
    private var voiceStreamTask: Task<Bool, Never>?
    /// stream_id of the in-flight WS audio stream.
    private var activeVoiceStreamId: String?
    /// True when the current capture is the record→HTTP-upload fallback
    /// (WS was not connected when the mic was pressed).
    private var voiceCaptureIsFallback = false

    /// Identity ("conversationId|turnId") of the chunked voice reply whose
    /// chunks are currently accepted into the playback queue. Chunks keyed
    /// to anything else (a superseded turn) are dropped.
    private var activeChunkedReplyKey: String?

    /// The "🎤 Voice message" placeholder bubble of the most recent streamed
    /// voice turn, so a late `transcript_partial(is_final:true)` (it usually
    /// lands after `audio_stream_end`) can replace the placeholder with the
    /// real transcript. Harmlessly stale if no final ever arrives.
    private var pendingVoiceBubble: (streamId: String, conversationId: String, messageId: String)?
    /// Final transcripts that arrived while their stream was still active
    /// (server finalized before the user released the mic), keyed by
    /// stream_id, consumed when the bubble is created at stream end.
    private var earlyFinalTranscripts: [String: String] = [:]

    /// Conversation mode: pumps VAD-segmented utterances from the continuous
    /// PCM capture into audio_stream cycles.
    @ObservationIgnored private var conversationTask: Task<Void, Never>?
    /// VAD state machine; recreated fresh on every `startConversation()`.
    @ObservationIgnored private var vad = VoiceActivityDetector()
    /// stream_id of conversation mode's currently OPEN utterance, if any.
    private var conversationStreamId: String?
    /// chunk_seq for the open conversation utterance (resets per utterance).
    @ObservationIgnored private var conversationChunkSeq: UInt64 = 0
    /// Conversations whose in-flight agent reply the user barged in on:
    /// VoiceReply frames for them are dropped until the next Final
    /// TurnEvent (which belongs to the user's NEW turn) clears the flag.
    private var suppressedVoiceReplyConversations: Set<String> = []

    /// Auto-reconnect triggers: network-path recovery, app activation, and
    /// a gentle periodic retry while frontmost. All of them only act on
    /// plain `.disconnected` — `.failed` is a fatal handshake rejection
    /// that needs operator action (the status bar's Reconnect button).
    private let pathMonitor = NWPathMonitor()
    @ObservationIgnored private var reconnectRetryTask: Task<Void, Never>?
    @ObservationIgnored private var activationObservers: [any NSObjectProtocol] = []
    private var isAppActive = true

    public init(
        edgeClient: EdgeClient = EdgeClient(),
        endpointSelector: EndpointSelector? = nil,
        conversationStore: ConversationStore = ConversationStore()
    ) {
        let loaded = ConnectionSettingsStore.load()
        self.settings = loaded
        self.edgeClient = edgeClient
        self.conversationStore = conversationStore
        self.speakAllReplies = UserDefaults.standard.bool(forKey: Self.speakAllRepliesDefaultsKey)
        self.transcribeOnDevice = UserDefaults.standard.bool(forKey: Self.transcribeOnDeviceDefaultsKey)
        if let endpointSelector {
            self.endpointSelector = endpointSelector
        } else if let anchorURL = loaded.anchorURL {
            self.endpointSelector = EndpointSelector(anchor: EndpointCandidate(name: "anchor", baseURL: anchorURL))
        } else {
            self.endpointSelector = EndpointSelector()
        }
        startReconnectTriggers()
    }

    // MARK: - Auto-reconnect triggers

    private func startReconnectTriggers() {
        // (a) Network path recovery: WiFi/wake transitions land here.
        pathMonitor.pathUpdateHandler = { [weak self] path in
            guard path.status == .satisfied else { return }
            Task { @MainActor [weak self] in
                await self?.reconnectIfDisconnected()
            }
        }
        pathMonitor.start(queue: DispatchQueue(label: "com.philotic.apple.path-monitor"))

        // (b) App activation: reconnect the moment the operator comes back.
        #if os(macOS)
            let activation = NSApplication.didBecomeActiveNotification
            let resignation = NSApplication.didResignActiveNotification
        #else
            let activation = UIApplication.didBecomeActiveNotification
            let resignation = UIApplication.willResignActiveNotification
        #endif
        activationObservers.append(
            NotificationCenter.default.addObserver(
                forName: activation, object: nil, queue: .main
            ) { [weak self] _ in
                Task { @MainActor [weak self] in
                    guard let self else { return }
                    self.isAppActive = true
                    await self.reconnectIfDisconnected()
                }
            }
        )
        activationObservers.append(
            NotificationCenter.default.addObserver(
                forName: resignation, object: nil, queue: .main
            ) { [weak self] _ in
                Task { @MainActor [weak self] in
                    self?.isAppActive = false
                }
            }
        )

        // (c) Gentle periodic retry while frontmost: EdgeClient's own backoff
        // covers socket-level retries, but once its run loop has fully
        // stopped (stream finished → .disconnected) something must call
        // connect() again — this timer is that something.
        reconnectRetryTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 10_000_000_000)
                guard let self else { return }
                guard self.isAppActive else { continue }
                await self.reconnectIfDisconnected()
            }
        }
    }

    /// Calls `connect()` only from plain `.disconnected` with usable
    /// settings. Never auto-retries `.failed` (fatal handshake rejection —
    /// retrying with the same credentials just hammers the server), and
    /// never interferes with `.connecting`/`.reconnecting`/`.connected`.
    private func reconnectIfDisconnected() async {
        guard case .disconnected = connectionState else { return }
        guard settings.isConfigured else { return }
        await connect()
    }

    // MARK: - Lifecycle

    public func loadConversations() async {
        conversations = await conversationStore.all()
    }

    /// Resolves the best endpoint and opens the edge connection. Idempotent:
    /// a second call while connected or mid-connect is a no-op — RootView's
    /// appear and the settings sheet both trigger this, and two live sockets
    /// for one node make the server's single-session kick fight itself.
    public func connect() async {
        guard !isConnectPending else { return }
        if case .connected = connectionState { return }
        if case .connecting = connectionState { return }
        isConnectPending = true
        defer { isConnectPending = false }

        guard let anchorURL = settings.anchorURL, settings.isConfigured else {
            lastError = "Connection not configured — enroll or enter settings first."
            return
        }

        let candidate = EndpointCandidate(name: "anchor", baseURL: anchorURL)
        let selected = await endpointSelector.selectEndpoint(from: [candidate])
        selectedEndpointName = selected?.name

        guard let wsURL = settings.edgeWebSocketURL else {
            lastError = "Could not derive edge WebSocket URL from anchor."
            return
        }

        let capabilities = EdgeCapabilities(deviceName: settings.deviceName, platform: DeviceIdentity.platform)

        do {
            let stream = try await edgeClient.connect(
                url: wsURL,
                bearerToken: settings.edgeToken,
                nodeId: settings.nodeId,
                capabilities: capabilities
            )
            lastError = nil
            startStatePolling()
            streamTask?.cancel()
            streamTask = Task { [weak self] in
                guard let self else { return }
                for await message in stream {
                    await self.handleInbound(message)
                }
            }
            await refreshAgents()
        } catch {
            lastError = "Connect failed: \(error.localizedDescription)"
        }
    }

    /// Replace the hardcoded v0 catalog with the hotel's live agent
    /// directory (real registry node ids). Falls back silently to the
    /// built-in catalog when the endpoint is unreachable (old hotel binary).
    private func refreshAgents() async {
        guard let anchorURL = settings.anchorURL else { return }
        guard
            let fetched = try? await AgentDirectoryClient()
                .fetchAgents(baseURL: anchorURL, bearerToken: settings.edgeToken),
            !fetched.isEmpty
        else { return }
        agents = fetched.map {
            AgentTarget(
                targetNodeId: $0.targetNodeId,
                targetAgentId: $0.agentId,
                displayName: $0.displayName
            )
        }
    }

    public func disconnect() async {
        streamTask?.cancel()
        streamTask = nil
        statePollTask?.cancel()
        statePollTask = nil
        fallbackTasks.values.forEach { $0.cancel() }
        fallbackTasks.removeAll()
        // A dropped/closed WS discards partial audio streams server-side;
        // just stop capturing and drop the forwarder / conversation loop.
        if isConversationActive {
            await stopConversation()
        }
        if isStreamingVoice {
            voiceController.stopPCMStreaming()
            voiceStreamTask?.cancel()
            voiceStreamTask = nil
            activeVoiceStreamId = nil
            isStreamingVoice = false
            liveTranscript = nil
        }
        voiceController.stopPlayback()
        voiceController.cancelFallback()
        await edgeClient.disconnect()
        connectionState = .disconnected
    }

    private func startStatePolling() {
        statePollTask?.cancel()
        statePollTask = Task { [weak self] in
            guard let self else { return }
            while !Task.isCancelled {
                let state = await self.edgeClient.state
                await MainActor.run { self.connectionState = state }
                try? await Task.sleep(nanoseconds: 500_000_000)
            }
        }
    }

    // MARK: - Agent selection & history

    private func selectAgent(_ target: AgentTarget?) async {
        guard let target else {
            currentConversation = nil
            return
        }
        var conversation = await conversationStore.findOrCreate(agentTarget: target, conversationId: nil)

        if let anchorURL = settings.anchorURL, settings.isConfigured {
            if let hydrated = await HistoryHydrator.hydrate(
                sessionId: conversation.conversationId,
                baseURL: anchorURL,
                bearerToken: settings.edgeToken
            ), !hydrated.isEmpty {
                conversation.messages = hydrated
                await conversationStore.upsert(conversation)
            }
        }

        currentConversation = conversation
        conversations = await conversationStore.all()
    }

    // MARK: - Sending / receiving

    public func send(_ text: String) async {
        // "Speak replies" prefers the agent's PERSONA voice over local TTS:
        // tagging the typed turn voice-modality sets philote's
        // had_voice_input, so a `mode: auto` voice policy synthesizes the
        // reply (ElevenLabs) server-side. Local Apple TTS remains only the
        // 2.5s no-VoiceReply fallback (e.g. agents on pre-voice hotels).
        if speakAllReplies {
            if let conversation = currentConversation {
                voiceExpectedConversations.insert(conversation.conversationId)
            }
            await submitTurn(content: text, messageKind: "voice")
        } else {
            await submitTurn(content: text, messageKind: nil)
        }
    }

    /// Dictation path (on-device STT text), used when the "Transcribe on
    /// device" setting is ON. Tags the turn `message_kind: "voice"` and marks
    /// the conversation as expecting a `VoiceReply` (so a Final `TurnEvent`
    /// with no reply in 2.5s falls back to on-device TTS).
    public func sendVoiceMessage(text: String) async {
        guard let conversation = currentConversation else { return }
        voiceExpectedConversations.insert(conversation.conversationId)
        await submitTurn(content: text, messageKind: "voice")
    }

    /// Raw-audio path (the default): uploads the recorded file to the
    /// hotel's blob store, then submits a `voice`-kind turn with empty
    /// content and the returned blob ref attached — the hotel transcribes it
    /// via philote's media routing, like a Telegram voice note. Deletes the
    /// temp file when done either way.
    public func sendVoiceRecording(fileURL: URL) async {
        guard currentAgent != nil, let conversation = currentConversation else {
            try? FileManager.default.removeItem(at: fileURL)
            return
        }
        guard let anchorURL = settings.anchorURL else {
            lastError = "Connection not configured — cannot upload voice message."
            try? FileManager.default.removeItem(at: fileURL)
            return
        }

        isSendingVoice = true
        defer {
            isSendingVoice = false
            try? FileManager.default.removeItem(at: fileURL)
        }

        do {
            let data = try Data(contentsOf: fileURL)
            let ref = try await BlobClient().upload(
                baseURL: anchorURL,
                bearerToken: settings.edgeToken,
                data: data,
                mimeType: VoiceController.recordingMimeType
            )
            voiceExpectedConversations.insert(conversation.conversationId)
            await submitTurn(
                content: "",
                displayText: "🎤 Voice message",
                messageKind: "voice",
                blobRefs: [ref]
            )
        } catch {
            lastError = "Voice upload failed: \(error.localizedDescription)"
            appendSystemMessage("Voice upload failed: \(error.localizedDescription)", isError: true)
        }
    }

    // MARK: - Streaming voice capture (default mic behavior)

    /// Begins voice capture. While the edge WS is connected, raw PCM
    /// (16 kHz mono s16le — what the realtime STT API consumes) streams
    /// live over the socket (`audio_stream_start` / `audio_chunk` /
    /// `audio_stream_end`) and the SERVER transcribes in realtime and
    /// submits the voice turn itself. When not connected, falls back to
    /// record-to-m4a → HTTP blob upload (batch transcription still wants a
    /// container) so the mic always works.
    public func startVoiceStreaming() async {
        guard let target = currentAgent, currentConversation != nil else { return }
        guard !isStreamingVoice, !voiceController.isRecording, !voiceController.isCapturingPCM
        else { return }

        guard case .connected = connectionState else {
            voiceCaptureIsFallback = true
            await voiceController.startRecording()
            return
        }
        voiceCaptureIsFallback = false

        guard let chunks = await voiceController.startPCMStreaming() else { return }

        let streamId = UUID().uuidString
        let conversationId = currentConversation?.conversationId
        liveTranscript = nil
        pendingVoiceBubble = nil
        earlyFinalTranscripts.removeAll()

        do {
            try await edgeClient.send(
                .audioStreamStart(
                    streamId: streamId,
                    targetNodeId: target.targetNodeId,
                    targetAgentId: target.targetAgentId,
                    conversationId: conversationId,
                    mimeType: VoiceController.pcmStreamMimeType
                )
            )
        } catch {
            lastError = "Voice stream failed to start: \(error.localizedDescription)"
            voiceController.stopPCMStreaming()
            return
        }

        activeVoiceStreamId = streamId
        isStreamingVoice = true

        // Forward chunks as they appear. chunk_seq must start at 0 and
        // increment by 1 — the server discards the stream on any gap.
        voiceStreamTask = Task { [edgeClient] in
            var chunkSeq: UInt64 = 0
            for await chunk in chunks {
                do {
                    try await edgeClient.send(
                        .audioChunk(
                            streamId: streamId,
                            chunkSeq: chunkSeq,
                            dataBase64: chunk.base64EncodedString()
                        )
                    )
                    chunkSeq += 1
                } catch {
                    return false
                }
            }
            return true
        }
    }

    /// Ends voice capture. Streaming path: stops PCM capture (flushing the
    /// accumulator's tail frame through the chunk stream), waits for the
    /// forwarder to send the remaining chunk(s), then sends
    /// `audio_stream_end(cancel: false)` — the server submits the turn and
    /// the usual accepted-status/reply/VoiceReply flow follows on this
    /// conversation. If any chunk failed, ends with `cancel: true` instead
    /// (a partial stream is useless — re-record, don't resume). Fallback
    /// path: uploads the finished m4a recording over HTTP.
    public func finishVoiceStreaming() async {
        if voiceCaptureIsFallback {
            voiceCaptureIsFallback = false
            guard let fileURL = voiceController.stopRecording() else { return }
            await sendVoiceRecording(fileURL: fileURL)
            return
        }

        guard isStreamingVoice, let streamId = activeVoiceStreamId else { return }

        // Capture stop FIRST: flushes the final sub-frame bytes and finishes
        // the chunk stream, which lets the forwarder task complete.
        voiceController.stopPCMStreaming()
        let allChunksSent = await voiceStreamTask?.value ?? false
        voiceStreamTask = nil
        activeVoiceStreamId = nil
        isStreamingVoice = false
        liveTranscript = nil

        do {
            if allChunksSent, currentConversation != nil {
                try await edgeClient.send(.audioStreamEnd(streamId: streamId, cancel: false))
                await concludeVoiceUtterance(streamId: streamId)
            } else {
                try await edgeClient.send(.audioStreamEnd(streamId: streamId, cancel: true))
                lastError = "Voice stream interrupted — please try again."
                appendSystemMessage("Voice stream interrupted — please try again.", isError: true)
            }
        } catch {
            lastError = "Voice stream failed: \(error.localizedDescription)"
            appendSystemMessage("Voice stream failed: \(error.localizedDescription)", isError: true)
        }
    }

    /// Shared post-`audio_stream_end(cancel:false)` bookkeeping for both
    /// push-to-talk and conversation-mode utterances: marks the conversation
    /// voice-expecting and appends the operator bubble. If the realtime STT
    /// already finalized, the bubble carries the transcript immediately;
    /// otherwise it's a "🎤 Voice message" placeholder that a late
    /// `transcript_partial(is_final:true)` replaces (and remains a
    /// placeholder if partials never arrive — graceful degradation).
    private func concludeVoiceUtterance(streamId: String) async {
        guard let conversation = currentConversation else { return }
        voiceExpectedConversations.insert(conversation.conversationId)
        let bubbleText = earlyFinalTranscripts.removeValue(forKey: streamId) ?? "🎤 Voice message"
        let message = ChatMessage(role: .operatorUser, content: bubbleText)
        var updated = conversation
        updated.messages.append(message)
        currentConversation = updated
        await conversationStore.upsert(updated)
        if bubbleText == "🎤 Voice message" {
            pendingVoiceBubble = (streamId, conversation.conversationId, message.id)
        }
    }

    // MARK: - Conversation mode (hands-free, client-side VAD)

    /// Starts hands-free conversation mode: one continuous PCM capture; an
    /// energy VAD segments utterances, each of which runs a full
    /// audio_stream_start/chunk/end cycle (fresh stream_id, same target and
    /// conversation) — live transcript, transcript bubble, accepted status,
    /// and streamed VoiceReply playback all apply per utterance unchanged.
    /// Requires a live connection (no HTTP fallback — hands-free over batch
    /// upload makes no sense). Push-to-talk is untouched.
    public func startConversation() async {
        guard currentAgent != nil, currentConversation != nil else { return }
        guard !isConversationActive else { return }
        guard !isStreamingVoice, !voiceController.isRecording, !voiceController.isListening,
            !voiceController.isCapturingPCM
        else { return }
        guard case .connected = connectionState else {
            lastError = "Conversation mode needs a live connection."
            return
        }

        guard let frames = await voiceController.startPCMStreaming() else { return }

        vad = VoiceActivityDetector()
        conversationStreamId = nil
        conversationChunkSeq = 0
        liveTranscript = nil
        pendingVoiceBubble = nil
        earlyFinalTranscripts.removeAll()
        isConversationActive = true

        conversationTask = Task { [weak self] in
            for await frame in frames {
                guard !Task.isCancelled, let self else { return }
                await self.handleConversationFrame(frame)
            }
        }
    }

    /// Ends conversation mode. An open (mid-speech) utterance is discarded
    /// (`cancel: true`) rather than submitted as a half phrase.
    public func stopConversation() async {
        guard isConversationActive else { return }
        isConversationActive = false

        if let streamId = conversationStreamId {
            conversationStreamId = nil
            try? await edgeClient.send(.audioStreamEnd(streamId: streamId, cancel: true))
        }
        voiceController.stopPCMStreaming()  // finishes the frame stream → task loop ends
        conversationTask?.cancel()
        conversationTask = nil
        activeVoiceStreamId = nil
        liveTranscript = nil
    }

    private func handleConversationFrame(_ frame: Data) async {
        guard isConversationActive else { return }
        // Stricter (sustained) onset while agent audio is audible or queued,
        // to resist residual echo triggering false barge-ins.
        let agentAudioActive = voiceController.hasPendingReplyAudio
        for event in vad.process(frame: frame, requireSustainedOnset: agentAudioActive) {
            switch event {
            case .utteranceStarted(let preRollFrames):
                if agentAudioActive {
                    performBargeIn()
                }
                await beginConversationUtterance(preRollFrames: preRollFrames)
            case .utteranceContinued(let frame):
                await sendConversationFrame(frame)
            case .utteranceEnded(let valid):
                await endConversationUtterance(valid: valid)
            }
        }
    }

    /// Client-side barge-in: kill the agent's audio locally and drop the
    /// rest of its reply chunks. Nothing is sent server-side (server-side
    /// cancel is a later slice) — the interrupted turn's remaining
    /// VoiceReply frames still arrive and are dropped via
    /// `suppressedVoiceReplyConversations` until the NEXT turn's Final
    /// TurnEvent clears the suppression (this also covers the case where
    /// the interrupted turn's chunk 0 hadn't even arrived yet, which the
    /// stale-key mechanism alone would miss).
    private func performBargeIn() {
        voiceController.stopPlayback()
        voiceController.cancelFallback()
        if let conversation = currentConversation {
            cancelScheduledFallback(for: conversation.conversationId)
            suppressedVoiceReplyConversations.insert(conversation.conversationId)
        }
        activeChunkedReplyKey = nil
    }

    private func beginConversationUtterance(preRollFrames: [Data]) async {
        guard let target = currentAgent, let conversation = currentConversation else { return }
        let streamId = UUID().uuidString
        conversationStreamId = streamId
        conversationChunkSeq = 0
        activeVoiceStreamId = streamId  // routes transcript_partial → liveTranscript
        liveTranscript = nil

        do {
            try await edgeClient.send(
                .audioStreamStart(
                    streamId: streamId,
                    targetNodeId: target.targetNodeId,
                    targetAgentId: target.targetAgentId,
                    conversationId: conversation.conversationId,
                    mimeType: VoiceController.pcmStreamMimeType
                )
            )
            // Pre-roll first, so the utterance's first words aren't clipped.
            for frame in preRollFrames {
                try await sendConversationChunk(frame, streamId: streamId)
            }
        } catch {
            await failConversation(error)
        }
    }

    private func sendConversationFrame(_ frame: Data) async {
        guard let streamId = conversationStreamId else { return }
        do {
            try await sendConversationChunk(frame, streamId: streamId)
        } catch {
            await failConversation(error)
        }
    }

    private func sendConversationChunk(_ frame: Data, streamId: String) async throws {
        try await edgeClient.send(
            .audioChunk(
                streamId: streamId,
                chunkSeq: conversationChunkSeq,
                dataBase64: frame.base64EncodedString()
            )
        )
        conversationChunkSeq += 1
    }

    private func endConversationUtterance(valid: Bool) async {
        guard let streamId = conversationStreamId else { return }
        conversationStreamId = nil
        activeVoiceStreamId = nil

        do {
            // Too-short utterances (coughs) are discarded server-side.
            try await edgeClient.send(.audioStreamEnd(streamId: streamId, cancel: !valid))
            if valid {
                await concludeVoiceUtterance(streamId: streamId)
            } else {
                earlyFinalTranscripts[streamId] = nil
            }
        } catch {
            await failConversation(error)
        }
        liveTranscript = nil
    }

    /// A send failed mid-conversation (WS died): surface it and shut the
    /// mode down — the reconnect triggers will restore the connection, and
    /// the operator can re-enter conversation mode.
    private func failConversation(_ error: Error) async {
        guard isConversationActive else { return }
        lastError = "Conversation stream failed: \(error.localizedDescription)"
        await stopConversation()
    }

    /// - Parameters:
    ///   - content: The wire content of the turn (may be empty for
    ///     blob-attached voice turns — the hotel transcribes the blob).
    ///   - displayText: What to show in the local operator bubble; defaults
    ///     to `content`.
    ///   - blobRefs: Hotel blob attachments riding the turn.
    private func submitTurn(
        content: String,
        displayText: String? = nil,
        messageKind: String?,
        blobRefs: [BlobRef] = []
    ) async {
        guard let target = currentAgent, var conversation = currentConversation else { return }
        let hasContent = !content.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        guard hasContent || !blobRefs.isEmpty else { return }

        let operatorMessage = ChatMessage(role: .operatorUser, content: displayText ?? content)
        conversation.messages.append(operatorMessage)
        currentConversation = conversation
        await conversationStore.upsert(conversation)

        do {
            try await edgeClient.send(
                .turnSubmit(
                    targetNodeId: target.targetNodeId,
                    targetAgentId: target.targetAgentId,
                    conversationId: conversation.conversationId,
                    content: content,
                    blobRefs: blobRefs,
                    messageKind: messageKind
                )
            )
        } catch {
            voiceExpectedConversations.remove(conversation.conversationId)
            lastError = "Send failed: \(error.localizedDescription)"
            appendSystemMessage("Send failed: \(error.localizedDescription)", isError: true)
        }
    }

    private func handleInbound(_ message: EdgeMessage) async {
        switch message {
        case .turnEvent(let conversationId, let eventKind, let content, _):
            await applyTurnEvent(conversationId: conversationId, eventKind: eventKind, content: content)

        case .voiceReply(
            let conversationId, let turnId, let audioBase64, let mimeType, _,
            let chunkSeq, _):
            // Barge-in suppression: reply audio for a turn the user talked
            // over is dropped entirely (chunked AND whole) — the next turn's
            // Final TurnEvent lifts the suppression before its reply arrives.
            if suppressedVoiceReplyConversations.contains(conversationId) {
                return
            }
            // Audio-only presentation: the matching Final `TurnEvent` carries
            // the text and lands in the transcript separately, so we do not
            // append another bubble here. The first frame (whole reply or
            // chunk 0) cancels any fallback TTS we scheduled.
            cancelScheduledFallback(for: conversationId)
            voiceExpectedConversations.remove(conversationId)
            if let chunkSeq {
                // Streamed per-sentence TTS: FIFO into the playback queue.
                // WS delivery is ordered, so chunk_seq is already monotonic
                // within a turn — the key only guards against interleave
                // ACROSS turns: a new turn's chunk 0 flushes anything stale,
                // and stragglers from a superseded turn are dropped.
                // `is_final` needs no handling: the queue simply drains.
                let replyKey = "\(conversationId)|\(turnId ?? "")"
                if chunkSeq == 0 {
                    activeChunkedReplyKey = replyKey
                    voiceController.resetReplyChunkQueue()
                    voiceController.enqueueReplyChunk(base64: audioBase64, mimeType: mimeType)
                } else if replyKey == activeChunkedReplyKey {
                    voiceController.enqueueReplyChunk(base64: audioBase64, mimeType: mimeType)
                }
            } else {
                // Whole reply: stop-and-replace, exactly as before.
                activeChunkedReplyKey = nil
                voiceController.play(base64: audioBase64, mimeType: mimeType)
            }

        case .transcriptPartial(let streamId, _, let text, let isFinal):
            await handleTranscriptPartial(streamId: streamId, text: text, isFinal: isFinal)

        case .error(_, let errorMessage, let fatal):
            lastError = errorMessage
            if fatal {
                appendSystemMessage("Fatal error: \(errorMessage)", isError: true)
            }

        case .approvalRequest(_, let description, _):
            appendSystemMessage("Approval requested: \(description)", isError: false)

        default:
            break
        }
    }

    // MARK: - Realtime transcript routing

    /// Routes `transcript_partial` frames: while the stream is active,
    /// partials feed ``liveTranscript`` (rendered live in the input field);
    /// a final transcript replaces the sent turn's "🎤 Voice message"
    /// placeholder bubble — whether it arrives before or after
    /// `audio_stream_end`. Frames for unknown/stale streams are ignored.
    private func handleTranscriptPartial(streamId: String, text: String, isFinal: Bool) async {
        if streamId == activeVoiceStreamId {
            liveTranscript = text
            if isFinal {
                // Finalized before the user released the mic: remember it so
                // the bubble created at stream end carries the transcript.
                earlyFinalTranscripts[streamId] = text
            }
            return
        }

        // Stream already ended: only a final transcript is actionable (it
        // replaces the placeholder bubble); late non-final partials for a
        // finished stream are ignored.
        guard isFinal, let pending = pendingVoiceBubble, pending.streamId == streamId else { return }
        pendingVoiceBubble = nil
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }

        var conversation: Conversation
        var isCurrent = false
        if let current = currentConversation, current.conversationId == pending.conversationId {
            conversation = current
            isCurrent = true
        } else if let stored = await conversationStore.conversationMatching(
            conversationId: pending.conversationId)
        {
            conversation = stored
        } else {
            return
        }

        guard let index = conversation.messages.firstIndex(where: { $0.id == pending.messageId })
        else { return }
        conversation.messages[index].content = text
        if isCurrent {
            currentConversation = conversation
        }
        await conversationStore.upsert(conversation)
    }

    // MARK: - Voice fallback scheduling

    private func cancelScheduledFallback(for conversationId: String) {
        fallbackTasks[conversationId]?.cancel()
        fallbackTasks[conversationId] = nil
    }

    /// Called after a Final `TurnEvent` lands. If the turn was submitted as
    /// voice (or the operator wants every reply spoken), schedules
    /// `speakFallback` after a 2.5s grace period for the server's
    /// `VoiceReply` to arrive — a `VoiceReply` for this conversation, or a
    /// new turn superseding it, cancels the timer first.
    private func scheduleVoiceFallbackIfNeeded(conversationId: String, text: String) {
        let wasExpectingVoice = voiceExpectedConversations.remove(conversationId) != nil
        guard wasExpectingVoice || speakAllReplies else { return }
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }

        cancelScheduledFallback(for: conversationId)
        fallbackTasks[conversationId] = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 2_500_000_000)
            guard !Task.isCancelled, let self else { return }
            self.fallbackTasks[conversationId] = nil
            self.voiceController.speakFallback(text: text)
        }
    }

    private func applyTurnEvent(conversationId: String, eventKind: TurnEventKind, content: String) async {
        // Route the event to the conversation that OWNS it, not just the one
        // on screen: replies that finish streaming after the user switches
        // agents must still be persisted to their conversation's history
        // (the local store is the source of truth — there is no server-side
        // hydration to recover a dropped reply from).
        var conversation: Conversation
        var isCurrent = false
        if let current = currentConversation, current.conversationId == conversationId {
            conversation = current
            isCurrent = true
        } else if let stored = await conversationStore.conversationMatching(conversationId: conversationId) {
            conversation = stored
        } else {
            // No local conversation claims this id — nothing to attach it to.
            return
        }

        var finalSpokenText: String?

        switch eventKind {
        case .token:
            if let last = conversation.messages.last, last.role == .agent, last.isStreaming {
                var updated = last
                updated.content += content
                conversation.messages[conversation.messages.count - 1] = updated
            } else {
                conversation.messages.append(ChatMessage(role: .agent, content: content, isStreaming: true))
            }

        case .final:
            // A Final always belongs to the newest turn: lift any barge-in
            // suppression so THIS turn's voice reply can play.
            suppressedVoiceReplyConversations.remove(conversationId)
            let spokenText: String
            if let last = conversation.messages.last, last.role == .agent, last.isStreaming {
                var updated = last
                updated.content = content.isEmpty ? updated.content : content
                updated.isStreaming = false
                conversation.messages[conversation.messages.count - 1] = updated
                spokenText = updated.content
            } else {
                conversation.messages.append(ChatMessage(role: .agent, content: content, isStreaming: false))
                spokenText = content
            }
            finalSpokenText = spokenText

        case .status:
            break

        case .error:
            conversation.messages.append(ChatMessage(role: .agent, content: content, isStreaming: false, isError: true))
            voiceExpectedConversations.remove(conversationId)
            cancelScheduledFallback(for: conversationId)
        }

        if isCurrent {
            currentConversation = conversation
        }
        await conversationStore.upsert(conversation)

        if let finalSpokenText {
            scheduleVoiceFallbackIfNeeded(conversationId: conversationId, text: finalSpokenText)
        }
    }

    private func appendSystemMessage(_ text: String, isError: Bool) {
        guard var conversation = currentConversation else { return }
        conversation.messages.append(ChatMessage(role: .system, content: text, isError: isError))
        currentConversation = conversation
        Task { await conversationStore.upsert(conversation) }
    }

    // MARK: - Enrollment

    public func enroll(inviteCode: String) async {
        guard let anchorURL = settings.anchorURL else {
            lastError = "Set the anchor URL before enrolling."
            return
        }
        let client = EnrollmentClient(baseURL: anchorURL)
        let request = EnrollmentRequest(
            inviteCode: inviteCode,
            devicePubkeyB64: DeviceIdentity.publicKeyBase64(),
            deviceName: settings.deviceName,
            platform: DeviceIdentity.platform
        )
        do {
            let response = try await client.enroll(request)
            settings.nodeId = response.nodeId
            settings.edgeToken = response.edgeToken
            lastError = nil
        } catch {
            lastError = "Enrollment failed: \(error)"
        }
    }
}
