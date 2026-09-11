// LifeIndexCache.swift
// Durable mirror of what this device has donated to the Spotlight index
// (seam: apple-entity-index-plane).
//
// Why this exists: the system resolves AppEntities on its own schedule —
// potentially at boot, offline, or long after the lens that produced them was
// fetched. An EntityQuery that reached for the network would simply fail in
// exactly those moments. This cache holds *only* snapshots that already
// passed `LifeIndexMapper`'s governance filter, so it can never widen what
// the server released; it just makes them resolvable without connectivity.
//
// Deliberately not @Observable and not @MainActor: this is touched from
// system-initiated query contexts, not from views.

import Foundation
import PhiloticKit

actor LifeIndexCache {
    static let shared = LifeIndexCache()

    private var snapshots: [String: LifeIndexSnapshot] = [:]
    private var loaded = false
    private let fileURL: URL?

    init(fileURL: URL? = LifeIndexCache.defaultFileURL()) {
        self.fileURL = fileURL
    }

    private static func defaultFileURL() -> URL? {
        guard
            let base = try? FileManager.default.url(
                for: .applicationSupportDirectory,
                in: .userDomainMask,
                appropriateFor: nil,
                create: true
            )
        else { return nil }
        let dir = base.appendingPathComponent("Philotic", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent("life-index-cache.json")
    }

    // MARK: - Reads

    func snapshots(ids: [String]) async -> [LifeIndexSnapshot] {
        load()
        return ids.compactMap { snapshots[$0] }
    }

    func mostRecent(limit: Int) async -> [LifeIndexSnapshot] {
        load()
        return
            snapshots
            .values
            .sorted { ($0.observedAt ?? "") > ($1.observedAt ?? "") }
            .prefix(limit)
            .map { $0 }
    }

    func search(text: String, limit: Int) async -> [LifeIndexSnapshot] {
        load()
        let needle = text.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !needle.isEmpty else { return [] }
        return
            snapshots
            .values
            .filter {
                $0.summary.lowercased().contains(needle)
                    || $0.label.lowercased().contains(needle)
            }
            .sorted { $0.score > $1.score }
            .prefix(limit)
            .map { $0 }
    }

    func count() async -> Int {
        load()
        return snapshots.count
    }

    /// Every namespaced index id this device has donated — the exact scope a
    /// purge should target, so it never reaches another slice's entities.
    func allIndexIds() async -> [String] {
        load()
        return snapshots.keys.sorted()
    }

    // MARK: - Writes

    /// Apply a governed plan: donations upsert, purges remove.
    func apply(_ plan: LifeIndexPlan) {
        load()
        for snapshot in plan.donate {
            // Keyed by namespaced indexId so LifeGraph and Muninn records
            // cannot collide, and so purges (which carry indexIds) hit.
            snapshots[snapshot.indexId] = snapshot
        }
        for id in plan.purge {
            snapshots.removeValue(forKey: id)
        }
        persist()
    }

    /// Drop everything — used when the operator disconnects or re-enrolls, so
    /// a device that loses authorisation stops resolving philotic content.
    func clear() {
        snapshots = [:]
        loaded = true
        persist()
    }

    // MARK: - Persistence

    private func load() {
        guard !loaded else { return }
        loaded = true
        guard let fileURL, let data = try? Data(contentsOf: fileURL) else { return }
        guard let decoded = try? JSONDecoder().decode([LifeIndexSnapshot].self, from: data) else {
            // A corrupt cache is recoverable: the next lens refresh rebuilds
            // it. Losing it is strictly better than failing to launch.
            return
        }
        snapshots = Dictionary(uniqueKeysWithValues: decoded.map { ($0.indexId, $0) })
    }

    private func persist() {
        guard let fileURL else { return }
        let ordered = snapshots.values.sorted { $0.indexId < $1.indexId }
        guard let data = try? JSONEncoder().encode(ordered) else { return }
        try? data.write(to: fileURL, options: .atomic)
    }
}
