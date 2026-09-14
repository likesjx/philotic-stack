import Foundation
import Observation

enum CompanionTab: String, CaseIterable, Identifiable {
    case today, agents, life
    var id: String { rawValue }
}

enum CompanionSheet: String, Identifiable {
    case settings, health, location, reminders, intelligence
    var id: String { rawValue }
}

/// One foreground handoff for windows, the notch, and App Intents.
/// Contains no credentials, observations, or permission-granting operations.
@MainActor
@Observable
final class CompanionRouter {
    static let shared = CompanionRouter()
    var tab: CompanionTab = .today
    var sheet: CompanionSheet?
    private(set) var navigationID = UUID()

    func open(_ tab: CompanionTab) {
        sheet = nil
        self.tab = tab
        navigationID = UUID()
    }
}
