import XCTest
@testable import PhiloticApp

@MainActor
final class CompanionTests: XCTestCase {
    func testAgentSelectionRejectsSupersededHistoryIncludingReturnToSameAgent() {
        var gate = AgentSelectionGate()
        let firstA = gate.begin()
        let b = gate.begin()
        let secondA = gate.begin()
        XCTAssertFalse(gate.isCurrent(firstA))
        XCTAssertFalse(gate.isCurrent(b))
        XCTAssertTrue(gate.isCurrent(secondA))
    }

    func testNavigationDismissesOldSheet() {
        let router = CompanionRouter()
        router.sheet = .health
        router.open(.agents)
        XCTAssertEqual(router.tab, .agents)
        XCTAssertNil(router.sheet)
    }

    func testOpeningCurrentTabStillProducesHandoff() {
        let router = CompanionRouter()
        let previous = router.navigationID
        router.open(.today)
        XCTAssertNotEqual(router.navigationID, previous)
    }

    func testIntentsOnlyRouteToForegroundDestinations() async throws {
        let router = CompanionRouter.shared
        defer { router.open(.today) }
        router.sheet = .health
        _ = try await TalkToAgentsIntent().perform()
        XCTAssertEqual(router.tab, .agents)
        XCTAssertNil(router.sheet)
        _ = try await OpenLifeGraphIntent().perform()
        XCTAssertEqual(router.tab, .life)
        _ = try await OpenTodayIntent().perform()
        XCTAssertEqual(router.tab, .today)
    }

    func testReminderReaderIsNotCalledUntilExplicitLoad() async {
        var calls = 0
        let store = ReminderPreviewStore { calls += 1; return [] }
        XCTAssertEqual(calls, 0)
        XCTAssertEqual(store.state, .idle)
        await store.load()
        XCTAssertEqual(calls, 1)
        XCTAssertEqual(store.state, .loaded)
        XCTAssertTrue(store.items.isEmpty)
    }

    func testReminderDenialAndFailureAreNotEmptySuccess() async {
        let denied = ReminderPreviewStore { throw ReminderPreviewError.denied }
        await denied.load()
        XCTAssertEqual(denied.state, .denied)
        let unavailable = ReminderPreviewStore { throw ReminderPreviewError.unavailable }
        await unavailable.load()
        XCTAssertEqual(unavailable.state, .failed)
    }

    func testReminderProjectionIsBoundedAndDiscardable() async {
        let store = ReminderPreviewStore {
            (0..<80).map { ReminderPreview(id: "\($0)", title: "Test", list: "Fixture", due: nil) }
        }
        await store.load()
        XCTAssertEqual(store.items.count, 50)
        store.discard()
        XCTAssertEqual(store.state, .idle)
        XCTAssertTrue(store.items.isEmpty)
    }

    func testDiscardInvalidatesLateReminderRead() async {
        var resume: CheckedContinuation<[ReminderPreview], Error>?
        let store = ReminderPreviewStore {
            try await withCheckedThrowingContinuation { resume = $0 }
        }
        let task = Task { await store.load() }
        while resume == nil { await Task.yield() }
        store.discard()
        resume?.resume(returning: [ReminderPreview(id: "late", title: "Private", list: "Test", due: nil)])
        await task.value
        XCTAssertEqual(store.state, .idle)
        XCTAssertTrue(store.items.isEmpty)
    }

    func testRemindersPurposeIsPackaged() {
        XCTAssertNotNil(Bundle.main.object(forInfoDictionaryKey: "NSRemindersFullAccessUsageDescription"))
    }

    #if os(macOS)
    func testPanelAvoidsNotchAndFitsOffsetExternalScreen() {
        for expanded in [false, true] {
            let screen = CGRect(x: -1920, y: 300, width: 1920, height: 1080)
            let visible = CGRect(x: -1920, y: 350, width: 1920, height: 990)
            let frame = CompanionPanelLayout.frame(screen: screen, visible: visible, safeTop: 40, expanded: expanded)
            XCTAssertTrue(visible.contains(frame))
            XCTAssertLessThan(frame.maxY, screen.maxY - 40)
            XCTAssertEqual(frame.midX, screen.midX)
        }
    }

    func testPanelClampsToSmallScreen() {
        let screen = CGRect(x: 0, y: 0, width: 400, height: 400)
        let visible = CGRect(x: 0, y: 0, width: 400, height: 375)
        let frame = CompanionPanelLayout.frame(screen: screen, visible: visible, safeTop: 25, expanded: true)
        XCTAssertTrue(visible.contains(frame))
    }
    #endif
}
