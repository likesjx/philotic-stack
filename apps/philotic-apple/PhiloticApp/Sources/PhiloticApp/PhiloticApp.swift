// PhiloticApp.swift
// App entry point for the Philotic edge-mesh client (macOS + iOS).

import PhiloticKit
import SwiftUI

@main
struct PhiloticApp: App {
    @State private var session = ChatSessionManager()

    var body: some Scene {
        WindowGroup {
            RootView(session: session)
        }
    }
}
