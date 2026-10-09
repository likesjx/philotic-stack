import Foundation

/// Local presentation identity. This is not a privacy grant or a tool rollback.
/// The server must echo requestID in its accepted status before a turn can speak.
public struct VoiceTurnGate: Sendable {
    public struct Scope: Equatable, Sendable {
        public let nodeID: String
        public let agentID: String
        public let conversationID: String

        public init(nodeID: String, agentID: String, conversationID: String) {
            self.nodeID = nodeID
            self.agentID = agentID
            self.conversationID = conversationID
        }
    }

    public struct Ticket: Equatable, Sendable {
        public let scope: Scope
        public let requestID: String
        public let generation: UUID
    }

    public struct Cancellation: Equatable, Sendable {
        public let ticket: Ticket
        /// Nil until accepted. The backend needs cancellation by requestID
        /// for an interrupt that races acceptance.
        public let turnID: String?
    }

    public private(set) var ticket: Ticket?
    public private(set) var turnID: String?
    public private(set) var hasReplyAudio = false
    private var finalReceived = false
    private var audioClosed = false
    private var nextChunk: UInt64 = 0

    public init() {}

    /// Call before any await in submission. A new generation revokes previous
    /// playback/timers even when the recipient is unchanged.
    @discardableResult
    public mutating func begin(scope: Scope) -> Ticket {
        invalidate()
        let newTicket = Ticket(scope: scope, requestID: UUID().uuidString, generation: UUID())
        ticket = newTicket
        return newTicket
    }

    /// Uncorrelated legacy/replayed acknowledgments cannot authorize speech.
    @discardableResult
    public mutating func accept(conversationID: String, requestID: String?, turnID: String?) -> Bool {
        guard let ticket, ticket.scope.conversationID == conversationID,
              let requestID, requestID == ticket.requestID,
              let turnID, !turnID.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return false
        }
        if let bound = self.turnID { return bound == turnID }
        self.turnID = turnID
        return true
    }

    public func matches(conversationID: String, turnID: String?) -> Bool {
        guard let ticket, ticket.scope.conversationID == conversationID,
              let bound = self.turnID, let turnID else { return false }
        return bound == turnID
    }

    public func isCurrent(_ candidate: Ticket) -> Bool { ticket == candidate }

    /// Final text is presented once; it never clears an interruption or binds
    /// a new turn. Audio may follow Final (the whole-reply protocol does so).
    @discardableResult
    public mutating func finish(conversationID: String, turnID: String?) -> Bool {
        guard matches(conversationID: conversationID, turnID: turnID), !finalReceived else { return false }
        finalReceived = true
        return true
    }

    /// Reject duplicate, skipped, stale and post-final chunks. A missing
    /// chunk fails the local audio stream closed instead of concatenating
    /// unrelated speech. Whole replies are accepted once.
    @discardableResult
    public mutating func receiveAudio(
        conversationID: String, turnID: String?, chunkSequence: UInt64?, isFinal: Bool?
    ) -> Bool {
        guard matches(conversationID: conversationID, turnID: turnID), !audioClosed else { return false }
        if let sequence = chunkSequence {
            guard sequence == nextChunk else {
                if sequence > nextChunk { audioClosed = true }
                return false
            }
            let (next, overflow) = nextChunk.addingReportingOverflow(1)
            nextChunk = next
            audioClosed = isFinal == true || overflow
        } else {
            guard !hasReplyAudio else { return false }
            audioClosed = true
        }
        hasReplyAudio = true
        return true
    }

    /// Used on barge-in, recipient/session change and transport reconnect.
    /// Late acceptance, Final and audio cannot restore the invalidated ticket.
    @discardableResult
    public mutating func invalidate() -> Cancellation? {
        let cancellation = ticket.map { Cancellation(ticket: $0, turnID: turnID) }
        ticket = nil
        turnID = nil
        hasReplyAudio = false
        finalReceived = false
        audioClosed = false
        nextChunk = 0
        return cancellation
    }
}
