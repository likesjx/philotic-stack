// LifeView.swift
// The Life surface: lens-first LifeGraph browsing. A lens is one named
// life.recall retrieval strategy rendered as a native list — every row is a
// graph node with provenance chips, tap-through to node detail. The canvas
// view comes in a later slice; the graph is for acting, not admiring.

import PhiloticKit
import SwiftUI

struct LifeView: View {
    @Bindable var session: ChatSessionManager
    @State private var contextText = ""

    private var store: LifeGraphStore { session.lifeGraph }

    var body: some View {
        Group {
            if session.lifeGraphCredentials() == nil {
                ContentUnavailableView(
                    "Not connected",
                    systemImage: "brain",
                    description: Text("Configure and connect to a hotel to browse the LifeGraph.")
                )
            } else {
                lensList
            }
        }
        .navigationTitle("Life")
        .task {
            store.markChangesSeen()
            await refresh()
        }
    }

    private var lensList: some View {
        List {
            Section {
                Picker("Lens", selection: Binding(
                    get: { store.selectedLens },
                    set: { newLens in
                        store.selectedLens = newLens
                        Task { await refresh() }
                    }
                )) {
                    ForEach(LifeLens.allCases) { lens in
                        Label(lens.title, systemImage: lens.systemImage).tag(lens)
                    }
                }
                .pickerStyle(.menu)

                if store.selectedLens == .reentry {
                    TextField("Where was I on…", text: $contextText)
                        .onSubmit { Task { await refresh() } }
                }
            }

            if let error = store.lastError {
                Section {
                    Label(error, systemImage: "exclamationmark.triangle")
                        .font(.caption)
                        .foregroundStyle(.orange)
                }
            }

            Section {
                if store.isLoading && store.packets.isEmpty {
                    ProgressView("Recalling…")
                } else if store.packets.isEmpty {
                    ContentUnavailableView(
                        "Nothing surfaced",
                        systemImage: store.selectedLens.systemImage,
                        description: Text("The \(store.selectedLens.title) lens came back empty.")
                    )
                } else {
                    ForEach(store.packets) { ranked in
                        NavigationLink(
                            destination: LifeNodeDetailView(
                                session: session, nodeId: ranked.packet.claimRef.id)
                        ) {
                            LensRow(ranked: ranked)
                        }
                    }
                }
            } footer: {
                if let refreshed = store.lastRefreshed {
                    Text("Updated \(refreshed.formatted(date: .omitted, time: .shortened))")
                }
            }

            if !store.recentChanges.isEmpty {
                Section("Recent changes") {
                    ForEach(store.recentChanges.prefix(5)) { change in
                        NavigationLink(
                            destination: LifeNodeDetailView(session: session, nodeId: change.nodeId)
                        ) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(change.summary ?? change.nodeId)
                                    .font(.subheadline)
                                    .lineLimit(2)
                                HStack(spacing: 6) {
                                    ProvenanceChip(text: change.changeKind, tint: .blue)
                                    if let label = change.label {
                                        ProvenanceChip(text: label, tint: .secondary)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        .refreshable { await refresh() }
    }

    private func refresh() async {
        guard let (baseURL, token) = session.lifeGraphCredentials() else { return }
        let context = contextText.trimmingCharacters(in: .whitespaces)
        await store.refresh(
            baseURL: baseURL,
            bearerToken: token,
            context: context.isEmpty ? nil : context
        )
    }
}

private struct LensRow: View {
    let ranked: LifeRankedPacket

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(ranked.packet.claimSummary)
                .font(.body)
                .lineLimit(3)
            HStack(spacing: 6) {
                ProvenanceChip(text: ranked.packet.claimRef.label, tint: .secondary)
                ProvenanceChip(
                    text: ranked.packet.validationState,
                    tint: ranked.packet.validationState == "confirmed" ? .green : .orange
                )
                ProvenanceChip(
                    text: "\(Int((ranked.packet.confidence * 100).rounded()))%",
                    tint: .secondary
                )
            }
        }
        .padding(.vertical, 2)
    }
}

/// Small capsule chip used for provenance facts (label, validation state,
/// confidence, source membrane).
struct ProvenanceChip: View {
    let text: String
    var tint: Color = .secondary

    var body: some View {
        Text(text)
            .font(.caption2)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(tint.opacity(0.15), in: Capsule())
            .foregroundStyle(tint)
    }
}

// MARK: - Node detail

struct LifeNodeDetailView: View {
    @Bindable var session: ChatSessionManager
    let nodeId: String

    @State private var detail: LifeNodeDetail?
    @State private var loadError: String?

    /// Provenance-envelope keys rendered in their own section (and therefore
    /// excluded from the generic properties list).
    private static let provenanceKeys: Set<String> = [
        "validation_state", "confidence", "source_membrane", "observed_at",
        "last_confirmed_at", "observed_by", "observed_role", "provenance",
        "origin_engram_id", "origin_trust",
    ]

    var body: some View {
        List {
            if let detail, let node = detail.node {
                nodeSections(node: node, neighbors: detail.neighbors)
            } else if let loadError {
                Label(loadError, systemImage: "exclamationmark.triangle")
                    .foregroundStyle(.orange)
            } else if detail != nil {
                ContentUnavailableView(
                    "Not found",
                    systemImage: "questionmark.circle",
                    description: Text("The hotel has no node with id \(nodeId).")
                )
            } else {
                ProgressView("Loading…")
            }
        }
        .navigationTitle(detail?.node?.string("title") ?? detail?.node?.primaryLabel ?? nodeId)
        .task(id: nodeId) { await load() }
    }

    @ViewBuilder
    private func nodeSections(node: LifeGraphNode, neighbors: [LifeNodeNeighbor]) -> some View {
        Section {
            if let title = node.string("title") ?? node.string("claim_summary") {
                Text(title).font(.headline)
            }
            HStack(spacing: 6) {
                ForEach(node.labels, id: \.self) { label in
                    ProvenanceChip(text: label, tint: .blue)
                }
                if let state = node.string("validation_state") {
                    ProvenanceChip(text: state, tint: state == "confirmed" ? .green : .orange)
                }
            }
        }

        Section("Provenance") {
            ForEach(Self.provenanceKeys.sorted(), id: \.self) { key in
                if let value = node.string(key) {
                    LabeledContent(key.replacingOccurrences(of: "_", with: " "), value: value)
                        .font(.caption)
                }
            }
        }

        let otherKeys = node.properties.keys
            .filter { !Self.provenanceKeys.contains($0) && $0 != "id" }
            .sorted()
        if !otherKeys.isEmpty {
            Section("Properties") {
                ForEach(otherKeys, id: \.self) { key in
                    if let value = node.properties[key]?.displayString {
                        LabeledContent(key.replacingOccurrences(of: "_", with: " "), value: value)
                            .font(.caption)
                    }
                }
            }
        }

        let grouped = Dictionary(grouping: neighbors, by: \.relType)
        ForEach(grouped.keys.sorted(), id: \.self) { relType in
            Section(relType) {
                ForEach(Array((grouped[relType] ?? []).enumerated()), id: \.offset) { _, neighbor in
                    if let neighborNode = neighbor.node, let id = neighborNode.canonicalId {
                        NavigationLink(
                            destination: LifeNodeDetailView(session: session, nodeId: id)
                        ) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(
                                    neighborNode.string("title")
                                        ?? neighborNode.string("claim_summary") ?? id
                                )
                                .font(.subheadline)
                                .lineLimit(2)
                                if let label = neighborNode.primaryLabel {
                                    ProvenanceChip(text: label, tint: .secondary)
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    private func load() async {
        guard let (baseURL, token) = session.lifeGraphCredentials() else {
            loadError = "Not connected"
            return
        }
        do {
            detail = try await session.lifeGraph.nodeDetail(
                baseURL: baseURL, bearerToken: token, nodeId: nodeId)
            loadError = nil
        } catch {
            loadError = String(describing: error)
        }
    }
}
