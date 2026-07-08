// ChatView.swift
// Streaming chat surface for the currently selected agent: message list
// (live-rendering tokens as they arrive) plus a send box.

import SwiftUI

struct ChatView: View {
    @Bindable var session: ChatSessionManager
    @State private var draft: String = ""
    @FocusState private var inputFocused: Bool

    var body: some View {
        VStack(spacing: 0) {
            ConnectionStatusBar(endpointName: session.selectedEndpointName, state: session.connectionState)

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
                TextField("Message", text: $draft, axis: .vertical)
                    .lineLimit(1...5)
                    .textFieldStyle(.roundedBorder)
                    .focused($inputFocused)
                    .onSubmit(send)

                Button(action: send) {
                    Image(systemName: "arrow.up.circle.fill")
                        .font(.title2)
                }
                .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || session.currentAgent == nil)
            }
            .padding(12)
        }
        .navigationTitle(session.currentAgent?.displayName ?? "Chat")
    }

    private func send() {
        let text = draft
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        draft = ""
        Task { await session.send(text) }
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
