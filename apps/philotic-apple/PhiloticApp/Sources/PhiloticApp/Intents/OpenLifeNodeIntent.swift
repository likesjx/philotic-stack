// OpenLifeNodeIntent.swift
// "Open this life graph entry" — the action half of the entity index plane
// (seam: apple-entity-index-plane, Slice A).
//
// This adopts the stable `OpenIntent` protocol rather than an App Schema
// macro. Rationale: the `system` schema domain's `Search` intent is deprecated
// (verified against Apple's docs 2026-07-26), and discovery is carried by
// IndexedEntity + Spotlight instead. `OpenIntent` is the long-stable
// expression of "open this content", it is what Spotlight invokes when the
// operator taps a donated result, and it does not depend on macro spellings
// that moved between iOS 18 assistant schemas and iOS 27 App Schemas.
//
// Layering an App Schema conformance on top is a follow-up, tracked in
// APPLE_INTELLIGENCE_PLANE_PROPOSAL.md Slice B.

import AppIntents
import Foundation

@available(iOS 18.0, macOS 15.0, *)
struct OpenLifeNodeIntent: OpenIntent {
    static var title: LocalizedStringResource { "Open Life Graph Entry" }

    static var description: IntentDescription {
        IntentDescription(
            "Opens a Life Graph entry in Philotic, with its provenance and typed neighbours.",
            categoryName: "Life Graph"
        )
    }

    /// `OpenIntent` requires the parameter to be named `target`.
    @Parameter(title: "Entry")
    var target: LifeNodeEntity

    @MainActor
    func perform() async throws -> some IntentResult {
        LifeIntentRouter.shared.requestOpen(nodeId: target.id)
        return .result()
    }
}

/// Bridges intent invocations into the SwiftUI navigation state.
///
/// Intents are performed outside the view hierarchy, so they cannot push
/// navigation directly. The router holds the request; `RootView` observes it
/// and presents the Life surface focused on the requested node.
@MainActor
@Observable
final class LifeIntentRouter {
    static let shared = LifeIntentRouter()

    /// Node the system asked us to open, consumed by `RootView`.
    private(set) var pendingNodeId: String?

    private init() {}

    func requestOpen(nodeId: String) {
        pendingNodeId = nodeId
    }

    /// Called by the view once it has presented the node, so a later
    /// re-render does not re-trigger navigation.
    func consume() {
        pendingNodeId = nil
    }
}
