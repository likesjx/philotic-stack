#if os(macOS)
import XCTest
@testable import PhiloticApp

final class NotchHoverTests: XCTestCase {
    func testBriefPassDoesNotOpenButDwellDoes() {
        var state = NotchHoverState()
        XCTAssertNil(state.update(inActivation: true, inRetention: true, expanded: false, interacting: false, now: 0))
        XCTAssertNil(state.update(inActivation: false, inRetention: false, expanded: false, interacting: false, now: 0.1))
        XCTAssertNil(state.update(inActivation: true, inRetention: true, expanded: false, interacting: false, now: 1))
        XCTAssertEqual(state.update(inActivation: true, inRetention: true, expanded: false, interacting: false, now: 1.2), .expand)
    }

    func testLeaveGraceAndReentry() {
        var state = NotchHoverState()
        XCTAssertNil(state.update(inActivation: true, inRetention: true, expanded: true, interacting: false, now: -1))
        XCTAssertNil(state.update(inActivation: false, inRetention: false, expanded: true, interacting: false, now: 0))
        XCTAssertNil(state.update(inActivation: false, inRetention: true, expanded: true, interacting: false, now: 0.3))
        XCTAssertNil(state.update(inActivation: false, inRetention: false, expanded: true, interacting: false, now: 1))
        XCTAssertNil(state.update(inActivation: false, inRetention: false, expanded: true, interacting: false, now: 1.3))
        XCTAssertEqual(state.update(inActivation: false, inRetention: false, expanded: true, interacting: false, now: 1.5), .collapse)
    }

    func testEditingMenuOrVoiceInteractionPreventsCollapse() {
        var state = NotchHoverState()
        XCTAssertNil(state.update(inActivation: true, inRetention: true, expanded: true, interacting: false, now: -1))
        XCTAssertNil(state.update(inActivation: false, inRetention: false, expanded: true, interacting: false, now: 0))
        XCTAssertNil(state.update(inActivation: false, inRetention: false, expanded: true, interacting: true, now: 1))
        XCTAssertNil(state.update(inActivation: false, inRetention: false, expanded: true, interacting: false, now: 2))
        XCTAssertEqual(state.update(inActivation: false, inRetention: false, expanded: true, interacting: false, now: 2.5), .collapse)
    }

    func testManualCloseDoesNotReopenUntilPointerLeavesAndReturns() {
        var state = NotchHoverState()
        state.suppressUntilExit()
        XCTAssertNil(state.update(inActivation: true, inRetention: true, expanded: false, interacting: false, now: 0))
        XCTAssertNil(state.update(inActivation: true, inRetention: true, expanded: false, interacting: false, now: 2))
        XCTAssertNil(state.update(inActivation: false, inRetention: false, expanded: false, interacting: false, now: 3))
        XCTAssertNil(state.update(inActivation: true, inRetention: true, expanded: false, interacting: false, now: 4))
        XCTAssertEqual(state.update(inActivation: true, inRetention: true, expanded: false, interacting: false, now: 4.2), .expand)
    }

    func testDraggingOverNotchDoesNotOpen() {
        var state = NotchHoverState()
        XCTAssertNil(state.update(inActivation: true, inRetention: true, expanded: false, interacting: true, now: 0))
        XCTAssertNil(state.update(inActivation: true, inRetention: true, expanded: false, interacting: true, now: 10))
    }

    func testKeyboardOpeningWaitsForPointerVisitBeforeAutoCollapse() {
        var state = NotchHoverState()
        XCTAssertNil(state.update(inActivation: false, inRetention: false, expanded: true, interacting: false, now: 0))
        XCTAssertNil(state.update(inActivation: false, inRetention: false, expanded: true, interacting: false, now: 10))
        XCTAssertNil(state.update(inActivation: false, inRetention: true, expanded: true, interacting: false, now: 11))
        XCTAssertNil(state.update(inActivation: false, inRetention: false, expanded: true, interacting: false, now: 12))
        XCTAssertEqual(state.update(inActivation: false, inRetention: false, expanded: true, interacting: false, now: 12.5), .collapse)
    }

    func testPhysicalNotchAndTransitionCorridorOnOffsetDisplay() {
        let screen = CGRect(x: -1800, y: 400, width: 1800, height: 1200)
        let collapsed = CGRect(x: -1015, y: 1518, width: 230, height: 38)
        let camera = CGRect(x: -1000, y: 1568, width: 200, height: 32)
        let activation = NotchHoverRegion.activation(screen: screen, collapsed: collapsed, camera: camera)
        XCTAssertTrue(activation.contains(CGPoint(x: -900, y: 1598)))
        XCTAssertTrue(activation.contains(CGPoint(x: -900, y: 1562)))
        XCTAssertFalse(activation.contains(CGPoint(x: -1700, y: 1590)))
        let panel = CGRect(x: -1150, y: 1016, width: 500, height: 540)
        let retention = NotchHoverRegion.retention(screen: screen, activation: activation, panel: panel)
        XCTAssertTrue(retention.contains(CGPoint(x: -900, y: 1500)))
        XCTAssertTrue(retention.contains(CGPoint(x: -640, y: 1300)))
        XCTAssertTrue(screen.contains(retention))
    }

    func testNonNotchedFallbackUsesTopCenterNotWholeMenuBar() {
        let screen = CGRect(x: 200, y: 0, width: 1440, height: 900)
        let collapsed = CGRect(x: 805, y: 832, width: 230, height: 38)
        let region = NotchHoverRegion.activation(screen: screen, collapsed: collapsed, camera: nil)
        XCTAssertTrue(region.contains(CGPoint(x: 920, y: 899)))
        XCTAssertFalse(region.contains(CGPoint(x: 250, y: 899)))
        XCTAssertTrue(screen.contains(region))
    }
}
#endif
