// LifeGraphStore.swift
// Domain state for the Life surface: lens packets fetched from the edge
// LifeGraph read plane, plus live LifeGraphChange events pushed over the WS.
// The store is optimistic-but-truthful: lens data is server truth at
// lastRefreshed; changes since then only badge — the next refresh reconciles.

import Foundation
import PhiloticKit

/// The lenses the Life surface offers — a curated subset of the named
/// `life.recall` retrieval strategies (see LIFEGRAPH_LENSES server-side).
public enum LifeLens: String, CaseIterable, Identifiable, Sendable {
    case openLoops = "open_loops_by_context"
    case commitments = "commitments_approaching"
    case goals = "goals_and_next_actions"
    case reentry = "re_entry_context"

    public var id: String { rawValue }

    public var title: String {
        switch self {
        case .openLoops: return "Open Loops"
        case .commitments: return "Commitments"
        case .goals: return "Goals"
        case .reentry: return "Re-entry"
        }
    }

    public var systemImage: String {
        switch self {
        case .openLoops: return "circle.dashed"
        case .commitments: return "calendar.badge.clock"
        case .goals: return "target"
        case .reentry: return "arrow.uturn.backward.circle"
        }
    }
}

/// One LifeGraphChange frame received over the edge WS.
public struct LifeGraphChangeEvent: Identifiable, Equatable, Sendable {
    public let id = UUID()
    public let changeKind: String
    public let nodeId: String
    public let label: String?
    public let summary: String?
    public let receivedAt: Date

    public init(changeKind: String, nodeId: String, label: String?, summary: String?) {
        self.changeKind = changeKind
        self.nodeId = nodeId
        self.label = label
        self.summary = summary
        self.receivedAt = Date()
    }
}

@MainActor
@Observable
public final class LifeGraphStore {
    public private(set) var packets: [LifeRankedPacket] = []
    public var selectedLens: LifeLens = .openLoops
    public private(set) var isLoading = false
    public private(set) var lastError: String?
    public private(set) var lastRefreshed: Date?
    /// Changes pushed since the operator last looked at the Life surface —
    /// drives the badge on the Life toolbar button.
    public private(set) var unseenChangeCount = 0
    public private(set) var recentChanges: [LifeGraphChangeEvent] = []

    private let client = LifeGraphClient()

    public init() {}

    /// Fetch the selected lens. Errors surface in `lastError`; stale packets
    /// stay visible so a flaky network never blanks the list.
    public func refresh(baseURL: URL, bearerToken: String, context: String? = nil) async {
        isLoading = true
        lastError = nil
        defer { isLoading = false }
        do {
            let response = try await client.fetchLens(
                baseURL: baseURL,
                bearerToken: bearerToken,
                lens: selectedLens.rawValue,
                context: context,
                limit: 25
            )
            packets = response.data.contextPacket?.rankedPackets ?? []
            lastRefreshed = Date()
            if response.data.status != "ok" {
                lastError = "lens returned status \(response.data.status)"
            }
        } catch {
            lastError = String(describing: error)
        }
    }

    public func nodeDetail(
        baseURL: URL, bearerToken: String, nodeId: String
    ) async throws -> LifeNodeDetail {
        try await client.fetchNode(baseURL: baseURL, bearerToken: bearerToken, nodeId: nodeId)
    }

    /// Record a live LifeGraphChange frame (called from the WS inbound path).
    public func noteChange(kind: String, nodeId: String, label: String?, summary: String?) {
        unseenChangeCount += 1
        recentChanges.insert(
            LifeGraphChangeEvent(changeKind: kind, nodeId: nodeId, label: label, summary: summary),
            at: 0
        )
        if recentChanges.count > 20 {
            recentChanges.removeLast(recentChanges.count - 20)
        }
    }

    /// The operator opened the Life surface — clear the badge.
    public func markChangesSeen() {
        unseenChangeCount = 0
    }
}
