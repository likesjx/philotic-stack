// RootView.swift
// Top-level navigation: agent list on the leading side, chat on the
// trailing side, with a toolbar entry point into ConnectionSettingsView.

import SwiftUI

struct RootView: View {
    @Bindable var session: ChatSessionManager
    @State private var showingSettings = false
    @State private var showingLife = false
    @State private var showingHealth = false
    @State private var health = HealthKitCaptureService()

    var body: some View {
        NavigationSplitView {
            AgentPickerView(session: session) { _ in }
                .toolbar {
                    ToolbarItem {
                        Button {
                            showingLife = true
                        } label: {
                            Image(systemName: "brain")
                        }
                        .badge(session.lifeGraph.unseenChangeCount)
                        .accessibilityLabel("Life graph")
                    }
                    ToolbarItem {
                        Button {
                            showingHealth = true
                        } label: {
                            Image(systemName: "heart.text.square")
                        }
                        .accessibilityLabel("Health sync")
                    }
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
        .sheet(isPresented: $showingLife) {
            NavigationStack {
                LifeView(session: session)
            }
            #if os(macOS)
                .frame(minWidth: 480, minHeight: 560)
            #endif
        }
        .sheet(isPresented: $showingHealth) {
            NavigationStack {
                HealthView(session: session, health: health)
            }
            #if os(macOS)
                .frame(minWidth: 480, minHeight: 560)
            #endif
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
