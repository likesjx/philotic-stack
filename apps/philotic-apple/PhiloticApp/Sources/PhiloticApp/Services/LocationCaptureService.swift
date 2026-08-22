// LocationCaptureService.swift
// Explicit, one-shot Core Location -> LifeGraph sharing. Each operator tap
// creates a timestamped Signal observation; the app never tracks location in
// the background. A new claim id per share is deliberate: life.observe keeps
// the original summary/timestamp when a claim id already exists.

import CoreLocation
import Foundation
import Observation
import PhiloticKit

public enum LocationSharingPrecision: String, CaseIterable, Identifiable, Sendable {
    case approximate
    case precise

    public var id: String { rawValue }

    public var title: String {
        switch self {
        case .approximate: return "Approximate"
        case .precise: return "Precise"
        }
    }

    public var detail: String {
        switch self {
        case .approximate: return "Rounds coordinates to about 1 km."
        case .precise: return "Shares coordinates to about 1 meter."
        }
    }

    fileprivate var desiredAccuracy: CLLocationAccuracy {
        switch self {
        case .approximate: return kCLLocationAccuracyKilometer
        case .precise: return kCLLocationAccuracyBest
        }
    }

    fileprivate var decimalPlaces: Int {
        switch self {
        case .approximate: return 2
        case .precise: return 5
        }
    }
}

struct SharedLocationSnapshot: Sendable {
    let latitude: Double
    let longitude: Double
    let horizontalAccuracyMeters: Double
    let observedAt: Date
    let placeName: String?
    let precision: LocationSharingPrecision
}

private enum LocationCaptureError: LocalizedError {
    case permissionDenied
    case unavailable
    case stale
    case requestFailed(String)

    var errorDescription: String? {
        switch self {
        case .permissionDenied:
            return "Location access is off. Enable it for PhiloticApp in Settings."
        case .unavailable:
            return "No location was available. Try again somewhere with a clearer signal."
        case .stale:
            return "The device returned an old location. Move to a clearer area and try again."
        case .requestFailed(let message):
            return "Location request failed: \(message)"
        }
    }
}

@MainActor
@Observable
public final class LocationCaptureService: NSObject, @preconcurrency CLLocationManagerDelegate {
    public static let observedBy = "edge:apple-corelocation"

    public var precision: LocationSharingPrecision {
        didSet {
            UserDefaults.standard.set(precision.rawValue, forKey: Self.precisionDefaultsKey)
        }
    }

    public private(set) var authorizationStatus: CLAuthorizationStatus
    public private(set) var isSharing = false
    public private(set) var lastSharedAt: Date?
    public private(set) var lastSharedSummary: String?
    public private(set) var lastError: String?

    public var authorizationDescription: String {
        #if os(macOS)
        switch authorizationStatus {
        case .notDetermined: return "Not requested"
        case .restricted: return "Restricted"
        case .denied: return "Denied"
        case .authorizedAlways: return "Allowed"
        @unknown default: return "Unknown"
        }
        #else
        switch authorizationStatus {
        case .notDetermined: return "Not requested"
        case .restricted: return "Restricted"
        case .denied: return "Denied"
        case .authorizedAlways: return "Allowed"
        case .authorizedWhenInUse: return "Allowed while using the app"
        @unknown default: return "Unknown"
        }
        #endif
    }

    private static let precisionDefaultsKey = "com.philotic.apple.location.precision"
    private static let maximumLocationAge: TimeInterval = 120

    private let manager: CLLocationManager
    private var authorizationContinuation: CheckedContinuation<CLAuthorizationStatus, Never>?
    private var locationContinuation: CheckedContinuation<CLLocation, Error>?

    public override init() {
        let manager = CLLocationManager()
        self.manager = manager
        authorizationStatus = manager.authorizationStatus
        precision = LocationSharingPrecision(
            rawValue: UserDefaults.standard.string(forKey: Self.precisionDefaultsKey) ?? ""
        ) ?? .approximate
        super.init()
        manager.delegate = self
    }

    /// Requests one location, converts it to a provenance-stamped LifeGraph
    /// Signal, and uploads it. There is intentionally no background mode.
    public func shareCurrentLocation(baseURL: URL, bearerToken: String) async {
        guard !isSharing else { return }
        isSharing = true
        lastError = nil
        defer { isSharing = false }

        do {
            let location = try await requestCurrentLocation()
            let placeName = await reverseGeocode(location)
            let snapshot = Self.snapshot(
                from: location,
                precision: precision,
                placeName: placeName
            )
            let observation = Self.observation(from: snapshot)
            let result = try await LifeGraphClient().postObservations(
                [observation], baseURL: baseURL, bearerToken: bearerToken)
            guard result.status != "error" else {
                throw LocationCaptureError.requestFailed(
                    "the LifeGraph rejected the location snapshot")
            }

            lastSharedAt = snapshot.observedAt
            lastSharedSummary = observation.evidence.claimSummary
        } catch {
            lastError = error.localizedDescription
        }
    }

    private func requestCurrentLocation() async throws -> CLLocation {
        let status = await requestAuthorizationIfNeeded()
        guard Self.isAuthorized(status) else {
            throw LocationCaptureError.permissionDenied
        }

        manager.desiredAccuracy = precision.desiredAccuracy
        let location = try await withCheckedThrowingContinuation {
            (continuation: CheckedContinuation<CLLocation, Error>) in
            locationContinuation = continuation
            manager.requestLocation()
        }

        guard Date().timeIntervalSince(location.timestamp) <= Self.maximumLocationAge else {
            throw LocationCaptureError.stale
        }
        return location
    }

    private func requestAuthorizationIfNeeded() async -> CLAuthorizationStatus {
        let current = manager.authorizationStatus
        authorizationStatus = current
        guard current == .notDetermined else { return current }

        return await withCheckedContinuation { continuation in
            authorizationContinuation = continuation
            #if os(macOS)
            manager.requestAlwaysAuthorization()
            #else
            manager.requestWhenInUseAuthorization()
            #endif
        }
    }

    private static func isAuthorized(_ status: CLAuthorizationStatus) -> Bool {
        #if os(macOS)
        return status == .authorizedAlways
        #else
        return status == .authorizedAlways || status == .authorizedWhenInUse
        #endif
    }

    private func reverseGeocode(_ location: CLLocation) async -> String? {
        do {
            guard let placemark = try await CLGeocoder().reverseGeocodeLocation(location).first else {
                return nil
            }

            let candidates: [String?]
            switch precision {
            case .approximate:
                candidates = [placemark.locality, placemark.administrativeArea, placemark.country]
            case .precise:
                candidates = [
                    placemark.name,
                    placemark.subLocality,
                    placemark.locality,
                    placemark.administrativeArea,
                    placemark.country,
                ]
            }

            var seen = Set<String>()
            let parts = candidates.compactMap { value -> String? in
                guard let value, !value.isEmpty, seen.insert(value).inserted else { return nil }
                return value
            }
            return parts.isEmpty ? nil : parts.joined(separator: ", ")
        } catch {
            // Coordinates still make a valid observation when reverse
            // geocoding is unavailable or the device is offline.
            return nil
        }
    }

    public func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
        let status = manager.authorizationStatus
        authorizationStatus = status
        guard status != .notDetermined, let continuation = authorizationContinuation else {
            return
        }
        authorizationContinuation = nil
        continuation.resume(returning: status)
    }

    public func locationManager(
        _ manager: CLLocationManager,
        didUpdateLocations locations: [CLLocation]
    ) {
        guard let continuation = locationContinuation else { return }
        locationContinuation = nil

        guard let location = locations.last(where: { $0.horizontalAccuracy >= 0 }) else {
            continuation.resume(throwing: LocationCaptureError.unavailable)
            return
        }
        continuation.resume(returning: location)
    }

    public func locationManager(_ manager: CLLocationManager, didFailWithError error: Error) {
        guard let continuation = locationContinuation else { return }
        locationContinuation = nil
        if let locationError = error as? CLError, locationError.code == .denied {
            continuation.resume(throwing: LocationCaptureError.permissionDenied)
        } else {
            continuation.resume(
                throwing: LocationCaptureError.requestFailed(error.localizedDescription))
        }
    }

    static func snapshot(
        from location: CLLocation,
        precision: LocationSharingPrecision,
        placeName: String?
    ) -> SharedLocationSnapshot {
        let factor = pow(10.0, Double(precision.decimalPlaces))
        let latitude = (location.coordinate.latitude * factor).rounded() / factor
        let longitude = (location.coordinate.longitude * factor).rounded() / factor
        let accuracy = precision == .approximate
            ? max(location.horizontalAccuracy, 1_000)
            : location.horizontalAccuracy
        return SharedLocationSnapshot(
            latitude: latitude,
            longitude: longitude,
            horizontalAccuracyMeters: accuracy,
            observedAt: location.timestamp,
            placeName: placeName,
            precision: precision
        )
    }

    static func observation(from snapshot: SharedLocationSnapshot) -> LifeObservation {
        let observedAt = rfc3339Formatter.string(from: snapshot.observedAt)
        let coordinates = String(
            format: "%.*f, %.*f",
            snapshot.precision.decimalPlaces,
            snapshot.latitude,
            snapshot.precision.decimalPlaces,
            snapshot.longitude
        )
        let accuracy = Int(snapshot.horizontalAccuracyMeters.rounded())
        let placePrefix = snapshot.placeName.map { "\($0); " } ?? ""
        let summary =
            "Operator location snapshot: \(placePrefix)\(coordinates) "
            + "(\(snapshot.precision.title.lowercased()), ±\(accuracy) m), observed \(observedAt)."

        return LifeObservation(
            observationId: "obs-location-\(UUID().uuidString)",
            evidence: EvidencePacket(
                packetId: "pkt-\(UUID().uuidString)",
                claimRef: GraphRecordRef(
                    id: "signal:operator-location:\(UUID().uuidString)",
                    label: "Signal",
                    datasource: "memgraph"
                ),
                claimSummary: summary,
                sourceRefs: [
                    SourceRef(
                        sourceId: observedBy,
                        sourceKind: "runtime_observation",
                        reliability: Reliability(score: 0.95, basis: "direct_observation")
                    )
                ],
                confidence: 0.95,
                // A sensor snapshot is strong evidence, but `life.observe`
                // proposes facts; confirmation remains a separate authority.
                validationState: "proposed",
                observedAt: observedAt,
                sourceReliability: 0.95,
                adjudicationStatus: "not_needed",
                metadata: [
                    "signal_type": .string("operator_location_snapshot"),
                    "latitude": .number(snapshot.latitude),
                    "longitude": .number(snapshot.longitude),
                    "horizontal_accuracy_m": .number(snapshot.horizontalAccuracyMeters),
                    "shared_precision": .string(snapshot.precision.rawValue),
                    "snapshot_only": .bool(true),
                ]
            ),
            observedBy: observedBy,
            observedRole: "sensor"
        )
    }

    private static let rfc3339Formatter: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.timeZone = TimeZone(identifier: "UTC")
        formatter.formatOptions = [.withInternetDateTime]
        return formatter
    }()
}
