// ChatSessionManager.swift
// App-level orchestrator: owns connection settings, the EdgeClient
// connection, local conversation history, and per-agent chat state. Wires
// PhiloticKit's edge-mesh transport to the SwiftUI views.

import Foundation
import Observation
import PhiloticKit

@MainActor
@Observable
public final class ChatSessionManager {
    public var settings: ConnectionSettings {
        didSet { ConnectionSettingsStore.save(settings) }
    }

    public private(set) var connectionState: EdgeConnectionState = .disconnected
    public private(set) var selectedEndpointName: String?
    public private(set) var conversations: [Conversation] = []
    public private(set) var currentConversation: Conversation?
    public var currentAgent: AgentTarget? {
        didSet { if currentAgent != oldValue { Task { await selectAgent(currentAgent) } } }
    }
    public var lastError: String?

    private let edgeClient: EdgeClient
    private let endpointSelector: EndpointSelector
    private let conversationStore: ConversationStore
    private var streamTask: Task<Void, Never>?
    private var statePollTask: Task<Void, Never>?

    public init(
        edgeClient: EdgeClient = EdgeClient(),
        endpointSelector: EndpointSelector? = nil,
        conversationStore: ConversationStore = ConversationStore()
    ) {
        let loaded = ConnectionSettingsStore.load()
        self.settings = loaded
        self.edgeClient = edgeClient
        self.conversationStore = conversationStore
        if let endpointSelector {
            self.endpointSelector = endpointSelector
        } else if let anchorURL = loaded.anchorURL {
            self.endpointSelector = EndpointSelector(anchor: EndpointCandidate(name: "anchor", baseURL: anchorURL))
        } else {
            self.endpointSelector = EndpointSelector()
        }
    }

    // MARK: - Lifecycle

    public func loadConversations() async {
        conversations = await conversationStore.all()
    }

    /// Resolves the best endpoint and opens the edge connection.
    public func connect() async {
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
        } catch {
            lastError = "Connect failed: \(error.localizedDescription)"
        }
    }

    public func disconnect() async {
        streamTask?.cancel()
        streamTask = nil
        statePollTask?.cancel()
        statePollTask = nil
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
        guard let target = currentAgent, var conversation = currentConversation else { return }
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }

        let operatorMessage = ChatMessage(role: .operatorUser, content: text)
        conversation.messages.append(operatorMessage)
        currentConversation = conversation
        await conversationStore.upsert(conversation)

        do {
            try await edgeClient.send(
                .turnSubmit(
                    targetNodeId: target.targetNodeId,
                    targetAgentId: target.targetAgentId,
                    conversationId: conversation.conversationId,
                    content: text,
                    blobRefs: []
                )
            )
        } catch {
            lastError = "Send failed: \(error.localizedDescription)"
            appendSystemMessage("Send failed: \(error.localizedDescription)", isError: true)
        }
    }

    private func handleInbound(_ message: EdgeMessage) async {
        switch message {
        case .turnEvent(let conversationId, let eventKind, let content, _):
            await applyTurnEvent(conversationId: conversationId, eventKind: eventKind, content: content)

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
            if let last = conversation.messages.last, last.role == .agent, last.isStreaming {
                var updated = last
                updated.content = content.isEmpty ? updated.content : content
                updated.isStreaming = false
                conversation.messages[conversation.messages.count - 1] = updated
            } else {
                conversation.messages.append(ChatMessage(role: .agent, content: content, isStreaming: false))
            }

        case .status:
            break

        case .error:
            conversation.messages.append(ChatMessage(role: .agent, content: content, isStreaming: false, isError: true))
        }

        if isCurrent {
            currentConversation = conversation
        }
        await conversationStore.upsert(conversation)
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
