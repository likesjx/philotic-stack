// ConnectionSettingsView.swift
// Anchor URL + bearer token entry, plus the one-time invite-code enroll
// flow (POST /api/edge/enroll via EnrollmentClient).

import SwiftUI
import PhiloticKit

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
                Picker("Speaking provider", selection: $session.speakingProvider) {
                    ForEach(VoiceProviderPreference.allCases, id: \.self) { provider in
                        Text(provider.displayName).tag(provider)
                    }
                }
                if let explanation = session.voiceProviderExplanation {
                    Text(explanation).font(.caption).foregroundStyle(.secondary)
                }
                Picker("Apple voice", selection: $session.appleVoiceIdentifier) {
                    Text("System voice").tag("")
                    ForEach(session.voiceController.appleSpeechProfiles) { profile in
                        Text("\(profile.name) (\(profile.language))").tag(profile.id)
                    }
                }
                Button("Preview Apple voice") { session.previewAppleVoice() }
                Button("Stop speech") { session.interruptVoiceReply() }
                    .disabled(!session.voiceController.hasPendingReplyAudio)
                Text("Dictation stays on this device. Hands-free transcription and external voice profiles are unavailable until their privacy policy can be verified.")
                    .font(.caption).foregroundStyle(.secondary)
                if let status = session.voiceStatus {
                    Text(status).font(.caption).foregroundStyle(.secondary)
                }
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
