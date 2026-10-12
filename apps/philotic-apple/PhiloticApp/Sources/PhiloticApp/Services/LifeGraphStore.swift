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
    private let memoryClient = MemoryClient()

    /// Minimum spacing between Muninn index refreshes, independent of how
    /// often the Life surface reloads.
    static let memoryRefreshInterval: TimeInterval = 300
    private var lastMemoryIndexRefresh: Date?

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
            // Donate to the Spotlight semantic index (seam:
            // apple-entity-index-plane). Only packets the server already
            // released reach here, and LifeIndexMapper applies the
            // validation-state filter before anything is indexed. Fire and
            // forget: a Spotlight failure must never degrade the Life surface.
            let donated = packets
            Task.detached(priority: .utility) {
                await LifeIndexDonor.applyLensPackets(donated)
            }
            // Muninn rides the same refresh so both memory planes reach the
            // index together, but on its own failure path (see below).
            let memoryContext = context
            Task { [weak self] in
                await self?.refreshMemoryIndex(
                    baseURL: baseURL, bearerToken: bearerToken, context: memoryContext)
            }
        } catch {
            lastError = String(describing: error)
        }
    }

    /// Pull structured Muninn recall and mirror it into the Spotlight index
    /// (seam: apple-entity-index-plane, Muninn extension).
    ///
    /// Kept separate from `refresh` because the two planes fail independently:
    /// Muninn being unconfigured or unreachable must never blank the Life
    /// surface, and a lens error must not stop memory indexing. Failures are
    /// intentionally swallowed — this is a background index refresh, not an
    /// operator-visible action.
    public func refreshMemoryIndex(
        baseURL: URL, bearerToken: String, context: String? = nil, force: Bool = false
    ) async {
        // `refresh` is called on appear, on every lens switch, on submit and
        // on pull-to-refresh — four lenses would mean four identical Muninn
        // round-trips. The memory plane does not vary by lens, so throttle it
        // on its own cadence rather than the Life surface's.
        if !force, let last = lastMemoryIndexRefresh,
            Date().timeIntervalSince(last) < Self.memoryRefreshInterval
        {
            return
        }
        lastMemoryIndexRefresh = Date()
        do {
            let response = try await memoryClient.recall(
                baseURL: baseURL,
                bearerToken: bearerToken,
                context: context,
                maxResults: 25
            )
            guard response.status == "ok" else { return }
            await LifeIndexDonor.applyMemories(response.memories)
        } catch {
            // Muninn is optional on a hotel; absence is not an error state.
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
        // A retirement must evict the node from the system index immediately,
        // not wait for the next lens refresh — Spotlight would keep answering
        // with a claim the graph has retracted. Non-removal kinds carry no
        // provenance and are intentionally no-ops here.
        Task.detached(priority: .utility) {
            await LifeIndexDonor.applyChange(
                changeKind: kind, nodeId: nodeId, label: label, summary: summary)
        }
    }

    /// The operator opened the Life surface — clear the badge.
    public func markChangesSeen() {
        unseenChangeCount = 0
    }
}
