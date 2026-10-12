import SwiftUI

struct RootView: View {
    @Bindable var session: ChatSessionManager
    @Bindable var router: CompanionRouter
    @State private var health = HealthKitCaptureService()
    @State private var location = LocationCaptureService()
    @State private var agentColumn: NavigationSplitViewColumn = .sidebar

    var body: some View {
        VStack(spacing: 0) {
            #if os(iOS)
            connectionStatus
            #endif
        TabView(selection: $router.tab) {
            NavigationStack {
                CompanionDashboard(session: session, router: router).toolbar { settingsButton }
            }
            .tabItem { Label("Today", systemImage: "sparkles.rectangle.stack") }.tag(CompanionTab.today)
            NavigationSplitView(preferredCompactColumn: $agentColumn) {
                AgentPickerView(session: session) { _ in
                    agentColumn = .detail
                }
                .navigationSplitViewColumnWidth(min: 240, ideal: 280, max: 360)
                .toolbar { settingsButton }
            } detail: {
                if session.currentAgent != nil {
                    ChatView(session: session)
                } else {
                    ContentUnavailableView("Pick an agent", systemImage: "bubble.left.and.bubble.right",
                        description: Text("Choose an agent to start a conversation."))
                }
            }
            .navigationSplitViewStyle(.balanced)
            .tabItem { Label("Agents", systemImage: "bubble.left.and.bubble.right") }.tag(CompanionTab.agents)
            NavigationStack {
                LifeView(session: session).toolbar { settingsButton }
            }
            .tabItem { Label("Life", systemImage: "brain") }.tag(CompanionTab.life)
            NavigationStack { CortexView() }
            .tabItem { Label("Cortex", systemImage: "brain.head.profile") }.tag(CompanionTab.cortex)
        }
            #if os(macOS)
            connectionStatus
            #endif
        }
        .sheet(item: $router.sheet, onDismiss: { health.discardPreview() }) { destination in
            CompanionSheetView(destination: destination, session: session, health: health, location: location)
                #if os(macOS)
                .frame(minWidth: 480, idealWidth: 560, minHeight: 520)
                #endif
        }
        .task {
            await session.loadConversations()
            if session.settings.isConfigured {
                await session.connect()
            } else if router.sheet == nil {
                router.sheet = .settings
            }
            #if DEBUG
            if CommandLine.arguments.contains("--show-location") { router.sheet = .location }
            if CommandLine.arguments.contains("--show-health") { router.sheet = .health }
            if CommandLine.arguments.contains("--show-today") { router.open(.today) }
            #endif
        }
    }

    private var settingsButton: some View {
        Button { router.sheet = .settings } label: { Image(systemName: "gearshape") }
            .accessibilityLabel("Connection settings")
    }

    private var connectionStatus: some View {
        ConnectionStatusBar(endpointName: session.selectedEndpointName, state: session.connectionState) {
            Task { await session.connect() }
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

private struct CompanionSheetView: View {
    let destination: CompanionSheet
    let session: ChatSessionManager
    let health: HealthKitCaptureService
    let location: LocationCaptureService
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Group {
                switch destination {
                case .settings: ConnectionSettingsView(session: session)
                case .health: HealthView(session: session, health: health)
                case .location: LocationView(session: session, location: location)
                case .reminders: RemindersView()
                case .intelligence: LocalIntelligenceView()
                }
            }
            #if os(macOS)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } } }
            #endif
        }
    }
}
