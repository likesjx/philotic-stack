import AppIntents

struct OpenTodayIntent: AppIntent {
    static let title: LocalizedStringResource = "Open Philotic Today"
    static let description = IntentDescription("Open your dashboard without requesting Apple permissions or sharing data.")
    static let openAppWhenRun = true
    @MainActor func perform() async throws -> some IntentResult {
        CompanionRouter.shared.open(.today)
        return .result()
    }
}

struct TalkToAgentsIntent: AppIntent {
    static let title: LocalizedStringResource = "Talk to Philotic Agents"
    static let description = IntentDescription("Open your agents. Choose the agent and send the message in the app.")
    static let openAppWhenRun = true
    @MainActor func perform() async throws -> some IntentResult {
        CompanionRouter.shared.open(.agents)
        return .result()
    }
}

struct OpenLifeGraphIntent: AppIntent {
    static let title: LocalizedStringResource = "Explore Philotic LifeGraph"
    static let openAppWhenRun = true
    @MainActor func perform() async throws -> some IntentResult {
        CompanionRouter.shared.open(.life)
        return .result()
    }
}

struct CompanionShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(intent: OpenTodayIntent(), phrases: ["Open today in \(.applicationName)"],
                    shortTitle: "Today", systemImageName: "sparkles.rectangle.stack")
        AppShortcut(intent: TalkToAgentsIntent(), phrases: ["Talk to my agents in \(.applicationName)"],
                    shortTitle: "Talk to agents", systemImageName: "bubble.left.and.bubble.right")
        AppShortcut(intent: OpenLifeGraphIntent(), phrases: ["Explore my life graph in \(.applicationName)"],
                    shortTitle: "Explore LifeGraph", systemImageName: "brain")
    }
}
