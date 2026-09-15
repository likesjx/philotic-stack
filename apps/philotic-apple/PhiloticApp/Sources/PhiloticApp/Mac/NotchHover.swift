#if os(macOS)
import Foundation

/// Screen-coordinate geometry; includes a bridge from the camera to the panel.
enum NotchHoverRegion {
    static func activation(screen: CGRect, collapsed: CGRect, camera: CGRect?) -> CGRect {
        let top = camera ?? CGRect(x: collapsed.minX, y: screen.maxY - 24,
                                   width: collapsed.width, height: 24)
        return collapsed.union(top).insetBy(dx: -18, dy: -10).intersection(screen)
    }

    static func retention(screen: CGRect, activation: CGRect, panel: CGRect) -> CGRect {
        activation.union(panel.insetBy(dx: -16, dy: -16)).intersection(screen)
    }
}

/// Pure timing policy, driven by monotonic time rather than asynchronous sleeps.
struct NotchHoverState {
    enum Action: Equatable { case expand, collapse }
    static let openingDelay: TimeInterval = 0.15
    static let closingDelay: TimeInterval = 0.45
    private var enteredAt: TimeInterval?
    private var exitedAt: TimeInterval?
    private var suppressedUntilExit = false
    private var visitedExpandedRegion = false

    mutating func suppressUntilExit() {
        enteredAt = nil
        exitedAt = nil
        suppressedUntilExit = true
        visitedExpandedRegion = false
    }

    mutating func update(inActivation: Bool, inRetention: Bool, expanded: Bool,
                         interacting: Bool, now: TimeInterval) -> Action? {
        if !inActivation { suppressedUntilExit = false }
        if expanded {
            enteredAt = nil
            if inRetention { visitedExpandedRegion = true }
            if inRetention || interacting { exitedAt = nil; return nil }
            // A keyboard/menu opening must not disappear before the operator
            // has had a chance to reach the panel.
            guard visitedExpandedRegion else { return nil }
            if exitedAt == nil { exitedAt = now }
            if now - (exitedAt ?? now) >= Self.closingDelay {
                exitedAt = nil
                return .collapse
            }
        } else {
            exitedAt = nil
            visitedExpandedRegion = false
            guard inActivation, !suppressedUntilExit, !interacting else {
                enteredAt = nil
                return nil
            }
            if enteredAt == nil { enteredAt = now }
            if now - (enteredAt ?? now) >= Self.openingDelay {
                enteredAt = nil
                visitedExpandedRegion = true
                return .expand
            }
        }
        return nil
    }
}
#endif
