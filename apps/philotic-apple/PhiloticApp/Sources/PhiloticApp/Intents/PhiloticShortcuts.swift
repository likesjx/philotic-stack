// PhiloticShortcuts.swift
// App Shortcuts — the Siri-invocable phrase surface
// (seam: apple-entity-index-plane / apple-custom-intents).
//
// Honest scope note, per APPLE_INTELLIGENCE_PLANE_PROPOSAL.md: philotic
// concepts have no App Schema domain, so these are *custom* intents. They get
// Shortcuts, Spotlight, widgets, the Action Button, and Siri invocation by the
// phrases declared here — but NOT free-form Siri language reasoning. The
// phrases are therefore the whole interface and are written deliberately.
//
// Deep Siri reasoning over our content arrives via IndexedEntity donation
// (LifeNodeEntity), not through this file.

import AppIntents

@available(iOS 18.0, macOS 15.0, *)
struct PhiloticShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(
            intent: OpenLifeNodeIntent(),
            phrases: [
                "Open \(\.$target) in \(.applicationName)",
                "Show \(\.$target) in \(.applicationName)",
                "Open my \(.applicationName) life graph entry \(\.$target)",
            ],
            shortTitle: "Open Life Graph Entry",
            systemImageName: "brain"
        )
    }
}
