#if os(macOS)
import AppKit
import Observation
import OSLog
import SwiftUI

enum CompanionPanelLayout {
    /// The shell grows from the camera, with its top edge pinned to the display.
    /// Interactive content is inset below the camera/menu band separately.
    static func frame(screen: CGRect, visible: CGRect, safeTop: CGFloat, expanded: Bool,
                      camera: CGRect? = nil) -> CGRect {
        if !expanded, let camera, !camera.isEmpty { return camera.intersection(screen) }
        let width = min(expanded ? 500.0 : 180.0, max(1, visible.width - 16))
        let height = min(expanded ? 540.0 + safeTop : max(1, safeTop),
                         max(1, screen.maxY - visible.minY - 16))
        let center = camera?.midX ?? screen.midX
        return CGRect(x: max(visible.minX, min(center - width / 2, visible.maxX - width)),
                      y: screen.maxY - height, width: width, height: height)
    }

    static func contentInset(screen: CGRect, visible: CGRect, safeTop: CGFloat) -> CGFloat {
        max(safeTop, screen.maxY - visible.maxY) + 8
    }
}

private final class CompanionPanel: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }
}

/// The sole AppKit owner. SwiftUI/session objects still own application state.
@MainActor
@Observable
final class NotchController {
    private static let logger = Logger(subsystem: "com.philotic.apple.mac", category: "NotchHover")
    var expanded = false {
        didSet {
            guard expanded != oldValue else { return }
            Self.logger.notice("presentation expanded=\(self.expanded)")
            if !expanded {
                hover.suppressUntilExit()
                // Retain the editor's draft, not keyboard ownership of an
                // invisible field after Escape/manual collapse.
                panel?.makeFirstResponder(nil)
                panel?.resignKey()
            }
            position(animated: true)
        }
    }
    /// Enabled is independent of window visibility: the resting panel is ordered out.
    private(set) var isEnabled = false
    private(set) var contentTopInset: CGFloat = 40
    private(set) var contentSize = CGSize(width: 500, height: 540)
    @ObservationIgnored private var panel: NSPanel?
    @ObservationIgnored private var displayObserver: NSObjectProtocol?
    @ObservationIgnored private var lockObserver: NSObjectProtocol?
    @ObservationIgnored private var resumeObserver: NSObjectProtocol?
    @ObservationIgnored private var sleepObserver: NSObjectProtocol?
    @ObservationIgnored private var wakeObserver: NSObjectProtocol?
    @ObservationIgnored private var menuObservers: [NSObjectProtocol] = []
    @ObservationIgnored private var hoverTimer: Timer?
    @ObservationIgnored private var hover = NotchHoverState()
    @ObservationIgnored private var menuDepth = 0
    @ObservationIgnored private var sessionActive = true
    @ObservationIgnored private var displayAwake = true
    @ObservationIgnored private var isBusy: () -> Bool = { false }
    @ObservationIgnored private var presentationGeneration = 0

    func configure(session: ChatSessionManager, router: CompanionRouter, openMain: @escaping () -> Void) {
        guard panel == nil else { return }
        let panel = CompanionPanel(contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel],
                                   backing: .buffered, defer: false)
        panel.title = "Philotic Companion"
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.level = .statusBar
        panel.hidesOnDeactivate = false
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        panel.isReleasedWhenClosed = false
        let hosting = NSHostingView(rootView: NotchView(
            session: session, router: router, controller: self, openMain: openMain))
        // The controller owns the animated frame, not the content's minimum size.
        hosting.sizingOptions = []
        panel.contentView = hosting
        self.panel = panel
        isBusy = { [weak session] in
            guard let session else { return false }
            return session.isConversationActive || session.isStreamingVoice || session.isSendingVoice
                || session.voiceController.isRecording || session.voiceController.isListening
        }
        displayObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.didChangeScreenParametersNotification, object: nil, queue: .main
        ) { [weak self] _ in Task { @MainActor in self?.position() } }
        lockObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.sessionDidResignActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in Task { @MainActor in
            self?.sessionActive = false
            self?.suspendHover()
        } }
        resumeObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.sessionDidBecomeActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in Task { @MainActor in
            self?.sessionActive = true
            self?.resumeHover()
        } }
        sleepObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.screensDidSleepNotification, object: nil, queue: .main
        ) { [weak self] _ in Task { @MainActor in
            self?.displayAwake = false
            self?.suspendHover()
        } }
        wakeObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.screensDidWakeNotification, object: nil, queue: .main
        ) { [weak self] _ in Task { @MainActor in
            self?.displayAwake = true
            self?.resumeHover()
        } }
        for name in [NSMenu.didBeginTrackingNotification, NSMenu.didEndTrackingNotification] {
            let entering = name == NSMenu.didBeginTrackingNotification
            menuObservers.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) {
                [weak self] _ in Task { @MainActor in
                    guard let self else { return }
                    self.menuDepth = max(0, self.menuDepth + (entering ? 1 : -1))
                }
            })
        }
    }

    func show() {
        isEnabled = panel != nil
        position()
        Self.logger.notice("companion enabled=\(self.isEnabled)")
        resumeHover()
    }

    func hide() {
        Self.logger.notice("hide requested")
        isEnabled = false
        stopHover()
        expanded = false
        presentationGeneration += 1
        panel?.orderOut(nil)
    }

    private var screen: NSScreen? {
        // Prefer the physical notch, even when another app is on an external display.
        NSScreen.screens.first(where: { $0.safeAreaInsets.top > 0 }) ?? panel?.screen ?? NSScreen.main
    }

    private func position(animated: Bool = false) {
        guard let panel, let screen else { return }
        let camera = cameraFrame(on: screen)
        let resting = CompanionPanelLayout.frame(screen: screen.frame, visible: screen.visibleFrame,
                                                 safeTop: screen.safeAreaInsets.top, expanded: false, camera: camera)
        let open = CompanionPanelLayout.frame(screen: screen.frame, visible: screen.visibleFrame,
                                              safeTop: screen.safeAreaInsets.top, expanded: true, camera: camera)
        contentTopInset = CompanionPanelLayout.contentInset(screen: screen.frame, visible: screen.visibleFrame,
                                                            safeTop: screen.safeAreaInsets.top)
        contentSize = open.size
        presentationGeneration += 1
        let generation = presentationGeneration
        let presenting = isEnabled && expanded && sessionActive && displayAwake
        let frame = presenting ? open : resting
        panel.ignoresMouseEvents = !presenting
        panel.hasShadow = presenting
        if presenting && !panel.isVisible {
            panel.setFrame(resting, display: false)
            panel.orderFrontRegardless()
        }
        if animated && isEnabled && panel.isVisible && sessionActive && displayAwake
            && !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion {
            NSAnimationContext.runAnimationGroup { context in
                context.duration = presenting ? 0.28 : 0.22
                context.timingFunction = CAMediaTimingFunction(name: .easeInEaseOut)
                panel.animator().setFrame(frame, display: true)
            } completionHandler: { [weak self] in
                MainActor.assumeIsolated {
                    guard let self, self.presentationGeneration == generation else { return }
                    if !presenting { self.panel?.orderOut(nil) }
                }
            }
        } else {
            panel.setFrame(frame, display: true)
            if !presenting { panel.orderOut(nil) }
        }
        // Hover only changes presentation. Clicking an editor may make the panel key.
    }

    private func resumeHover() {
        guard isEnabled, sessionActive, displayAwake, hoverTimer == nil else { return }
        // Sampling only the current pointer also covers the camera/menu-bar area
        // while other apps are active. No event interception, key logging, or TCC grant.
        let timer = Timer(timeInterval: 0.05, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.samplePointer() }
        }
        timer.tolerance = 0.01
        RunLoop.main.add(timer, forMode: .common)
        hoverTimer = timer
        Self.logger.notice("pointer sampler started")
    }

    private func stopHover() {
        hoverTimer?.invalidate()
        hoverTimer = nil
        Self.logger.notice("pointer sampler stopped")
        hover = NotchHoverState()
    }

    private func suspendHover() {
        stopHover()
        expanded = false
        position()
    }

    private func samplePointer() {
        guard isEnabled, sessionActive, displayAwake, let panel, let screen else { return }
        let camera = cameraFrame(on: screen)
        let collapsed = CompanionPanelLayout.frame(screen: screen.frame, visible: screen.visibleFrame,
                                                   safeTop: screen.safeAreaInsets.top, expanded: false, camera: camera)
        let expandedFrame = CompanionPanelLayout.frame(screen: screen.frame, visible: screen.visibleFrame,
                                                       safeTop: screen.safeAreaInsets.top, expanded: true, camera: camera)
        let activation = NotchHoverRegion.activation(screen: screen.frame, collapsed: collapsed, camera: camera)
        let retention = NotchHoverRegion.retention(screen: screen.frame, activation: activation, panel: expandedFrame)
        let point = NSEvent.mouseLocation
        let editing = panel.isKeyWindow && panel.firstResponder is NSTextView
        let interacting = editing || menuDepth > 0 || NSEvent.pressedMouseButtons != 0 || isBusy()
        switch hover.update(inActivation: NotchHoverRegion.contains(point, in: activation),
                            inRetention: NotchHoverRegion.contains(point, in: retention),
                            expanded: expanded, interacting: interacting, now: ProcessInfo.processInfo.systemUptime) {
        case .expand: expanded = true
        case .collapse: expanded = false
        case nil: break
        }
    }

    private func cameraFrame(on screen: NSScreen) -> CGRect? {
        guard let left = screen.auxiliaryTopLeftArea, let right = screen.auxiliaryTopRightArea,
              right.minX > left.maxX else { return nil }
        return CGRect(x: left.maxX, y: screen.frame.maxY - screen.safeAreaInsets.top,
                      width: right.minX - left.maxX, height: screen.safeAreaInsets.top)
    }

    deinit {
        hoverTimer?.invalidate()
        if let displayObserver { NotificationCenter.default.removeObserver(displayObserver) }
        for observer in [lockObserver, resumeObserver, sleepObserver, wakeObserver].compactMap({ $0 }) {
            NSWorkspace.shared.notificationCenter.removeObserver(observer)
        }
        for observer in menuObservers { NotificationCenter.default.removeObserver(observer) }
    }
}
#endif
