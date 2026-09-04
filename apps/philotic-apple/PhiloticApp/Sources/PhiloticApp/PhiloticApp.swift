// PhiloticApp.swift
// App entry point for the Philotic edge-mesh client (macOS + iOS).

import PhiloticKit
import SwiftUI

@main
@MainActor
struct PhiloticApp: App {
    @State private var session: ChatSessionManager?

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

    var body: some Scene {
        WindowGroup {
            if let session {
                RootView(session: session)
            } else {
                Color.clear
            }
        }
    }
}
