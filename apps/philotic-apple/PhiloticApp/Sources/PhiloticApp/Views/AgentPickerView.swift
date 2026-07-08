// AgentPickerView.swift
// Picks which mesh agent to chat with. v0 uses the hardcoded
// `AgentTarget.builtIn` catalog — see the TODO on that type for the live
// `GET /api/mesh/targets/:node/agents` fetch this should become.

import SwiftUI

struct AgentPickerView: View {
    @Bindable var session: ChatSessionManager
    let onSelect: (AgentTarget) -> Void

    var body: some View {
        List(AgentTarget.builtIn) { target in
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
