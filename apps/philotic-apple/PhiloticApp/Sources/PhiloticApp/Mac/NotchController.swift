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
    var expanded = false { didSet { position() } }
    private(set) var isVisible = false
    @ObservationIgnored private var panel: NSPanel?
    @ObservationIgnored private var displayObserver: NSObjectProtocol?
    @ObservationIgnored private var lockObserver: NSObjectProtocol?

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
        displayObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.didChangeScreenParametersNotification, object: nil, queue: .main
        ) { [weak self] _ in Task { @MainActor in self?.position() } }
        lockObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.sessionDidResignActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in Task { @MainActor in self?.expanded = false } }
    }

    func show() {
        position()
        panel?.orderFrontRegardless()
        isVisible = panel != nil
    }

    func hide() {
        expanded = false
        panel?.orderOut(nil)
        isVisible = false
    }

    private func position() {
        guard let panel, let screen = panel.screen ?? NSScreen.main else { return }
        panel.setFrame(CompanionPanelLayout.frame(screen: screen.frame, visible: screen.visibleFrame,
                                                 safeTop: screen.safeAreaInsets.top, expanded: expanded),
                       display: true)
        if expanded { panel.makeKey() }
    }

    deinit {
        if let displayObserver { NotificationCenter.default.removeObserver(displayObserver) }
        if let lockObserver { NSWorkspace.shared.notificationCenter.removeObserver(lockObserver) }
    }
}
#endif
