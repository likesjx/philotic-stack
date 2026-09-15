// PhiloticApp.swift
// App entry point for the Philotic edge-mesh client (macOS + iOS).

import PhiloticKit
import SwiftUI
import AppIntents

@main
@MainActor
struct PhiloticApp: App {
    @State private var session: ChatSessionManager?
    @State private var router = CompanionRouter.shared
    #if os(macOS)
    @State private var notch = NotchController()
    #endif

    init() {
        #if DEBUG
            // Hosted unit tests must not load the operator's Keychain,
            // connect to a real hotel, or initialize device permissions.
            if NSClassFromString("XCTestCase") != nil {
                _session = State(initialValue: nil)
                return
            }
        #endif
        _session = State(initialValue: ChatSessionManager())
    }

    @ViewBuilder
    private var mainContent: some View {
        if let session {
            #if os(macOS)
            CompanionWindow(session: session, router: router, notch: notch)
            #else
            RootView(session: session, router: router)
            #endif
        } else {
            Color.clear
        }
    }

    var body: some Scene {
        #if os(macOS)
        // A single identified window makes notch/intent handoffs re-use the
        // existing app instead of creating another root and permission sheet.
        Window("Philotic", id: "main") { mainContent }
        .commands {
            CommandGroup(after: .windowArrangement) {
                Button("Toggle Companion") {
                    if notch.isVisible && notch.expanded { notch.expanded = false }
                    else { notch.show(); notch.expanded = true }
                }
                .keyboardShortcut("n", modifiers: [.command, .option])
            }
        }
        Settings {
            if let session { ConnectionSettingsView(session: session).frame(width: 520, height: 600) }
        }
        MenuBarExtra("Philotic", systemImage: "waveform") {
            Button(notch.isVisible ? "Hide companion" : "Show companion") {
                if notch.isVisible { notch.hide() } else { notch.show() }
            }
            OpenCompanionWindowButton(router: router)
            Divider()
            SettingsLink()
            Button("Quit Philotic") { NSApplication.shared.terminate(nil) }
                .keyboardShortcut("q")
        }
        #else
        WindowGroup { mainContent }
        #endif
    }
}

#if os(macOS)
private struct CompanionWindow: View {
    let session: ChatSessionManager
    let router: CompanionRouter
    let notch: NotchController
    @Environment(\.openWindow) private var openWindow
    var body: some View {
        RootView(session: session, router: router)
            .frame(minWidth: 620, minHeight: 600)
            .task {
                notch.configure(session: session, router: router) {
                    openWindow(id: "main")
                    NSApplication.shared.activate(ignoringOtherApps: true)
                }
                notch.show()
            }
    }
}

private struct OpenCompanionWindowButton: View {
    let router: CompanionRouter
    @Environment(\.openWindow) private var openWindow
    var body: some View {
        Button("Open Philotic") {
            openWindow(id: "main")
            NSApplication.shared.activate(ignoringOtherApps: true)
        }.keyboardShortcut("p", modifiers: [.command, .shift])
    }
}
#endif
