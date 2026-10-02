import PhiloticKit
import SwiftUI

struct OpenLoopReminderSheet: View {
    let node: LifeGraphNode
    let hotelURL: URL
    @Environment(\.dismiss) private var dismiss
    @State private var lists: [OpenLoopReminderExporter.ListChoice] = []
    @State private var listID = ""
    @State private var busy = false
    @State private var message: String?
    @State private var exported = false

    var body: some View {
        NavigationStack {
            Form {
                Section("Selected open loop") {
                    Text(node.string("title") ?? node.string("claim_summary") ?? "Open loop").font(.headline)
                    Text("Only this title and a private linking marker will be copied. No other graph properties are shared.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Section("Destination") {
                    Picker("Reminders list", selection: $listID) {
                        Text("Choose a list").tag("")
                        ForEach(lists) { list in Text(list.title).tag(list.id) }
                    }.disabled(busy || exported)
                    Button("Send to Reminders", systemImage: "checklist") { Task { await export() } }
                        .disabled(listID.isEmpty || busy || exported)
                }
                if busy { ProgressView("Checking Reminders…") }
                if let message { Text(message).font(.subheadline) }
                Section {
                    Text("This is a linked export, not automatic two-way sync. Reminder completion does not close or confirm the LifeGraph loop. Repeating an export checks for an existing linked item first.")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }.navigationTitle("Send open loop")
                .toolbar { Button("Done") { dismiss() }.disabled(busy) }
        }
        #if os(macOS)
        .frame(minWidth: 460, minHeight: 400)
        #endif
        .task { await loadLists() }
    }

    private func loadLists() async {
        busy = true
        defer { busy = false }
        do {
            lists = try await OpenLoopReminderExporter.shared.lists()
            if lists.isEmpty { message = "No writable Reminders lists are available. Create a list in Reminders, then reopen this screen." }
        } catch is CancellationError { } catch {
            message = "Reminders access is unavailable. Allow Philotic in Privacy & Security → Reminders, then reopen this screen."
        }
    }

    private func export() async {
        busy = true; message = nil
        defer { busy = false }
        do {
            let result = try await OpenLoopReminderExporter.shared.export(node: node, hotelURL: hotelURL, listID: listID)
            exported = true
            switch result {
            case .created: message = "Saved to Reminders. The LifeGraph loop is unchanged."
            case let .existing(completed, list):
                message = completed ? "Already completed in \(list). No duplicate created; review its LifeGraph status separately." : "Already linked in \(list). No duplicate created."
            }
        } catch is CancellationError { } catch {
            message = "Could not export. Check Reminders access and your selected list, then try again."
        }
    }
}
