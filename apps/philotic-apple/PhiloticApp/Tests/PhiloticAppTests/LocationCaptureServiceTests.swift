import CoreLocation
import PhiloticKit
import XCTest

@testable import PhiloticApp

@MainActor
final class LocationCaptureServiceTests: XCTestCase {
    func testApproximateSnapshotRoundsCoordinatesAndExpandsAccuracy() {
        let location = CLLocation(
            coordinate: CLLocationCoordinate2D(latitude: 33.749_123, longitude: -84.388_456),
            altitude: 0,
            horizontalAccuracy: 12,
            verticalAccuracy: 10,
            timestamp: Date(timeIntervalSince1970: 0)
        )

        let snapshot = LocationCaptureService.snapshot(
            from: location,
            precision: .approximate,
            placeName: "Atlanta, Georgia, United States"
        )

        XCTAssertEqual(snapshot.latitude, 33.75)
        XCTAssertEqual(snapshot.longitude, -84.39)
        XCTAssertEqual(snapshot.horizontalAccuracyMeters, 1_000)
        XCTAssertEqual(snapshot.precision, .approximate)
    }

    func testPreciseSnapshotKeepsFiveDecimalsAndMeasuredAccuracy() {
        let location = CLLocation(
            coordinate: CLLocationCoordinate2D(latitude: 33.749_123, longitude: -84.388_456),
            altitude: 0,
            horizontalAccuracy: 12,
            verticalAccuracy: 10,
            timestamp: Date(timeIntervalSince1970: 0)
        )

        let snapshot = LocationCaptureService.snapshot(
            from: location,
            precision: .precise,
            placeName: nil
        )

        XCTAssertEqual(snapshot.latitude, 33.74912)
        XCTAssertEqual(snapshot.longitude, -84.38846)
        XCTAssertEqual(snapshot.horizontalAccuracyMeters, 12)
        XCTAssertEqual(snapshot.precision, .precise)
    }

    func testObservationIsTimestampedProposedSignalWithStructuredCoordinates() {
        let snapshot = SharedLocationSnapshot(
            latitude: 33.75,
            longitude: -84.39,
            horizontalAccuracyMeters: 1_000,
            observedAt: Date(timeIntervalSince1970: 0),
            placeName: "Atlanta, Georgia, United States",
            precision: .approximate
        )

        let observation = LocationCaptureService.observation(from: snapshot)

        XCTAssertTrue(observation.observationId.hasPrefix("obs-location-"))
        XCTAssertTrue(observation.evidence.claimRef.id.hasPrefix("signal:operator-location:"))
        XCTAssertEqual(observation.evidence.claimRef.label, "Signal")
        XCTAssertEqual(observation.evidence.validationState, "proposed")
        XCTAssertEqual(observation.evidence.observedAt, "1970-01-01T00:00:00Z")
        XCTAssertEqual(observation.observedBy, LocationCaptureService.observedBy)
        XCTAssertEqual(observation.observedRole, "sensor")
        XCTAssertEqual(observation.evidence.metadata["latitude"], .number(33.75))
        XCTAssertEqual(observation.evidence.metadata["longitude"], .number(-84.39))
        XCTAssertEqual(observation.evidence.metadata["shared_precision"], .string("approximate"))
        XCTAssertEqual(observation.evidence.metadata["snapshot_only"], .bool(true))
        XCTAssertTrue(observation.evidence.claimSummary.contains("Atlanta"))
        XCTAssertTrue(observation.evidence.claimSummary.contains("33.75, -84.39"))
    }
}
