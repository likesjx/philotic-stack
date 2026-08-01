// LifeIndexDonor.swift
// Performs the actual Spotlight donation for the entity index plane
// (seam: apple-entity-index-plane).
//
// The split of responsibilities is deliberate:
//   LifeIndexMapper (PhiloticKit) — decides WHAT may be indexed  [unit tested]
//   LifeIndexCache                — remembers what WAS indexed   [resolvable offline]
//   LifeIndexDonor (here)         — talks to CoreSpotlight       [thin, untestable]
//
// Keeping the untestable Apple-framework call thin is what lets the governance
// rules be verified without a device or simulator.

import Foundation
import OSLog
import PhiloticKit

#if canImport(CoreSpotlight)
    import CoreSpotlight
#endif

enum LifeIndexDonor {
    private static let log = Logger(subsystem: "com.philotic.apple", category: "life-index")

    /// Apply a governed plan to the system index and the local cache.
    ///
    /// Safe to call on any OS: on pre-iOS 18 / pre-macOS 15 the cache is still
    /// updated (so the app's own surfaces stay correct) and the Spotlight
    /// donation is skipped.
    static func apply(_ plan: LifeIndexPlan) async {
        guard !plan.isEmpty else { return }

        await LifeIndexCache.shared.apply(plan)

        #if canImport(CoreSpotlight)
            if #available(iOS 18.0, macOS 15.0, *) {
                await donateToSpotlight(plan)
            } else {
                log.debug("Spotlight donation skipped: requires iOS 18 / macOS 15")
            }
        #endif
    }

    /// Build a plan from lens packets and apply it. The single entry point
    /// callers should use after a lens refresh.
    static func applyLensPackets(_ packets: [LifeRankedPacket]) async {
        await apply(LifeIndexMapper.plan(from: packets))
    }

    /// Apply an incremental LifeGraphChange frame. Only removals act; see
    /// `LifeIndexMapper.plan(forChangeKind:...)` for why.
    static func applyChange(
        changeKind: String, nodeId: String, label: String?, summary: String?
    ) async {
        await apply(
            LifeIndexMapper.plan(
                forChangeKind: changeKind, nodeId: nodeId, label: label, summary: summary))
    }

    /// Remove everything this device has donated. Called when the operator
    /// disconnects or re-enrolls — a device that loses authorisation must stop
    /// answering with philotic content.
    static func purgeAll() async {
        #if canImport(CoreSpotlight)
            if #available(iOS 18.0, macOS 15.0, *) {
                do {
                    try await CSSearchableIndex.default().deleteAllSearchableItems()
                } catch {
                    log.error("Spotlight purge-all failed: \(error.localizedDescription)")
                }
            }
        #endif
        await LifeIndexCache.shared.clear()
    }

    #if canImport(CoreSpotlight)
        @available(iOS 18.0, macOS 15.0, *)
        private static func donateToSpotlight(_ plan: LifeIndexPlan) async {
            let index = CSSearchableIndex.default()

            if !plan.donate.isEmpty {
                let entities = plan.donate.map(LifeNodeEntity.init(snapshot:))
                do {
                    try await index.indexAppEntities(entities)
                    log.debug("Donated \(entities.count) life entities to Spotlight")
                } catch {
                    // Non-fatal: the cache still holds the snapshots, and the
                    // next lens refresh retries. Never surface as a user error.
                    log.error("Spotlight donation failed: \(error.localizedDescription)")
                }
            }

            if !plan.purge.isEmpty {
                do {
                    try await index.deleteAppEntities(
                        identifiedBy: plan.purge, ofType: LifeNodeEntity.self)
                    log.debug("Purged \(plan.purge.count) life entities from Spotlight")
                } catch {
                    log.error("Spotlight purge failed: \(error.localizedDescription)")
                }
            }
        }
    #endif
}
