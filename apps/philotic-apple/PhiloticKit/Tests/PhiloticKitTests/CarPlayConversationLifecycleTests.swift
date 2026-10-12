import XCTest
@testable import PhiloticKit

@MainActor
final class CarPlayConversationLifecycleTests: XCTestCase {
    func testLaunchNeverStartsAndPreparationIsExactContext() async {
        var starts = 0
        let lifecycle = CarPlayConversationLifecycle(start: { starts += 1; return true }, stop: {})
        lifecycle.connect()
        lifecycle.start(context: "beacon")
        XCTAssertEqual(starts, 0)
        lifecycle.prepare(context: "beacon", generation: lifecycle.preparationGeneration)
        lifecycle.start(context: "jane")
        XCTAssertEqual(lifecycle.state, .ready)
        lifecycle.start(context: "beacon")
        lifecycle.start(context: "beacon")
        await settle()
        XCTAssertEqual(starts, 1)
        XCTAssertEqual(lifecycle.state, .active)
    }

    func testDisconnectDuringPermissionWaitCleansUpLateStartOnce() async {
        var continuation: CheckedContinuation<Bool, Never>?
        var stops = 0
        let lifecycle = CarPlayConversationLifecycle(start: {
            await withCheckedContinuation { continuation = $0 }
        }, stop: { stops += 1 })
        lifecycle.connect()
        lifecycle.prepare(context: "beacon", generation: lifecycle.preparationGeneration)
        lifecycle.start(context: "beacon")
        await settle()
        lifecycle.end(disconnect: true)
        lifecycle.end(disconnect: true)
        lifecycle.connect()
        lifecycle.prepare(context: "beacon", generation: lifecycle.preparationGeneration)
        lifecycle.start(context: "beacon")
        XCTAssertEqual(lifecycle.state, .stopping)
        continuation?.resume(returning: true)
        await settle()
        XCTAssertEqual(stops, 1)
        XCTAssertEqual(lifecycle.state, .needsPreparation)
        XCTAssertNil(lifecycle.preparedContext)
    }

    func testDenialAndConnectionLossRequireFreshPreparation() async {
        var allowed = false
        var stops = 0
        let lifecycle = CarPlayConversationLifecycle(start: { allowed }, stop: { stops += 1 })
        lifecycle.connect()
        lifecycle.prepare(context: "beacon", generation: lifecycle.preparationGeneration)
        lifecycle.start(context: "beacon")
        await settle()
        XCTAssertEqual(lifecycle.state, .failed)
        XCTAssertNil(lifecycle.preparedContext)
        XCTAssertEqual(stops, 1)
        allowed = true
        lifecycle.prepare(context: "beacon", generation: lifecycle.preparationGeneration)
        lifecycle.start(context: "beacon")
        await settle()
        lifecycle.validate(context: "beacon", available: false)
        await settle()
        XCTAssertEqual(lifecycle.state, .needsPreparation)
    }

    func testSelectionChangeAndEndNeverResumeAutomatically() async {
        var starts = 0
        let lifecycle = CarPlayConversationLifecycle(start: { starts += 1; return true }, stop: {})
        lifecycle.connect()
        lifecycle.prepare(context: "beacon/one", generation: lifecycle.preparationGeneration)
        lifecycle.validate(context: "beacon/two", available: true)
        lifecycle.start(context: "beacon/two")
        XCTAssertEqual(starts, 0)
        lifecycle.prepare(context: "beacon/two", generation: lifecycle.preparationGeneration)
        lifecycle.start(context: "beacon/two")
        await settle()
        lifecycle.end()
        await settle()
        lifecycle.start(context: "beacon/two")
        XCTAssertEqual(starts, 1)
        XCTAssertEqual(lifecycle.state, .needsPreparation)
    }

    func testPhonePreparationCannotSurviveCarDisconnectAndReconnect() {
        let lifecycle = CarPlayConversationLifecycle(start: { true }, stop: {})
        lifecycle.connect()
        let pendingPhonePermission = lifecycle.preparationGeneration
        lifecycle.end(disconnect: true)
        lifecycle.connect()
        lifecycle.prepare(context: "same-hotel/same-agent/same-conversation", generation: pendingPhonePermission)
        XCTAssertEqual(lifecycle.state, .needsPreparation)
        XCTAssertNil(lifecycle.preparedContext)
    }

    private func settle() async { for _ in 0..<30 { await Task.yield() } }
}
