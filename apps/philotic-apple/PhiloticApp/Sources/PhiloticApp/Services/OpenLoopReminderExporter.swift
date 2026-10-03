import EventKit
import PhiloticKit

@MainActor
final class OpenLoopReminderExporter {
    static let shared = OpenLoopReminderExporter()
    struct ListChoice: Identifiable { let id: String; let title: String }
    enum Result { case created, existing(completed: Bool, list: String) }
    enum Failure: Error { case denied, invalidList, busy, invalidNode }
    private let store = EKEventStore()
    private var exporting = false

    func lists() async throws -> [ListChoice] {
        try await authorize()
        return store.calendars(for: .reminder).filter(\.allowsContentModifications)
            .map { ListChoice(id: $0.calendarIdentifier, title: $0.title) }
            .sorted { ($0.title, $0.id) < ($1.title, $1.id) }
    }

    func export(node: LifeGraphNode, hotelURL: URL, listID: String) async throws -> Result {
        guard !exporting else { throw Failure.busy }
        exporting = true
        defer { exporting = false }
        guard node.labels.contains("OpenLoop"), let id = node.canonicalId,
              let title = node.string("title") ?? node.string("claim_summary"),
              !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        else { throw Failure.invalidNode }
        let link = try OpenLoopReminderLink(hotelURL: hotelURL, nodeID: id)
        try await authorize()
        // Include completed reminders and all lists: changing the destination
        // must not accidentally duplicate or reopen an already-exported loop.
        let predicate = store.predicateForReminders(in: nil)
        let existing = try await ReminderFetch(store: store).run(predicate)
        try Task.checkCancellation()
        guard EKEventStore.authorizationStatus(for: .reminder) == .fullAccess else { throw Failure.denied }
        if let match = existing.first(where: { link.matches(notes: $0.notes) }) {
            return .existing(completed: match.isCompleted, list: match.calendar.title)
        }
        guard let calendar = store.calendar(withIdentifier: listID), calendar.allowsContentModifications,
              calendar.allowedEntityTypes.contains(.reminder) else { throw Failure.invalidList }
        let reminder = EKReminder(eventStore: store)
        reminder.calendar = calendar
        reminder.title = title
        reminder.notes = link.marker
        // No notes, provenance, health data, due dates or credentials exported.
        // Never change LifeGraph status or complete an existing reminder here.
        try store.save(reminder, commit: true)
        return .created
    }

    private func authorize() async throws {
        if EKEventStore.authorizationStatus(for: .reminder) == .notDetermined {
            guard try await store.requestFullAccessToReminders() else { throw Failure.denied }
        }
        try Task.checkCancellation()
        guard EKEventStore.authorizationStatus(for: .reminder) == .fullAccess else { throw Failure.denied }
    }
}
