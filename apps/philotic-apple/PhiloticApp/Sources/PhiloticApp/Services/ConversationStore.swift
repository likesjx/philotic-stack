// ConversationStore.swift
// Local-first conversation history. The server-side turn-history REST
// surface is still new (there is no `GET .../sessions/:id/turns` route
// today — see HistoryHydrator), so this JSON file store is the source of
// truth for chat history; hydration from the server is opportunistic.

import Foundation

/// Persists `Conversation` records to a single JSON file under Application
/// Support. An actor because it's mutated from the chat send/receive path.
public actor ConversationStore {
    private let fileURL: URL
    private var conversations: [Conversation]
    private var loaded = false

    public init(fileURL: URL? = nil) {
        self.fileURL = fileURL ?? Self.defaultFileURL()
        self.conversations = []
    }

    /// All conversations, most recently updated first. Loads from disk on
    /// first call.
    public func all() -> [Conversation] {
        loadIfNeeded()
        return conversations.sorted { $0.updatedAt > $1.updatedAt }
    }

    public func conversation(id: String) -> Conversation? {
        loadIfNeeded()
        return conversations.first { $0.id == id }
    }

    /// Looks a conversation up by its wire `conversationId` (the id turn
    /// events are tagged with), as opposed to the local record `id`.
    public func conversationMatching(conversationId: String) -> Conversation? {
        loadIfNeeded()
        return conversations.first { $0.conversationId == conversationId }
    }

    /// Finds or creates the conversation for a given agent target, reusing
    /// an existing one if `conversationId` is already known.
    public func findOrCreate(agentTarget: AgentTarget, conversationId: String?) -> Conversation {
        loadIfNeeded()
        if let conversationId, let existing = conversations.first(where: { $0.conversationId == conversationId }) {
            return existing
        }
        if let existing = conversations.first(where: { $0.agentTarget == agentTarget }) {
            return existing
        }
        let created = Conversation(
            agentTarget: agentTarget,
            conversationId: conversationId ?? UUID().uuidString
        )
        conversations.append(created)
        persist()
        return created
    }

    /// Replaces (or inserts) a conversation and persists to disk.
    public func upsert(_ conversation: Conversation) {
        loadIfNeeded()
        var updated = conversation
        updated.updatedAt = Date()
        if let index = conversations.firstIndex(where: { $0.id == conversation.id }) {
            conversations[index] = updated
        } else {
            conversations.append(updated)
        }
        persist()
    }

    public func appendMessage(conversationId: String, message: ChatMessage) {
        loadIfNeeded()
        guard let index = conversations.firstIndex(where: { $0.id == conversationId }) else { return }
        conversations[index].messages.append(message)
        conversations[index].updatedAt = Date()
        persist()
    }

    /// Updates the last message in place (used while streaming tokens in).
    public func replaceLastMessage(conversationId: String, with message: ChatMessage) {
        loadIfNeeded()
        guard let index = conversations.firstIndex(where: { $0.id == conversationId }),
            !conversations[index].messages.isEmpty
        else { return }
        conversations[index].messages[conversations[index].messages.count - 1] = message
        conversations[index].updatedAt = Date()
        persist()
    }

    // MARK: - Disk I/O

    private func loadIfNeeded() {
        guard !loaded else { return }
        loaded = true
        guard let data = try? Data(contentsOf: fileURL) else { return }
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        if let decoded = try? decoder.decode([Conversation].self, from: data) {
            conversations = decoded
        }
    }

    private func persist() {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        guard let data = try? encoder.encode(conversations) else { return }
        try? FileManager.default.createDirectory(
            at: fileURL.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try? data.write(to: fileURL, options: .atomic)
    }

    private static func defaultFileURL() -> URL {
        let base =
            FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
        return base.appendingPathComponent("PhiloticApp", isDirectory: true)
            .appendingPathComponent("conversations.json")
    }
}
