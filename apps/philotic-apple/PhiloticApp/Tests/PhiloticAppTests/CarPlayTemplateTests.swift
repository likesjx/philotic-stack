#if os(iOS) && PHILOTIC_CARPLAY
import CarPlay
import XCTest
@testable import PhiloticApp

@MainActor
final class CarPlayTemplateTests: XCTestCase {
    func testVoiceTemplateHasOnlyStatusAndTwoControls() throws {
        guard #available(iOS 26.4, *) else { throw XCTSkip("Voice conversational category requires iOS 26.4") }
        let template = CarPlaySceneDelegate.makeTemplate(start: {}, end: {})
        XCTAssertEqual(template.voiceControlStates.count, 8)
        for state in template.voiceControlStates {
            XCTAssertEqual(state.actionButtons.map(\.title), ["Talk", "End"])
            XCTAssertEqual(state.actionButtons[0].isEnabled, state.identifier == "ready")
            XCTAssertEqual(state.actionButtons[1].isEnabled, ["starting", "active", "speaking"].contains(state.identifier))
        }
    }

    func testSceneImplementsActualObjectiveCConnectionCallbacks() throws {
        guard #available(iOS 26.4, *) else { throw XCTSkip("Voice conversational category requires iOS 26.4") }
        let delegate = CarPlaySceneDelegate()
        XCTAssertTrue(delegate.responds(to: #selector(CPTemplateApplicationSceneDelegate.templateApplicationScene(_:didConnect:))))
        XCTAssertTrue(delegate.responds(to: #selector(CPTemplateApplicationSceneDelegate.templateApplicationScene(_:didDisconnectInterfaceController:))))
        XCTAssertEqual(CarPlayBridge.shared.lifecycle.state, .disconnected)
        XCTAssertNil(CarPlayBridge.shared.session)
    }
}
#endif
