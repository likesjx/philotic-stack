// ChatView.swift
// Streaming chat surface for the currently selected agent: message list
// (live-rendering tokens as they arrive) plus a send box.

import SwiftUI

struct ChatView: View {
    @Bindable var session: ChatSessionManager
    @State private var draft: String = ""
    @FocusState private var inputFocused: Bool

    private var voice: VoiceController { session.voiceController }

    var body: some View {
        VStack(spacing: 0) {
            ConnectionStatusBar(
                endpointName: session.selectedEndpointName,
                state: session.connectionState,
                onReconnect: { Task { await session.connect() } }
            )

            if let voiceError = voice.voiceError {
                Text(voiceError)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .padding(.horizontal, 12)
                    .padding(.top, 4)
            }

            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 12) {
                        ForEach(session.currentConversation?.messages ?? []) { message in
                            MessageBubble(message: message)
                                .id(message.id)
                        }
                    }
                    .padding()
                }
                .onChange(of: session.currentConversation?.messages.last?.content) { _, _ in
                    if let lastId = session.currentConversation?.messages.last?.id {
                        withAnimation(.easeOut(duration: 0.15)) {
                            proxy.scrollTo(lastId, anchor: .bottom)
                        }
                    }
                }
            }

            Divider()

            HStack(alignment: .bottom, spacing: 8) {
                Button(action: toggleMic) {
                    Image(systemName: micActive ? "mic.fill" : "mic")
                        .font(.title2)
                        .foregroundStyle(micActive ? Color.red : Color.accentColor)
                        .symbolEffect(.pulse, isActive: micActive)
                }
                .disabled(
                    session.currentAgent == nil || session.isSendingVoice
                        || session.isConversationActive
                )
                .accessibilityLabel(micAccessibilityLabel)

                // Hands-free conversation mode toggle (additive to
                // push-to-talk): pulses while active, tap again to end.
                Button(action: toggleConversation) {
                    Image(systemName: session.isConversationActive ? "waveform.circle.fill" : "waveform.circle")
                        .font(.title2)
                        .foregroundStyle(session.isConversationActive ? Color.red : Color.accentColor)
                        .symbolEffect(.pulse, isActive: session.isConversationActive)
                }
                .disabled(session.currentAgent == nil || micActive)
                .accessibilityLabel(session.isConversationActive ? "End conversation" : "Start conversation")

                TextField(inputPlaceholder, text: fieldBinding, axis: .vertical)
                    .lineLimit(1...5)
                    .textFieldStyle(.roundedBorder)
                    .focused($inputFocused)
                    .disabled(micActive || session.isSendingVoice || session.isConversationActive)
                    .onSubmit(send)

                if session.isSendingVoice {
                    ProgressView()
                        .controlSize(.small)
                } else {
                    Button(action: send) {
                        Image(systemName: "arrow.up.circle.fill")
                            .font(.title2)
                    }
                    .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || session.currentAgent == nil)
                }
            }
            .padding(12)
        }
        .navigationTitle(session.currentAgent?.displayName ?? "Chat")
        .toolbar {
            if voice.isPlaying {
                ToolbarItem(placement: .automatic) {
                    Label("Speaking", systemImage: "speaker.wave.2.fill")
                        .labelStyle(.iconOnly)
                        .foregroundStyle(.tint)
                }
            }
        }
    }

    /// True while the mic is captured by a PUSH-TO-TALK mode (streaming,
    /// fallback recording, or dictation). Conversation mode's continuous
    /// capture deliberately doesn't count — its state shows on the
    /// waveform button instead.
    private var micActive: Bool {
        guard !session.isConversationActive else { return false }
        return voice.isCapturingPCM || voice.isRecording || voice.isListening
    }

    private var micAccessibilityLabel: String {
        if voice.isCapturingPCM || voice.isRecording { return "Stop recording and send" }
        if voice.isListening { return "Stop dictation" }
        return session.transcribeOnDevice ? "Start dictation" : "Record voice message"
    }

    private var inputPlaceholder: String {
        if session.isConversationActive {
            // Hands-free status line: idle-open mic vs agent audio.
            return voice.isPlaying ? "Speaking…" : "Listening…"
        }
        if session.isSendingVoice { return "Sending voice…" }  // HTTP fallback upload
        if session.isStreamingVoice { return "Streaming…" }  // live over the WS
        if voice.isRecording { return "Recording…" }  // fallback capture (WS down)
        if voice.isListening { return "Listening…" }
        return "Message"
    }

    /// While dictating, mirrors the local partial transcript; while
    /// WS-streaming or in conversation mode with realtime server STT,
    /// mirrors the incoming `transcript_partial` text. Otherwise it's a
    /// normal editable draft. (No partials from the server → the field
    /// stays empty behind the placeholder — graceful degradation.)
    private var fieldBinding: Binding<String> {
        if voice.isListening {
            return .constant(voice.transcript)
        }
        if session.isStreamingVoice || session.isConversationActive,
            let live = session.liveTranscript
        {
            return .constant(live)
        }
        return $draft
    }

    private func send() {
        let text = draft
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        draft = ""
        Task { await session.send(text) }
    }

    private func toggleConversation() {
        if session.isConversationActive {
            Task { await session.stopConversation() }
        } else {
            Task { await session.startConversation() }
        }
    }

    /// Default mode streams raw PCM live over the edge WS for realtime
    /// hotel-side transcription (record→HTTP-upload fallback when
    /// disconnected); the "Transcribe on device" setting switches to local
    /// dictation.
    private func toggleMic() {
        if voice.isCapturingPCM || voice.isRecording {
            // Covers both the WS-streaming and HTTP-fallback captures —
            // the session knows which is active.
            Task { await session.finishVoiceStreaming() }
        } else if voice.isListening {
            let finalText = voice.stopListening()
            let trimmed = finalText.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmed.isEmpty else { return }
            Task { await session.sendVoiceMessage(text: trimmed) }
        } else if session.transcribeOnDevice {
            draft = ""
            Task { await voice.startListening() }
        } else {
            Task { await session.startVoiceStreaming() }
        }
    }
}

private struct MessageBubble: View {
    let message: ChatMessage

    var body: some View {
        HStack {
            if message.role == .operatorUser { Spacer(minLength: 40) }

            VStack(alignment: .leading, spacing: 4) {
                Text(message.content.isEmpty && message.isStreaming ? "…" : message.content)
                    .textSelection(.enabled)
                if message.isStreaming {
                    ProgressView()
                        .controlSize(.mini)
                }
            }
            .padding(10)
            .background(background)
            .foregroundStyle(message.role == .operatorUser ? .white : .primary)
            .clipShape(RoundedRectangle(cornerRadius: 12))

            if message.role != .operatorUser { Spacer(minLength: 40) }
        }
    }

    private var background: some ShapeStyle {
        if message.isError {
            return AnyShapeStyle(.red.opacity(0.2))
        }
        switch message.role {
        case .operatorUser: return AnyShapeStyle(.tint)
        case .agent: return AnyShapeStyle(.secondary.opacity(0.15))
        case .system: return AnyShapeStyle(.yellow.opacity(0.15))
        }
    }
}

#Preview {
    NavigationStack {
        ChatView(session: ChatSessionManager())
    }
}
