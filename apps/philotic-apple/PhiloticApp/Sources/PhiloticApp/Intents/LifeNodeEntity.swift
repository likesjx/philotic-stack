// LifeNodeEntity.swift
// The AppEntity/IndexedEntity projection of a LifeGraph claim
// (seam: apple-entity-index-plane, Slice A).
//
// Conforming to IndexedEntity is what puts philotic memory into the Spotlight
// semantic index — which is how Apple Intelligence reaches our content with
// attribution back to this app. Everything donated here has already passed
// the governance filter in `LifeIndexMapper` (PhiloticKit), which is unit
// tested independently of Xcode.
//
// Availability: IndexedEntity is iOS 18 / macOS 15. The app deploys to
// iOS 17 / macOS 14, so the whole surface is @available-gated rather than
// forcing a deployment-target bump that would drop OS support.

import AppIntents
import CoreSpotlight
import Foundation
import PhiloticKit

@available(iOS 18.0, macOS 15.0, *)
struct LifeNodeEntity: IndexedEntity {
    static var typeDisplayRepresentation: TypeDisplayRepresentation {
        TypeDisplayRepresentation(
            name: "Life Graph Entry",
            numericFormat: "\(placeholder: .int) life graph entries"
        )
    }

    /// The canonical LifeGraph node id.
    var id: String

    @Property(title: "Summary")
    var summary: String

    @Property(title: "Kind")
    var label: String

    @Property(title: "Provenance")
    var provenance: String

    /// ISO-8601 instant the claim was observed, if the server supplied one.
    var observedAt: String?

    init(snapshot: LifeIndexSnapshot) {
        self.id = snapshot.id
        self.summary = snapshot.summary
        self.label = snapshot.label
        self.provenance = snapshot.provenanceLine
        self.observedAt = snapshot.observedAt
    }

    var displayRepresentation: DisplayRepresentation {
        DisplayRepresentation(
            title: "\(summary)",
            subtitle: "\(provenance)"
        )
    }

    /// Spotlight attributes. Provenance travels into the index as the
    /// content description so attribution is visible in the search result
    /// itself, not merely inside the app.
    var attributeSet: CSSearchableItemAttributeSet {
        let attributes = CSSearchableItemAttributeSet(contentType: .content)
        attributes.title = summary
        attributes.contentDescription = provenance
        attributes.displayName = summary
        attributes.kind = label
        // Keywords keep the graph label searchable ("goal", "commitment")
        // alongside the claim text itself.
        attributes.keywords = ["philotic", "life graph", label.lowercased()]
        if let observedAt, let date = ISO8601DateFormatter().date(from: observedAt) {
            attributes.contentCreationDate = date
        }
        return attributes
    }

    static var defaultQuery = LifeNodeEntityQuery()
}

/// Resolves entities for Spotlight, Siri, and Shortcuts.
///
/// Backed by `LifeIndexCache` rather than the network: the system may resolve
/// an entity long after the lens that produced it was fetched, potentially
/// with no connectivity to the mesh, and a query that blocks on an edge
/// round-trip would simply fail. The cache holds only what was already
/// donated, so it can never widen what the server released.
@available(iOS 18.0, macOS 15.0, *)
struct LifeNodeEntityQuery: EntityQuery {
    func entities(for identifiers: [String]) async throws -> [LifeNodeEntity] {
        let snapshots = await LifeIndexCache.shared.snapshots(ids: identifiers)
        return snapshots.map(LifeNodeEntity.init(snapshot:))
    }

    func suggestedEntities() async throws -> [LifeNodeEntity] {
        let snapshots = await LifeIndexCache.shared.mostRecent(limit: 10)
        return snapshots.map(LifeNodeEntity.init(snapshot:))
    }
}

@available(iOS 18.0, macOS 15.0, *)
extension LifeNodeEntityQuery: EntityStringQuery {
    /// Free-text lookup, so "show me my commitment about X" resolves without
    /// the operator knowing an opaque node id.
    func entities(matching string: String) async throws -> [LifeNodeEntity] {
        let snapshots = await LifeIndexCache.shared.search(text: string, limit: 20)
        return snapshots.map(LifeNodeEntity.init(snapshot:))
    }
}
