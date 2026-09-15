#if os(macOS)
import SwiftUI

struct NotchView: View {
    @Bindable var session: ChatSessionManager
    @Bindable var router: CompanionRouter
    @Bindable var controller: NotchController
    let openMain: () -> Void
    @State private var pane = 0

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Button {
                    controller.expanded.toggle()
                } label: {
                    HStack(spacing: 8) {
                        Image(systemName: "waveform").foregroundStyle(.cyan)
                        Text("Philotic").font(.headline)
                        Image(systemName: controller.expanded ? "chevron.up" : "chevron.down").font(.caption2)
                    }
                    .frame(maxWidth: .infinity)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain).accessibilityLabel(controller.expanded ? "Collapse companion" : "Expand companion")
                if controller.expanded {
                    Button { openMain(); controller.expanded = false } label: {
                        Image(systemName: "arrow.up.right.square")
                    }.help("Open full app").accessibilityLabel("Open full app")
                    Button { controller.expanded = false } label: { Image(systemName: "xmark") }
                        .help("Dismiss panel; hover near the notch to reopen").accessibilityLabel("Dismiss companion")
                }
            }
            .padding(.horizontal, 14).frame(height: 38)
            VStack(spacing: 0) {
                Picker("Companion view", selection: $pane) {
                    Text("Ask").tag(0)
                    Text("Today").tag(1)
                }
                .pickerStyle(.segmented).padding(.horizontal, 16).padding(.bottom, 10)
                if pane == 0 {
                    VStack(spacing: 8) {
                        Menu {
                            ForEach(session.agents) { agent in
                                Button(agent.displayName) { session.currentAgent = agent }
                            }
                        } label: {
                            Label(session.currentAgent?.displayName ?? "Choose an agent", systemImage: "person.crop.circle")
                        }
                        .padding(.horizontal, 16)
                        if session.currentAgent != nil {
                            ChatView(session: session)
                        } else {
                            ContentUnavailableView("A thought away.", systemImage: "bubble.left.and.bubble.right",
                                description: Text("Choose an agent above to continue your conversation."))
                        }
                    }
                } else {
                    CompanionDashboard(session: session, router: router, compact: true)
                }
                ConnectionStatusBar(endpointName: session.selectedEndpointName, state: session.connectionState) {
                    Task { await session.connect() }
                }
            }
        }
        // Keep the expanded layout and chat draft mounted while the AppKit
        // shell animates. Content is clipped, not reflowed into a tiny notch.
        .padding(.top, controller.contentTopInset)
        .frame(width: controller.contentSize.width, height: controller.contentSize.height, alignment: .top)
        .opacity(controller.expanded ? 1 : 0)
        .allowsHitTesting(controller.expanded)
        .accessibilityHidden(!controller.expanded)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(Color.black)
        .clipShape(UnevenRoundedRectangle(bottomLeadingRadius: 24, bottomTrailingRadius: 24))
        .ignoresSafeArea()
        .preferredColorScheme(.dark)
        .onExitCommand { controller.expanded = false }
        .onChange(of: router.sheet) { _, sheet in
            if sheet != nil, controller.expanded { openMain(); controller.expanded = false }
        }
        .onChange(of: router.navigationID) { _, _ in
            if controller.expanded { openMain(); controller.expanded = false }
        }
    }
}
#endif
