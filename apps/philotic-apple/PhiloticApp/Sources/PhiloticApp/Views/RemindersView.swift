import SwiftUI

struct RemindersView: View {
    @State private var store = ReminderPreviewStore()
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        List {
            Section {
                Label("A local look at what's next.", systemImage: "checklist")
                    .font(.title3.bold())
                Text("Preview up to 50 incomplete reminders due within seven days, including overdue items. Undated reminders are not included.")
                Text("Apple asks for full Reminders access. This version only reads: it cannot change reminders, sync them to LifeGraph, or send them to agents.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Section {
                switch store.state {
                case .idle:
                    Button("Review Reminders on this device") { Task { await store.load() } }
                case .loading:
                    ProgressView("Reading reminders…")
                case .denied:
                    Label("Reminders access is off", systemImage: "hand.raised")
                    Text("Allow PhiloticApp in your system privacy settings, then retry.")
                    Button("Retry") { Task { await store.load() } }
                case .failed:
                    Label("Reminders unavailable", systemImage: "exclamationmark.triangle")
                    Text("No empty-list conclusion was made.")
                    Button("Retry") { Task { await store.load() } }
                case .loaded:
                    if store.items.isEmpty { Text("No incomplete reminders due in this window.") }
                    ForEach(store.items) { item in
                        VStack(alignment: .leading, spacing: 4) {
                            Text(item.title)
                            Text(item.list).font(.caption).foregroundStyle(.secondary)
                            if let due = item.due {
                                Text(due, format: .dateTime.month().day().hour().minute())
                                    .font(.caption).foregroundStyle(.secondary)
                            }
                        }
                    }
                    Button("Clear local preview") { store.discard() }
                }
            }
        }
        .navigationTitle("Reminders")
        .onDisappear { store.discard() }
        .onChange(of: scenePhase) { _, phase in
            if phase != .active { store.discard() }
        }
        #if os(iOS)
        .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } } }
        #endif
    }
}
