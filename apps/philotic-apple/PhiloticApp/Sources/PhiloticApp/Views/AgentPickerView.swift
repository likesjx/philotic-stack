// AgentPickerView.swift
// Picks which mesh agent to chat with. Lists the live agent directory from
// the hotel (session.agents, fetched via GET /api/edge/agents after connect);
// the hardcoded catalog remains only as the pre-connect fallback.

import SwiftUI

struct AgentPickerView: View {
    @Bindable var session: ChatSessionManager
    let onSelect: (AgentTarget) -> Void

    var body: some View {
        List(session.agents) { target in
            Button {
                session.currentAgent = target
                onSelect(target)
            } label: {
                HStack {
                    VStack(alignment: .leading) {
                        Text(target.displayName)
                            .font(.headline)
                        Text("\(target.targetNodeId) / \(target.targetAgentId)")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                    if session.currentAgent == target {
                        Image(systemName: "checkmark")
                            .foregroundStyle(.tint)
                    }
                }
            }
            .buttonStyle(.plain)
        }
        .navigationTitle("Agents")
    }
}

#Preview {
    NavigationStack {
        AgentPickerView(session: ChatSessionManager(), onSelect: { _ in })
    }
}
