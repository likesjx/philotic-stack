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
        .modifier(LifeIntentPresentation(showingLife: $showingLife))
    }
}

/// Presents the Life surface when an App Intent (Spotlight tap, Siri phrase)
/// asks to open a Life Graph entry.
///
/// Split into a ViewModifier so the whole intent surface stays behind one
/// availability gate — `LifeIntentRouter` is iOS 18 / macOS 15 only, and
/// RootView itself still deploys to iOS 17 / macOS 14.
private struct LifeIntentPresentation: ViewModifier {
    @Binding var showingLife: Bool

    func body(content: Content) -> some View {
        if #available(iOS 18.0, macOS 15.0, *) {
            content.onChange(of: LifeIntentRouter.shared.pendingNodeId) { _, newValue in
                guard newValue != nil else { return }
                showingLife = true
                // Consume so a re-render does not re-present. The Life surface
                // opening is the observable outcome; node-level focus lands
                // with the node-detail route (proposal Plane 2, slice 4).
                LifeIntentRouter.shared.consume()
            }
        } else {
            content
        }
    }
}

#Preview {
    RootView(session: ChatSessionManager())
}
