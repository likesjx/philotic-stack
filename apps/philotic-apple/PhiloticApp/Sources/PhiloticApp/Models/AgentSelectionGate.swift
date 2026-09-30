import Foundation

/// A revision, not just an agent ID: an A → B → A switch must invalidate the
/// first A request as well as B. The session remains the sole state owner.
struct AgentSelectionGate {
    private var revision = UUID()

    mutating func begin() -> UUID {
        revision = UUID()
        return revision
    }

    func isCurrent(_ request: UUID) -> Bool { request == revision }
}
