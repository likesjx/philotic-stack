import PhiloticKit
import SwiftUI

struct LifeExploreView: View {
    let session: ChatSessionManager
    let packets: [LifeRankedPacket]
    @State private var selectedID: String?
    @State private var detail: LifeNodeDetail?
    @State private var error: String?
    @State private var loading = false
    @State private var reload = UUID()

    private var nodeID: String? {
        if let selectedID, packets.contains(where: { $0.packet.claimRef.id == selectedID }) { return selectedID }
        return packets.first?.packet.claimRef.id
    }
    private var requestID: String {
        (session.lifeGraphCredentials()?.0.absoluteString ?? "") + "|" + (nodeID ?? "") + "|" + reload.uuidString
            + "|" + String(session.lifeGraph.lastRefreshed?.timeIntervalSince1970 ?? 0)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Explore your connections").font(.title2.bold())
            Text("Choose a starting point. Lines show stored relationships, not inferred similarities.")
                .font(.caption).foregroundStyle(.secondary)
            ScrollView(.horizontal) {
                HStack(spacing: 10) {
                    ForEach(packets) { ranked in
                        Button { selectedID = ranked.packet.claimRef.id } label: {
                            VStack(alignment: .leading, spacing: 6) {
                                Text(ranked.packet.claimSummary).font(.subheadline.bold()).lineLimit(3)
                                Text(ranked.packet.validationState).font(.caption).foregroundStyle(.secondary)
                            }.frame(width: 160, alignment: .leading).padding(12)
                                .background(nodeID == ranked.packet.claimRef.id ? Color.accentColor.opacity(0.2) : Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 14))
                        }.buttonStyle(.plain)
                            .accessibilityAddTraits(nodeID == ranked.packet.claimRef.id ? .isSelected : [])
                    }
                }
            }
            if loading { ProgressView("Loading connections…") }
            if let error {
                Text(error).font(.caption).foregroundStyle(.orange)
                Button("Retry connections", systemImage: "arrow.clockwise") { reload = UUID() }
            }
            if let detail, let node = detail.node, let id = node.canonicalId {
                NavigationLink(destination: LifeNodeDetailView(session: session, nodeId: id)) {
                    Label("Open details & actions", systemImage: "slider.horizontal.3")
                }
                if detail.neighbors.isEmpty {
                    ContentUnavailableView("No returned relationships", systemImage: "point.topleft.down.to.point.bottomright.curvepath",
                        description: Text("This node has no connections in this response. Its details are still available."))
                } else {
                    LifeRelationshipsView(session: session, node: node, neighbors: detail.neighbors)
                    DisclosureGroup("Relationship list") {
                        ForEach(Array(detail.neighbors.enumerated()), id: \.offset) { _, edge in
                            if let target = edge.node, let targetID = target.canonicalId {
                                NavigationLink(destination: LifeNodeDetailView(session: session, nodeId: targetID)) {
                                    VStack(alignment: .leading) {
                                        Text(target.string("title") ?? target.string("claim_summary") ?? targetID)
                                        Text(edge.relType).font(.caption).foregroundStyle(.secondary)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }.task(id: requestID) { await load() }
    }

    private func load() async {
        detail = nil; error = nil; loading = true
        guard let id = nodeID, let (url, token) = session.lifeGraphCredentials() else { loading = false; return }
        let requested = requestID
        defer { if requestID == requested { loading = false } }
        do {
            let value = try await session.lifeGraph.nodeDetail(baseURL: url, bearerToken: token, nodeId: id)
            guard !Task.isCancelled, requestID == requested else { return }
            detail = value
        } catch {
            guard !Task.isCancelled, requestID == requested else { return }
            self.error = "Could not load connections. Select another node or try again."
        }
    }
}
