import PhiloticKit
import SwiftUI

struct LifeNodeEditor: View {
    let session: ChatSessionManager
    let node: LifeGraphNode
    let hotelURL: URL
    let onSaved: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var values: [String: String] = [:]
    @State private var saving = false
    @State private var error: String?

    private var edit: LifeNodeEdit { LifeNodeEdit(node: node, values: values) }

    var body: some View {
        NavigationStack {
            Form {
                Section("Node text") {
                    ForEach(LifeNodeEdit.fields, id: \.self) { key in
                        if editable(key) {
                            TextField(label(key), text: Binding(
                                get: { values[key] ?? "" }, set: { values[key] = $0 }
                            ), axis: .vertical)
                            .lineLimit(key == "title" ? 1...3 : 3...8)
                        }
                    }
                }
                Section {
                    Text("Save updates this node directly and records the original and new text, time, and your enrolled device. Confirmation status and source provenance are unchanged.")
                        .font(.caption).foregroundStyle(.secondary)
                    if let error { Text(error).foregroundStyle(.orange).textSelection(.enabled) }
                }
            }
            .formStyle(.grouped)
            .navigationTitle("Edit node")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }.disabled(saving)
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(saving ? "Saving…" : "Save") { Task { await save() } }
                        .disabled(saving || edit.changes.isEmpty ||
                            edit.changes.values.contains { $0.utf8.count > 16_384 } ||
                            edit.changes["claim_summary"]?.trimmingCharacters(in: .whitespacesAndNewlines) == "")
                }
            }
            .disabled(saving)
        }
        .interactiveDismissDisabled(saving || !edit.changes.isEmpty)
        .onAppear {
            values = Dictionary(uniqueKeysWithValues: LifeNodeEdit.fields.map { ($0, node.string($0) ?? "") })
        }
        #if os(macOS)
        .frame(minWidth: 480, minHeight: 420)
        #endif
    }

    private func editable(_ key: String) -> Bool {
        guard let value = node.properties[key] else { return true }
        switch value { case .string, .null: return true; default: return false }
    }
    private func label(_ key: String) -> String {
        key == "claim_summary" ? "Summary" : key.capitalized
    }
    private func save() async {
        guard let id = node.canonicalId, let (url, token) = session.lifeGraphCredentials() else {
            error = "Connect to your hotel before saving."; return
        }
        guard url == hotelURL else {
            error = "The selected hotel changed. Close this editor and reload before editing."; return
        }
        saving = true
        defer { saving = false }
        do {
            let receipt = try await LifeGraphClient().editNode(baseURL: url, bearerToken: token, nodeId: id, edit: edit)
            onSaved(receipt.auditId)
            dismiss()
        } catch { self.error = error.localizedDescription }
    }
}
