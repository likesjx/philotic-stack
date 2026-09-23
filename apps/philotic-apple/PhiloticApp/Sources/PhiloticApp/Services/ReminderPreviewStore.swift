import EventKit
import Foundation
import Observation

struct ReminderPreview: Identifiable, Equatable, Sendable {
    let id: String
    let title: String
    let list: String
    let due: Date?
}

enum ReminderPreviewError: Error { case denied, unavailable }

/// EventKit exposes full Reminders access, not a read-only grant. This adapter
/// deliberately exposes only reads; no save, completion, deletion, or upload API.
@MainActor
final class DeviceReminderReader {
    private let store = EKEventStore()

    func read() async throws -> [ReminderPreview] {
        var status = EKEventStore.authorizationStatus(for: .reminder)
        if status == .notDetermined {
            guard try await store.requestFullAccessToReminders() else { throw ReminderPreviewError.denied }
            status = EKEventStore.authorizationStatus(for: .reminder)
        }
        guard status == .fullAccess else { throw ReminderPreviewError.denied }
        let deadline = Calendar.current.date(byAdding: .day, value: 7, to: Date())!
        let predicate = store.predicateForIncompleteReminders(
            withDueDateStarting: nil, ending: deadline, calendars: nil)
        let reminders = try await ReminderFetch(store: store).run(predicate)
        try Task.checkCancellation()
        let currentStatus = EKEventStore.authorizationStatus(for: .reminder)
        guard currentStatus == .fullAccess else { throw ReminderPreviewError.denied }
        // Keep the projection small; do not copy notes, URLs, alarms or attendees.
        let previews = reminders.map {
            ReminderPreview(id: $0.calendarItemIdentifier, title: $0.title ?? "Untitled reminder",
                list: $0.calendar.title, due: $0.dueDateComponents.flatMap { components in
                    (components.calendar ?? Calendar.current).date(from: components)
                })
        }
        return Array(previews.sorted { ($0.due ?? .distantFuture) < ($1.due ?? .distantFuture) }.prefix(50))
    }
}

/// Bound the callback-based query and ignore late completions after timeout.
@MainActor
private final class ReminderFetch {
    let store: EKEventStore
    private var continuation: CheckedContinuation<[EKReminder], Error>?
    private var token: Any?
    private var timeout: Task<Void, Never>?

    init(store: EKEventStore) { self.store = store }

    func run(_ predicate: NSPredicate) async throws -> [EKReminder] {
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                self.continuation = continuation
                token = store.fetchReminders(matching: predicate) { [weak self] values in
                    Task { @MainActor in
                        if let values { self?.finish(.success(values)) }
                        else { self?.finish(.failure(ReminderPreviewError.unavailable)) }
                    }
                }
                timeout = Task { [weak self] in
                    do { try await Task.sleep(for: .seconds(20)) } catch { return }
                    self?.finish(.failure(ReminderPreviewError.unavailable), cancelFetch: true)
                }
                if Task.isCancelled { finish(.failure(CancellationError()), cancelFetch: true) }
            }
        } onCancel: {
            Task { @MainActor [weak self] in
                self?.finish(.failure(CancellationError()), cancelFetch: true)
            }
        }
    }

    private func finish(_ result: Result<[EKReminder], Error>, cancelFetch: Bool = false) {
        guard let continuation else { return }
        self.continuation = nil
        timeout?.cancel()
        timeout = nil
        if cancelFetch, let token { store.cancelFetchRequest(token) }
        token = nil
        continuation.resume(with: result)
    }
}

@MainActor
@Observable
final class ReminderPreviewStore {
    enum State: Equatable { case idle, loading, loaded, denied, failed }
    private(set) var state: State = .idle
    private(set) var items: [ReminderPreview] = []
    private var generation = 0
    private let read: () async throws -> [ReminderPreview]

    init(read: @escaping () async throws -> [ReminderPreview]) { self.read = read }
    convenience init() {
        let reader = DeviceReminderReader()
        self.init { try await reader.read() }
    }

    func load() async {
        guard state != .loading else { return }
        generation += 1
        let request = generation
        items = []
        state = .loading
        do {
            let result = try await read()
            guard request == generation else { return }
            guard !Task.isCancelled else { state = .idle; return }
            items = Array(result.prefix(50))
            state = .loaded
        } catch {
            guard request == generation else { return }
            if error is CancellationError { state = .idle }
            else if case ReminderPreviewError.denied = error { state = .denied }
            else { state = .failed }
        }
    }

    func discard() {
        generation += 1
        items = []
        state = .idle
    }
}
