#if os(macOS)
import AppKit
import Observation
import SwiftUI

enum CompanionPanelLayout {
    /// Sit below the camera/menu exclusion area, including on non-notched displays.
    static func frame(screen: CGRect, visible: CGRect, safeTop: CGFloat, expanded: Bool) -> CGRect {
        let width = min(expanded ? 500.0 : 230.0, max(1, visible.width - 16))
        let height = min(expanded ? 540.0 : 38.0, max(1, visible.height - 16))
        let top = min(visible.maxY, screen.maxY - safeTop) - 6
        return CGRect(x: max(visible.minX, min(screen.midX - width / 2, visible.maxX - width)),
                      y: max(visible.minY, top - height), width: width, height: height)
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
    var expanded = false {
        didSet {
            guard expanded != oldValue else { return }
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
    private(set) var isVisible = false
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

    func configure(session: ChatSessionManager, router: CompanionRouter, openMain: @escaping () -> Void) {
        guard panel == nil else { return }
        let panel = CompanionPanel(contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel],
                                   backing: .buffered, defer: false)
        panel.title = "Philotic Companion"
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.level = .floating
        panel.hidesOnDeactivate = false
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        panel.isReleasedWhenClosed = false
        panel.contentView = NSHostingView(rootView: NotchView(
            session: session, router: router, controller: self, openMain: openMain))
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
        position()
        panel?.orderFrontRegardless()
        isVisible = panel != nil
        resumeHover()
    }

    func hide() {
        stopHover()
        expanded = false
        panel?.orderOut(nil)
        isVisible = false
    }

    private var screen: NSScreen? {
        // Prefer the physical notch, even when another app is on an external display.
        NSScreen.screens.first(where: { $0.safeAreaInsets.top > 0 }) ?? panel?.screen ?? NSScreen.main
    }

    private func position(animated: Bool = false) {
        guard let panel, let screen else { return }
        let frame = CompanionPanelLayout.frame(screen: screen.frame, visible: screen.visibleFrame,
                                                safeTop: screen.safeAreaInsets.top, expanded: expanded)
        if animated && isVisible && !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion {
            NSAnimationContext.runAnimationGroup { context in
                context.duration = 0.18
                panel.animator().setFrame(frame, display: true)
            }
        } else { panel.setFrame(frame, display: true) }
        // Hover only changes presentation. Clicking an editor may make the panel key.
    }

    private func resumeHover() {
        guard isVisible, sessionActive, displayAwake, hoverTimer == nil else { return }
        // Sampling only the current pointer also covers the camera/menu-bar area
        // while other apps are active. No event interception, key logging, or TCC grant.
        let timer = Timer(timeInterval: 0.05, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.samplePointer() }
        }
        timer.tolerance = 0.01
        RunLoop.main.add(timer, forMode: .common)
        hoverTimer = timer
    }

    private func stopHover() {
        hoverTimer?.invalidate()
        hoverTimer = nil
        hover = NotchHoverState()
    }

    private func suspendHover() {
        stopHover()
        expanded = false
    }

    private func samplePointer() {
        guard isVisible, sessionActive, displayAwake, let panel, let screen else { return }
        let collapsed = CompanionPanelLayout.frame(screen: screen.frame, visible: screen.visibleFrame,
                                                   safeTop: screen.safeAreaInsets.top, expanded: false)
        let expandedFrame = CompanionPanelLayout.frame(screen: screen.frame, visible: screen.visibleFrame,
                                                       safeTop: screen.safeAreaInsets.top, expanded: true)
        var camera: CGRect?
        if let left = screen.auxiliaryTopLeftArea, let right = screen.auxiliaryTopRightArea,
           right.minX > left.maxX {
            camera = CGRect(x: left.maxX, y: screen.frame.maxY - screen.safeAreaInsets.top,
                            width: right.minX - left.maxX, height: screen.safeAreaInsets.top)
        }
        let activation = NotchHoverRegion.activation(screen: screen.frame, collapsed: collapsed, camera: camera)
        let retention = NotchHoverRegion.retention(screen: screen.frame, activation: activation, panel: expandedFrame)
        let point = NSEvent.mouseLocation
        let editing = panel.isKeyWindow && panel.firstResponder is NSTextView
        let interacting = editing || menuDepth > 0 || NSEvent.pressedMouseButtons != 0 || isBusy()
        switch hover.update(inActivation: activation.contains(point), inRetention: retention.contains(point),
                            expanded: expanded, interacting: interacting, now: ProcessInfo.processInfo.systemUptime) {
        case .expand: expanded = true
        case .collapse: expanded = false
        case nil: break
        }
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
