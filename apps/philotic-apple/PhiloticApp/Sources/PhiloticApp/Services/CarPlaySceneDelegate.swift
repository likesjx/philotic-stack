// Dormant until the separately reviewed CarPlay capability/configuration step.
#if os(iOS) && PHILOTIC_CARPLAY
import AVFoundation
import CarPlay
import PhiloticKit
import UIKit

@MainActor
final class CarPlayBridge {
    static let shared = CarPlayBridge()
    weak var session: ChatSessionManager?
    private var ownedLease: UUID?
    var context: String? {
        guard let conversation = session?.currentConversation,
              conversation.agentTarget == session?.currentAgent else { return nil }
        guard let session else { return nil }
        let scope = [session.carPlayTransportEpoch.uuidString,
                     conversation.agentTarget.targetNodeId,
                     conversation.agentTarget.targetAgentId, conversation.conversationId]
        guard let data = try? JSONSerialization.data(withJSONObject: scope) else { return nil }
        return String(data: data, encoding: .utf8)
    }
    var available: Bool {
        guard let session, case .connected = session.connectionState else { return false }
        return session.canStartCarPlayConversation && context != nil
            && AVAudioApplication.shared.recordPermission == .granted
    }
    lazy var lifecycle: CarPlayConversationLifecycle = CarPlayConversationLifecycle(start: { [weak self] in
        guard let self, let session = self.session, self.available,
              self.lifecycle.preparedContext == self.context,
              !session.isConversationActive, !session.isStreamingVoice,
              !session.voiceController.isRecording, !session.voiceController.isListening,
              !session.voiceController.isCapturingPCM else { return false }
        self.ownedLease = await session.startConversation(carPlay: true)
        return self.ownedLease != nil
    }, stop: { [weak self] in
        guard let self, let lease = self.ownedLease else { return }
        self.ownedLease = nil
        await self.session?.endCarPlayConversation(lease: lease)
    })

    /// Phone permission is not routing authority. Current develop denies here
    /// before the permission request because trusted voice policy is absent.
    func prepare() async -> Bool {
        guard let session, session.canStartCarPlayConversation,
              case .connected = session.connectionState,
              let context, !session.isConversationActive,
              lifecycle.state == .needsPreparation || lifecycle.state == .failed else { return false }
        let generation = lifecycle.preparationGeneration
        let granted = await AVAudioApplication.requestRecordPermission()
        guard granted, !Task.isCancelled, generation == lifecycle.preparationGeneration,
              self.context == context, available else { return false }
        lifecycle.prepare(context: context, generation: generation)
        return lifecycle.state == .ready
    }
}

@MainActor
final class CarPlaySceneDelegate: UIResponder, CPTemplateApplicationSceneDelegate {
    private let bridge = CarPlayBridge.shared
    private var refreshTask: Task<Void, Never>?
    private var observers: [NSObjectProtocol] = []
    private var connectionID = UUID()

    func templateApplicationScene(_ templateApplicationScene: CPTemplateApplicationScene,
                                  didConnect interfaceController: CPInterfaceController) {
        teardown()
        guard #available(iOS 26.4, *) else { return }
        let connectionID = self.connectionID
        let template = Self.makeTemplate(start: { [weak self] in
            guard let self, let context = self.bridge.context else { return }
            self.bridge.lifecycle.start(context: context)
        }, end: { [weak self] in self?.bridge.lifecycle.end() })
        // Capture is impossible until the voice template is installed successfully.
        interfaceController.setRootTemplate(template, animated: false) { [weak self] success, _ in
            Task { @MainActor [weak self] in
                guard let self, self.connectionID == connectionID else { return }
                if success {
                    self.bridge.lifecycle.connect()
                    self.observe(template: template)
                }
                else { self.teardown() }
            }
        }
    }

    @available(iOS 26.4, *)
    static func makeTemplate(start: @escaping () -> Void, end: @escaping () -> Void) -> CPVoiceControlTemplate {
        let descriptions = [
            ("unavailable", "Voice unavailable", "mic.slash"),
            ("needsPreparation", "Prepare on iPhone while parked", "iphone"),
            ("ready", "Ready to talk", "mic"),
            ("starting", "Starting", "waveform"),
            ("active", "Listening", "mic.fill"),
            ("speaking", "Speaking", "speaker.wave.2.fill"),
            ("stopping", "Ending", "stop.fill"),
            ("failed", "Check iPhone while parked", "exclamationmark.triangle")
        ]
        let states = descriptions.map { id, title, symbol in
            let state = CPVoiceControlState(identifier: id, titleVariants: [title],
                                            image: UIImage(systemName: symbol), repeats: false)
            let talkButton = CPButton(image: UIImage(systemName: "mic.fill")!) { _ in start() }
            talkButton.title = "Talk"
            talkButton.isEnabled = id == "ready"
            let endButton = CPButton(image: UIImage(systemName: "stop.fill")!) { _ in end() }
            endButton.title = "End"
            endButton.isEnabled = ["starting", "active", "speaking"].contains(id)
            state.actionButtons = [talkButton, endButton]
            return state
        }
        let template = CPVoiceControlTemplate(voiceControlStates: states)
        template.activateVoiceControlState(withIdentifier: "unavailable")
        return template
    }

    @available(iOS 26.4, *)
    private func observe(template: CPVoiceControlTemplate) {
        for name in [AVAudioSession.interruptionNotification, AVAudioSession.routeChangeNotification,
                     AVAudioSession.mediaServicesWereResetNotification] {
            observers.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) {
                [weak self] notification in
                // A category change caused by our own duplex setup is expected.
                if name == AVAudioSession.routeChangeNotification,
                   let raw = notification.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt,
                   raw == AVAudioSession.RouteChangeReason.categoryChange.rawValue { return }
                Task { @MainActor [weak self] in self?.bridge.lifecycle.end() }
            })
        }
        refreshTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                self.bridge.lifecycle.validate(context: self.bridge.context, available: self.bridge.available)
                if self.bridge.lifecycle.state == .active,
                   self.bridge.session?.isConversationActive != true {
                    self.bridge.lifecycle.end()
                }
                let identifier: String
                if self.bridge.session?.canStartCarPlayConversation != true {
                    identifier = "unavailable"
                } else { switch self.bridge.lifecycle.state {
                case .disconnected, .needsPreparation: identifier = "needsPreparation"
                case .ready: identifier = "ready"
                case .starting: identifier = "starting"
                case .active: identifier = self.bridge.session?.voiceController.isPlaying == true ? "speaking" : "active"
                case .stopping: identifier = "stopping"
                case .failed: identifier = "failed"
                } }
                template.activateVoiceControlState(withIdentifier: identifier)
                try? await Task.sleep(for: .milliseconds(200))
            }
        }
    }

    func templateApplicationScene(_ templateApplicationScene: CPTemplateApplicationScene,
                                  didDisconnectInterfaceController interfaceController: CPInterfaceController) { teardown() }

    private func teardown() {
        connectionID = UUID()
        refreshTask?.cancel()
        refreshTask = nil
        observers.forEach { NotificationCenter.default.removeObserver($0) }
        observers.removeAll()
        bridge.lifecycle.end(disconnect: true)
    }
}
#endif
