import Foundation
import Observation

/// Owns only the CarPlay surface's capture lease, never routing or chat state.
/// Preparation is ephemeral and scoped to an exact hotel/agent/conversation.
@MainActor
@Observable
public final class CarPlayConversationLifecycle {
    public enum State: Equatable { case disconnected, needsPreparation, ready, starting, active, stopping, failed }
    public private(set) var state: State = .disconnected
    public private(set) var preparedContext: String?
    /// Revokes suspended phone preparation across car connect/end transitions.
    public private(set) var preparationGeneration = UUID()
    private var connected = false
    private var startTask: Task<Void, Never>?
    private var stopTask: Task<Void, Never>?
    private let startCapture: @MainActor () async -> Bool
    private let stopCapture: @MainActor () async -> Void

    public init(start: @escaping @MainActor () async -> Bool, stop: @escaping @MainActor () async -> Void) {
        startCapture = start
        stopCapture = stop
    }

    public func connect() {
        preparationGeneration = UUID()
        connected = true
        if state == .disconnected { state = .needsPreparation }
    }

    /// Called only from the phone after disclosure and microphone permission.
    public func prepare(context: String, generation: UUID) {
        guard generation == preparationGeneration else { return }
        guard state == .needsPreparation || state == .ready || state == .failed else { return }
        preparedContext = context
        state = connected ? .ready : .disconnected
    }

    public func start(context: String) {
        guard connected, state == .ready, preparedContext == context else { return }
        state = .starting
        startTask = Task { [weak self] in
            guard let self else { return }
            let success = await self.startCapture()
            guard !Task.isCancelled else { return }
            if !success {
                self.preparedContext = nil
                await self.stopCapture()
            }
            guard !Task.isCancelled else { return }
            self.state = success ? .active : .failed
        }
    }

    /// No automatic restart after end, route interruption, or connection loss.
    /// Wait for a suspended start to finish before cleanup; no replacement start
    /// can race with cleanup and accidentally acquire the old microphone lease.
    public func end(disconnect: Bool = false) {
        preparationGeneration = UUID()
        if disconnect { connected = false }
        preparedContext = nil
        guard stopTask == nil else { return }
        guard state == .starting || state == .active || state == .failed else {
            state = connected ? .needsPreparation : .disconnected
            return
        }
        state = .stopping
        let pending = startTask
        pending?.cancel()
        stopTask = Task { [weak self] in
            await pending?.value
            guard let self else { return }
            await self.stopCapture()
            self.startTask = nil
            self.stopTask = nil
            self.state = self.connected ? .needsPreparation : .disconnected
        }
    }

    public func validate(context: String?, available: Bool) {
        guard preparedContext != nil else { return }
        if !available || context != preparedContext { end() }
    }
}
