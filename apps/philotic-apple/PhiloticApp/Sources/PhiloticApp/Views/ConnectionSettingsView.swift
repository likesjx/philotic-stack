// ConnectionSettingsView.swift
// Anchor URL + bearer token entry, plus the one-time invite-code enroll
// flow (POST /api/edge/enroll via EnrollmentClient).

import SwiftUI

struct ConnectionSettingsView: View {
    @Bindable var session: ChatSessionManager
    @State private var inviteCode: String = ""
    @State private var isEnrolling = false
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        Form {
            Section("Hotel") {
                TextField("Anchor URL (e.g. http://100.79.239.64:7700)", text: $session.settings.anchorURLString)
                    #if os(iOS)
                    .textInputAutocapitalization(.never)
                    .keyboardType(.URL)
                    #endif
                    .autocorrectionDisabled()
                TextField("Device name", text: $session.settings.deviceName)
            }

            Section("Credentials") {
                TextField("Node ID", text: $session.settings.nodeId)
                    .autocorrectionDisabled()
                SecureField("Edge bearer token", text: $session.settings.edgeToken)
            }

            Section("Voice") {
                Toggle("Speak replies", isOn: $session.speakAllReplies)
                Toggle("Transcribe on device", isOn: $session.transcribeOnDevice)
            }

            Section("Enroll a new device") {
                TextField("Invite code", text: $inviteCode)
                    .autocorrectionDisabled()
                Button {
                    Task {
                        isEnrolling = true
                        await session.enroll(inviteCode: inviteCode)
                        isEnrolling = false
                    }
                } label: {
                    if isEnrolling {
                        ProgressView()
                    } else {
                        Text("Enroll")
                    }
                }
                .disabled(inviteCode.isEmpty || session.settings.anchorURL == nil || isEnrolling)
            }

            if let error = session.lastError {
                Section {
                    Text(error)
                        .foregroundStyle(.red)
                        .font(.caption)
                }
            }

            Section {
                Button("Connect") {
                    Task { await session.connect() }
                    dismiss()
                }
                .disabled(!session.settings.isConfigured)
            }
        }
        .navigationTitle("Connection Settings")
        #if os(iOS)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
            }
        #endif
    }
}

#Preview {
    NavigationStack {
        ConnectionSettingsView(session: ChatSessionManager())
    }
}
