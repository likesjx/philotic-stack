import PhiloticKit
import SwiftUI

/// A bounded one-hop diagram. Arrowheads use the server's endpoints, never
/// inferred relationship names. The complete accessible list remains below.
struct LifeRelationshipsView: View {
    let session: ChatSessionManager
    let node: LifeGraphNode
    let neighbors: [LifeNodeNeighbor]

    private var visible: [LifeNodeNeighbor] { Array(neighbors.prefix(8)) }
    private let center = CGPoint(x: 290, y: 230)

    var body: some View {
        VStack(alignment: .leading) {
            ScrollView(.horizontal) {
                ZStack {
                    ForEach(Array(visible.enumerated()), id: \.offset) { index, edge in
                        let end = point(index)
                        let isOutgoing = edge.fromId == node.canonicalId && edge.toId != nil
                        let isIncoming = edge.toId == node.canonicalId && edge.fromId != nil
                        Path { p in
                            let dx = end.x - center.x, dy = end.y - center.y
                            let length = hypot(dx, dy)
                            let start = CGPoint(x: center.x + dx / length * 60, y: center.y + dy / length * 36)
                            let finish = CGPoint(x: end.x - dx / length * 62, y: end.y - dy / length * 36)
                            p.move(to: start); p.addLine(to: finish)
                            if isOutgoing || isIncoming {
                                let tip = isOutgoing ? finish : start
                                let angle = atan2(dy, dx) + (isIncoming ? .pi : 0)
                                p.move(to: CGPoint(x: tip.x - 10 * cos(angle - .pi / 6), y: tip.y - 10 * sin(angle - .pi / 6)))
                                p.addLine(to: tip)
                                p.addLine(to: CGPoint(x: tip.x - 10 * cos(angle + .pi / 6), y: tip.y - 10 * sin(angle + .pi / 6)))
                            }
                        }
                        .stroke(.secondary, lineWidth: 1.5)
                        Text(edge.relType.replacingOccurrences(of: "_", with: " "))
                            .font(.system(size: 9)).lineLimit(2).multilineTextAlignment(.center)
                            .padding(3).background(.regularMaterial, in: RoundedRectangle(cornerRadius: 4))
                            .frame(width: 95)
                            .position(x: (center.x + end.x) / 2, y: (center.y + end.y) / 2)
                        if let target = edge.node, let id = target.canonicalId {
                            NavigationLink(destination: LifeNodeDetailView(session: session, nodeId: id)) {
                                badge(target, selected: false)
                            }
                            .buttonStyle(.plain)
                            .accessibilityLabel("Open \(title(target)), \(edge.relType)")
                            .position(end)
                        } else {
                            Text("Unavailable node").font(.caption).position(end)
                        }
                    }
                    badge(node, selected: true).position(center)
                }
                .frame(width: 580, height: 460)
            }
            Text("One-hop view · \(visible.count) of \(neighbors.count) returned relationships. Arrows show stored direction. Select a node to explore.")
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    private func point(_ index: Int) -> CGPoint {
        let angle = Double(index) * 2 * Double.pi / Double(max(visible.count, 1)) - .pi / 2
        return CGPoint(x: center.x + cos(angle) * 215, y: center.y + sin(angle) * 175)
    }
    private func title(_ node: LifeGraphNode) -> String {
        node.string("title") ?? node.string("claim_summary") ?? node.canonicalId ?? "Node"
    }
    private func badge(_ node: LifeGraphNode, selected: Bool) -> some View {
        VStack(spacing: 3) {
            Text(title(node)).font(.caption).lineLimit(3)
            Text(node.primaryLabel ?? "Node").font(.caption2).foregroundStyle(.secondary)
        }
        .multilineTextAlignment(.center).padding(8).frame(width: 125, height: 70)
        .background(selected ? Color.accentColor.opacity(0.2) : Color.secondary.opacity(0.1), in: RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).stroke(selected ? Color.accentColor : Color.secondary.opacity(0.4)))
    }
}
