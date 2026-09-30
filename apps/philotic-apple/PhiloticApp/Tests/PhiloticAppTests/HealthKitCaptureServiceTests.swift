import Foundation
import PhiloticKit
import XCTest

@testable import PhiloticApp

#if os(iOS)
    import HealthKit
#endif

@MainActor
final class HealthKitCaptureServiceTests: XCTestCase {
    private let url = URL(string: "https://hotel.example")!
    private let now = ISO8601DateFormatter().date(from: "2026-09-04T15:00:00Z")!
    private var calendar: Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "America/New_York")!
        return calendar
    }

    func testDefaultSelectionIsEmptyAndDoesNotRequestAccessOrPost() async {
        let reader = StubHealthReader()
        let health = HealthKitCaptureService(reader: reader, post: unexpectedPost)
        XCTAssertTrue(health.enabledMetrics.isEmpty)
        await health.preparePreview()
        XCTAssertTrue(reader.authorizations.isEmpty)
        XCTAssertTrue(reader.queries.isEmpty)
        XCTAssertNil(health.previewID)
    }

    func testUnavailableNeverSynthesizesOrPosts() async {
        let reader = StubHealthReader()
        reader.isAvailable = false
        let health = HealthKitCaptureService(reader: reader, post: unexpectedPost)
        health.enabledMetrics = [.steps]
        await health.preparePreview()
        XCTAssertTrue(reader.authorizations.isEmpty)
        XCTAssertTrue(reader.queries.isEmpty)
        XCTAssertTrue(health.previewItems.isEmpty)
        XCTAssertNotNil(health.lastError)
    }

    func testReadOnlySelectedMetricsForBoundedCompletedDaysWithoutPosting() async {
        let reader = StubHealthReader()
        let health = HealthKitCaptureService(reader: reader, post: unexpectedPost)
        health.enabledMetrics = [.steps, .sleep]
        health.window = .week
        await health.preparePreview(now: now, calendar: calendar)
        XCTAssertEqual(reader.authorizations, [[.steps, .sleep]])
        XCTAssertEqual(reader.queries.count, 14)
        XCTAssertTrue(reader.queries.allSatisfy { $0.1.end <= calendar.startOfDay(for: now) })
        XCTAssertEqual(Set(reader.queries.map { $0.0 }), [.steps, .sleep])
        XCTAssertEqual(health.previewItems.count, 14)
        XCTAssertNotNil(health.previewID)
    }

    func testNoReadableSamplesIsNotZeroOrDisabledMetrics() async {
        let reader = StubHealthReader()
        reader.value = nil
        let health = HealthKitCaptureService(reader: reader, post: unexpectedPost)
        health.enabledMetrics = [.steps]
        await health.preparePreview()
        XCTAssertTrue(health.previewItems.isEmpty)
        XCTAssertEqual(health.missingCount, 1)
        XCTAssertTrue(health.statusMessage?.contains("No readable data") == true)
        XCTAssertNil(health.previewID)
    }

    func testValidZeroIsPreserved() async {
        let reader = StubHealthReader()
        reader.value = 0
        let health = HealthKitCaptureService(reader: reader, post: unexpectedPost)
        health.enabledMetrics = [.steps]
        await health.preparePreview()
        XCTAssertEqual(
            health.previewItems.first?.observation.evidence.metadata["value"], .number(0))
    }

    func testAuthorizationAndQueryErrorsDoNotCreatePreview() async {
        for failAuthorization in [true, false] {
            let reader = StubHealthReader()
            reader.failAuthorization = failAuthorization
            reader.failQuery = !failAuthorization
            let health = HealthKitCaptureService(reader: reader, post: unexpectedPost)
            health.enabledMetrics = [.steps]
            await health.preparePreview()
            XCTAssertNil(health.previewID)
            XCTAssertTrue(health.previewItems.isEmpty)
            XCTAssertNotNil(health.lastError)
            if failAuthorization { XCTAssertTrue(reader.queries.isEmpty) }
        }
    }

    func testInvalidValuesCannotBeShared() async {
        for value in [Double.nan, Double.infinity, -1] {
            let reader = StubHealthReader()
            reader.value = value
            let health = HealthKitCaptureService(reader: reader, post: unexpectedPost)
            health.enabledMetrics = [.steps]
            await health.preparePreview()
            XCTAssertTrue(health.previewItems.isEmpty)
            XCTAssertNotNil(health.lastError)
        }
    }

    func testChangedSelectionOrDiscardInvalidatesConfirmation() async throws {
        let health = HealthKitCaptureService(reader: StubHealthReader(), post: unexpectedPost)
        health.enabledMetrics = [.steps]
        await health.preparePreview()
        let oldID = try XCTUnwrap(health.previewID)
        health.enabledMetrics = [.sleep]
        await health.sharePreview(id: oldID, baseURL: url, bearerToken: "test")
        XCTAssertNil(health.previewID)
        await health.preparePreview()
        let secondID = try XCTUnwrap(health.previewID)
        health.discardPreview()
        await health.sharePreview(id: secondID, baseURL: url, bearerToken: "test")
        XCTAssertTrue(health.previewItems.isEmpty)
    }

    func testDiscardDuringReadDoesNotRepopulatePreview() async {
        let reader = StubHealthReader()
        let health = HealthKitCaptureService(reader: reader, post: unexpectedPost)
        reader.duringQuery = { health.discardPreview() }
        health.enabledMetrics = [.steps]
        await health.preparePreview()
        XCTAssertNil(health.previewID)
        XCTAssertTrue(health.previewItems.isEmpty)
    }

    func testOnlyExactPreviewCanBeSharedAndCannotBeSentTwice() async throws {
        var sent: [LifeObservation] = []
        var posts = 0
        let health = HealthKitCaptureService(reader: StubHealthReader()) {
            observations, url, token in
            sent = observations
            posts += 1
            XCTAssertEqual(url.host, "hotel.example")
            XCTAssertEqual(token, "test")
            return Self.acknowledge(observations)
        }
        health.enabledMetrics = [.steps]
        await health.preparePreview()
        let preview = health.previewItems.map(\.observation)
        let id = try XCTUnwrap(health.previewID)
        await health.sharePreview(id: UUID(), baseURL: url, bearerToken: "test")
        XCTAssertEqual(posts, 0)
        await health.sharePreview(id: id, baseURL: url, bearerToken: "test")
        XCTAssertEqual(sent, preview)
        XCTAssertEqual(posts, 1)
        XCTAssertNil(health.lastError)
        await health.sharePreview(id: id, baseURL: url, bearerToken: "test")
        XCTAssertEqual(posts, 1)
    }

    func testPartialFailedUnknownAndMissingAcknowledgmentAreNotSuccess() async throws {
        for status in ["partial", "error", "failed", "unknown", "ok"] {
            var posts = 0
            let health = HealthKitCaptureService(reader: StubHealthReader()) { _, _, _ in
                posts += 1
                return ObserveResult(status: status, results: [])
            }
            health.enabledMetrics = [.steps]
            await health.preparePreview()
            let id = try XCTUnwrap(health.previewID)
            await health.sharePreview(id: id, baseURL: url, bearerToken: "test")
            XCTAssertNotNil(health.lastError, status)
            XCTAssertNil(health.statusMessage)
            XCTAssertNil(health.previewID)
            await health.sharePreview(id: id, baseURL: url, bearerToken: "test")
            XCTAssertEqual(posts, 1)
        }
    }

    func testNetworkFailureDoesNotClaimSuccessOrRetry() async throws {
        var posts = 0
        let health = HealthKitCaptureService(reader: StubHealthReader()) { _, _, _ in
            posts += 1
            throw URLError(.timedOut)
        }
        health.enabledMetrics = [.steps]
        await health.preparePreview()
        await health.sharePreview(
            id: try XCTUnwrap(health.previewID), baseURL: url, bearerToken: "test")
        XCTAssertEqual(posts, 1)
        XCTAssertNil(health.previewID)
        XCTAssertNotNil(health.lastError)
    }

    func testCompletedDaysRespectDSTAndUseLocalCalendarNotUTCLabels() throws {
        let spring = ISO8601DateFormatter().date(from: "2026-03-09T12:00:00Z")!
        let fall = ISO8601DateFormatter().date(from: "2026-11-02T12:00:00Z")!
        XCTAssertEqual(
            try XCTUnwrap(
                HealthKitCaptureService.completedDays(before: spring, count: 1, calendar: calendar)
                    .first
            ).duration, 23 * 3600)
        XCTAssertEqual(
            try XCTUnwrap(
                HealthKitCaptureService.completedDays(before: fall, count: 1, calendar: calendar)
                    .first
            ).duration, 25 * 3600)
        XCTAssertEqual(
            HealthKitCaptureService.completedDays(before: now, count: 99, calendar: calendar).count,
            7)
    }

    func testSleepClipsMidnightAndUnionsOverlapsAcrossSources() {
        func interval(_ a: Double, _ b: Double) -> DateInterval {
            DateInterval(
                start: Date(timeIntervalSince1970: a * 60), end: Date(timeIntervalSince1970: b * 60)
            )
        }
        let day = interval(0, 1440)
        let samples = [
            interval(-120, 60), interval(30, 180), interval(180, 240), interval(60, 90),
            interval(1380, 1500),
        ]
        XCTAssertEqual(HealthKitCaptureService.asleepMinutes(samples, within: day), 300)
        XCTAssertNil(HealthKitCaptureService.asleepMinutes([], within: day))
        XCTAssertNil(HealthKitCaptureService.asleepMinutes([interval(-120, 0)], within: day))
    }

    func testObservationsAreUniqueTimestampedProposedSignalsWithDurableText() throws {
        let interval = try XCTUnwrap(
            HealthKitCaptureService.completedDays(before: now, count: 1, calendar: calendar).first)
        let first = HealthKitCaptureService.observation(
            metric: .steps, value: 1200, interval: interval, capturedAt: now,
            timeZone: calendar.timeZone)
        let next = HealthKitCaptureService.observation(
            metric: .steps, value: 1300, interval: interval, capturedAt: now,
            timeZone: calendar.timeZone)
        XCTAssertNotEqual(first.evidence.claimRef.id, next.evidence.claimRef.id)
        XCTAssertEqual(first.evidence.claimRef.label, "Signal")
        XCTAssertEqual(first.evidence.validationState, "proposed")
        XCTAssertEqual(first.evidence.observedAt, "2026-09-04T15:00:00Z")
        XCTAssertEqual(first.evidence.metadata["period_start"], .string("2026-09-03T04:00:00Z"))
        XCTAssertTrue(first.evidence.claimSummary.contains("1200.0 count"))
        XCTAssertTrue(first.evidence.claimSummary.contains("America/New_York"))
        XCTAssertEqual(first.evidence.sourceRefs.first?.sourceKind, "imported_record")
    }

    #if os(iOS)
        func testHealthKitTypesAreLimitedToSelection() {
            XCTAssertEqual(
                DeviceHealthReader.readTypes(for: [.steps]),
                [HKObjectType.quantityType(forIdentifier: .stepCount)!])
            XCTAssertTrue(DeviceHealthReader.readTypes(for: []).isEmpty)
            XCTAssertEqual(
                DeviceHealthReader.readTypes(for: [.sleep]),
                [HKObjectType.categoryType(forIdentifier: .sleepAnalysis)!])
        }
    #else
        func testMacReaderIsUnavailable() { XCTAssertFalse(DeviceHealthReader().isAvailable) }
    #endif

    private func unexpectedPost(_ observations: [LifeObservation], _ url: URL, _ token: String)
        async throws -> ObserveResult
    {
        XCTFail("Read/invalidated preview must never post")
        return ObserveResult(status: "error", results: [])
    }

    private static func acknowledge(_ observations: [LifeObservation]) -> ObserveResult {
        ObserveResult(
            status: "ok",
            results: observations.map {
                ObserveResultItem(
                    observationId: $0.observationId, status: "proposed", message: nil,
                    nodeId: $0.evidence.claimRef.id)
            })
    }
}

@MainActor
private final class StubHealthReader: HealthDataReading {
    var isAvailable = true
    var value: Double? = 42
    var failAuthorization = false
    var failQuery = false
    var duringQuery: (() -> Void)?
    var authorizations: [Set<HealthMetric>] = []
    var queries: [(HealthMetric, DateInterval)] = []
    func requestAuthorization(for metrics: Set<HealthMetric>) async throws {
        authorizations.append(metrics)
        if failAuthorization { throw HealthCaptureError.authorizationFailed }
    }
    func aggregate(_ metric: HealthMetric, interval: DateInterval) async throws -> Double? {
        queries.append((metric, interval))
        duringQuery?()
        if failQuery { throw HealthCaptureError.unavailable }
        return value
    }
}
