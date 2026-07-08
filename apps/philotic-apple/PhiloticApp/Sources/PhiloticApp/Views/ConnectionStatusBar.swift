// ConnectionStatusBar.swift
// Small status strip: which endpoint we're talking to, and whether we're
// connected, reconnecting, or disconnected.

import PhiloticKit
import SwiftUI

struct ConnectionStatusBar: View {
    let endpointName: String?
    let state: EdgeConnectionState

    var body: some View {
        HStack(spacing: 8) {
            Circle()
                .fill(color)
                .frame(width: 8, height: 8)
            Text(label)
                .font(.caption)
                .foregroundStyle(.secondary)
            if let endpointName {
                Text("· \(endpointName)")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            }
            Spacer()
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        #if os(macOS)
            .background(.regularMaterial)
        #else
            .background(.bar)
        #endif
    }

    private var label: String {
        switch state {
        case .disconnected: return "Disconnected"
        case .connecting: return "Connecting…"
        case .connected: return "Connected"
        case .reconnecting(let attempt): return "Reconnecting (attempt \(attempt))…"
        case .failed(let code, _): return "Connection rejected (\(code)) — re-enroll in Settings"
        }
    }

    private var color: Color {
        switch state {
        case .disconnected: return .gray
        case .connecting: return .yellow
        case .connected: return .green
        case .reconnecting: return .orange
        case .failed: return .red
        }
    }
}

#Preview {
    VStack {
        ConnectionStatusBar(endpointName: "anchor", state: .connected(sessionId: "abc"))
        ConnectionStatusBar(endpointName: "anchor", state: .reconnecting(attempt: 2))
        ConnectionStatusBar(endpointName: nil, state: .disconnected)
    }
}
