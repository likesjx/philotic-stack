// RootView.swift
// Top-level navigation: agent list on the leading side, chat on the
// trailing side, with a toolbar entry point into ConnectionSettingsView.

import SwiftUI

struct RootView: View {
    @Bindable var session: ChatSessionManager
    @State private var showingSettings = false

    var body: some View {
        NavigationSplitView {
            AgentPickerView(session: session) { _ in }
                .toolbar {
                    ToolbarItem {
                        Button {
                            showingSettings = true
                        } label: {
                            Image(systemName: "gearshape")
                        }
                    }
                }
        } detail: {
            if session.currentAgent != nil {
                ChatView(session: session)
            } else {
                ContentUnavailableView(
                    "Pick an agent",
                    systemImage: "bubble.left.and.bubble.right",
                    description: Text("Choose an agent from the list to start chatting.")
                )
            }
        }
        .sheet(isPresented: $showingSettings) {
            NavigationStack {
                ConnectionSettingsView(session: session)
            }
        }
        .task {
            await session.loadConversations()
            if session.settings.isConfigured {
                await session.connect()
            } else {
                showingSettings = true
            }
        }
    }
}

#Preview {
    RootView(session: ChatSessionManager())
}
