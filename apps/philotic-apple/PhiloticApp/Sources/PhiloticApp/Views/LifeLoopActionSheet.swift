import PhiloticKit
import SwiftUI

struct LifeLoopActionSheet: View {
    let session: ChatSessionManager
    let node: LifeGraphNode
    let hotelURL: URL
    let action: LifeLoopAction.Action
    let onSaved: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var note = ""
    @State private var saving = false
    @State private var error: String?
    @State private var attempted: LifeLoopAction?
    @State private var mustReload = false

    var body: some View {
        NavigationStack {
            Form {
                Section(node.string("title") ?? node.string("claim_summary") ?? "Open loop") {
                    Text(action.explanation)
                    TextField("Optional note", text: $note, axis: .vertical)
                        .lineLimit(3...6).disabled(attempted != nil)
                }
                Section {
                    Text("Saved directly to LifeGraph with a before/after audit and your enrolled device. If the loop changed since loading, the action will be rejected.")
                        .font(.caption).foregroundStyle(.secondary)
                    if attempted != nil, error != nil, !mustReload {
                        Text("Retry keeps the same action and note to avoid duplicate saves.").font(.caption)
                    }
                    if let error { Text(error).foregroundStyle(.orange).textSelection(.enabled) }
                }
            }
            .formStyle(.grouped)
            .navigationTitle(action.title)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }.disabled(saving)
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(saving ? "Saving…" : attempted == nil ? action.title : "Retry same action") {
                        Task { await save() }
                    }.disabled(saving || mustReload || note.utf8.count > 4096)
                }
            }
        }
        .interactiveDismissDisabled(saving)
        #if os(macOS)
        .frame(minWidth: 480, minHeight: 340)
        #endif
    }

    private func save() async {
        guard !saving, let id = node.canonicalId,
              let (url, token) = session.lifeGraphCredentials(), url == hotelURL else {
            error = "The connection changed. Close this sheet and reload the loop."; mustReload = true; return
        }
        guard let command = attempted ?? LifeLoopAction(node: node, action: action, note: note) else {
            error = "This action is not available for the loaded loop. Reload it first."; mustReload = true; return
        }
        attempted = command
        saving = true
        defer { saving = false }
        do {
            let receipt = try await LifeGraphClient().actOnLoop(baseURL: url, bearerToken: token, nodeId: id, action: command)
            guard session.lifeGraphCredentials()?.0 == hotelURL else {
                error = "Saved to the original hotel, but your connection changed. Reconnect there and reload."; mustReload = true; return
            }
            onSaved(receipt.auditId)
            dismiss()
        } catch {
            self.error = error.localizedDescription
            if let failure = error as? LifeGraphClient.LoopActionError, case .conflict = failure { mustReload = true }
        }
    }
}
