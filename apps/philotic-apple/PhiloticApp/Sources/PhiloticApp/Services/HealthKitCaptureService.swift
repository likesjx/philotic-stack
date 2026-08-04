// HealthKitCaptureService.swift
// Pillar 3: HealthKit -> LifeGraph. Reads daily HealthKit aggregates and
// turns each metric/day into a LifeGraph `HealthMetric` observation, then
// POSTs them to the edge observe endpoint.
//
// Platform strategy: HealthKit *imports* on native macOS (the module exists
// in the macOS SDK) but there is no health data there, so all real query
// code is gated behind `#if os(iOS)`. On macOS (the build gate) the service
// reports `isAvailable == false` and a demo path synthesizes plausible
// sample observations so the end-to-end POST is exercisable. Keeping the
// gate on `os(iOS)` rather than `canImport(HealthKit)` is deliberate:
// canImport is TRUE on macOS, so it would not exclude the framework.

import Foundation
import Observation
import PhiloticKit

#if os(iOS)
    import HealthKit
#endif

/// A health metric the app can sync to the LifeGraph.
public enum HealthMetric: String, CaseIterable, Identifiable, Sendable {
    case steps = "step_count"
    case restingHeartRate = "resting_heart_rate"
    case heartRate = "heart_rate"
    case sleep = "sleep_asleep_duration"
    case activeEnergy = "active_energy_burned"

    public var id: String { rawValue }

    /// Human display name for the toggle row.
    public var displayName: String {
        switch self {
        case .steps: return "Steps"
        case .restingHeartRate: return "Resting Heart Rate"
        case .heartRate: return "Heart Rate (avg)"
        case .sleep: return "Sleep (asleep)"
        case .activeEnergy: return "Active Energy"
        }
    }

    public var systemImage: String {
        switch self {
        case .steps: return "figure.walk"
        case .restingHeartRate: return "heart"
        case .heartRate: return "waveform.path.ecg"
        case .sleep: return "bed.double"
        case .activeEnergy: return "flame"
        }
    }

    /// Unit string recorded in observation metadata (HealthKit-style).
    var unit: String {
        switch self {
        case .steps: return "count"
        case .restingHeartRate, .heartRate: return "count/min"
        case .sleep: return "min"
        case .activeEnergy: return "kcal"
        }
    }

    /// Hyphenated slug used inside the claim_ref id
    /// (`healthmetric:<slug>:<yyyy-MM-dd>`).
    var claimSlug: String {
        switch self {
        case .steps: return "steps"
        case .restingHeartRate: return "resting-hr"
        case .heartRate: return "heart-rate"
        case .sleep: return "sleep"
        case .activeEnergy: return "active-energy"
        }
    }

    /// Plausible sample value used on macOS / when HealthKit is unavailable,
    /// so the observe path is demoable without a device.
    var sampleValue: Double {
        switch self {
        case .steps: return 8000
        case .restingHeartRate: return 58
        case .heartRate: return 72
        case .sleep: return 431  // ~7h11m in minutes
        case .activeEnergy: return 540
        }
    }
}

/// One captured daily aggregate before it becomes a `LifeObservation`.
struct HealthSample {
    let metric: HealthMetric
    let value: Double
    /// The day this aggregate summarizes (local calendar day).
    let day: Date
}

@MainActor
@Observable
public final class HealthKitCaptureService {
    /// Source identifier stamped on every observation.
    public static let observedBy = "edge:ios-healthkit"

    /// True when real HealthKit data can be read (iOS with health data).
    public var isAvailable: Bool {
        #if os(iOS)
            return HKHealthStore.isHealthDataAvailable()
        #else
            return false
        #endif
    }

    /// Which metrics the operator wants synced. Persisted to UserDefaults.
    public var enabledMetrics: Set<HealthMetric> {
        didSet { persistEnabledMetrics() }
    }

    public private(set) var isSyncing = false
    public private(set) var lastSyncDate: Date?
    public private(set) var lastSyncSummary: String?
    public var lastError: String?

    private static let enabledDefaultsKey = "com.philotic.apple.health.enabledMetrics"

    #if os(iOS)
        private let healthStore = HKHealthStore()
    #endif

    public init() {
        if let raw = UserDefaults.standard.array(forKey: Self.enabledDefaultsKey) as? [String] {
            enabledMetrics = Set(raw.compactMap(HealthMetric.init(rawValue:)))
        } else {
            // Sensible default: everything on.
            enabledMetrics = Set(HealthMetric.allCases)
        }
    }

    private func persistEnabledMetrics() {
        UserDefaults.standard.set(
            enabledMetrics.map(\.rawValue), forKey: Self.enabledDefaultsKey)
    }

    // MARK: - Authorization

    /// Requests read authorization for the starter metric set. No-op (returns
    /// true) where HealthKit is unavailable — the sample path needs no grant.
    @discardableResult
    public func requestAuthorization() async -> Bool {
        #if os(iOS)
            guard HKHealthStore.isHealthDataAvailable() else { return false }
            let readTypes = Set(Self.allReadTypes())
            return await withCheckedContinuation { continuation in
                healthStore.requestAuthorization(toShare: [], read: readTypes) { granted, _ in
                    continuation.resume(returning: granted)
                }
            }
        #else
            return true
        #endif
    }

    // MARK: - Capture

    /// Captures one observation per enabled metric for `date`'s calendar day.
    /// Uses real HealthKit aggregates on iOS; synthesizes sample values
    /// elsewhere (macOS demo).
    public func captureDailySummaries(for date: Date) async -> [LifeObservation] {
        let metrics = HealthMetric.allCases.filter(enabledMetrics.contains)
        guard !metrics.isEmpty else { return [] }

        var samples: [HealthSample] = []
        for metric in metrics {
            if let value = await aggregate(metric: metric, day: date) {
                samples.append(HealthSample(metric: metric, value: value, day: date))
            }
        }
        return samples.map(Self.observation(from:))
    }

    /// Returns the daily aggregate for a metric, or nil when there's no data.
    private func aggregate(metric: HealthMetric, day: Date) async -> Double? {
        #if os(iOS)
            guard HKHealthStore.isHealthDataAvailable() else { return metric.sampleValue }
            let (start, end) = Self.dayBounds(for: day)
            let predicate = HKQuery.predicateForSamples(
                withStart: start, end: end, options: .strictStartDate)

            switch metric {
            case .sleep:
                return await sleepAsleepMinutes(predicate: predicate)
            case .steps, .activeEnergy:
                return await sumQuantity(metric: metric, predicate: predicate)
            case .restingHeartRate, .heartRate:
                return await averageQuantity(metric: metric, predicate: predicate)
            }
        #else
            // macOS / no HealthKit: plausible demo value.
            return metric.sampleValue
        #endif
    }

    // MARK: - Observation construction

    static func observation(from sample: HealthSample) -> LifeObservation {
        let dayString = dateOnlyFormatter.string(from: sample.day)
        let claimId = "healthmetric:\(sample.metric.claimSlug):\(dayString)"
        let observedAt = rfc3339Formatter.string(from: endOfDay(for: sample.day))
        let valueText = formatValue(sample.value, metric: sample.metric)
        let summary =
            "\(sample.metric.displayName) \(valueText) \(sample.metric.unit) (daily, \(dayString))"

        let evidence = EvidencePacket(
            packetId: "pkt-\(UUID().uuidString)",
            // LifeGraph label MUST be a known ontology label (cypher.rs
            // KNOWN_LABELS) — "HealthMetric" is rejected at Cypher compile
            // ("unknown Life Graph label"). Health readings are Signals; the
            // specific metric kind rides in metadata.metric.
            claimRef: GraphRecordRef(id: claimId, label: "Signal", datasource: "memgraph"),
            claimSummary: summary,
            sourceRefs: [
                SourceRef(
                    sourceId: observedBy,
                    sourceKind: "runtime_observation",
                    reliability: Reliability(score: 0.9, basis: "direct_observation")
                )
            ],
            confidence: 0.9,
            validationState: "proposed",
            observedAt: observedAt,
            sourceReliability: 0.9,
            adjudicationStatus: "not_needed",
            metadata: [
                "metric": .string(sample.metric.rawValue),
                "unit": .string(sample.metric.unit),
                "value": .number(sample.value),
            ]
        )
        return LifeObservation(
            observationId: "obs-\(UUID().uuidString)",
            evidence: evidence,
            observedBy: observedBy,
            observedRole: "sensor"
        )
    }

    // MARK: - Sync

    /// Captures the last `dayCount` days (inclusive of today) for enabled
    /// metrics and POSTs them via `LifeGraphClient`. Updates status state.
    public func syncNow(baseURL: URL, bearerToken: String, dayCount: Int = 7) async {
        guard !isSyncing else { return }
        isSyncing = true
        lastError = nil
        defer { isSyncing = false }

        _ = await requestAuthorization()

        let calendar = Calendar.current
        var observations: [LifeObservation] = []
        for offset in 0..<max(1, dayCount) {
            guard let day = calendar.date(byAdding: .day, value: -offset, to: Date()) else { continue }
            observations += await captureDailySummaries(for: day)
        }

        guard !observations.isEmpty else {
            lastSyncSummary = "No metrics enabled — nothing to sync."
            lastSyncDate = Date()
            return
        }

        do {
            let result = try await LifeGraphClient().postObservations(
                observations, baseURL: baseURL, bearerToken: bearerToken)
            lastSyncDate = Date()
            let mode = isAvailable ? "HealthKit" : "sample data"
            lastSyncSummary =
                "Synced \(observations.count) observations (\(mode)) — status \(result.status)."
            if result.status == "error" {
                lastError = "Server rejected the observation batch."
            }
        } catch {
            lastError = "Sync failed: \(error.localizedDescription)"
            lastSyncSummary = nil
        }
    }

    // MARK: - Formatting helpers

    private static let dateOnlyFormatter: DateFormatter = {
        let f = DateFormatter()
        f.calendar = Calendar(identifier: .gregorian)
        f.locale = Locale(identifier: "en_US_POSIX")
        f.timeZone = TimeZone(identifier: "UTC")
        f.dateFormat = "yyyy-MM-dd"
        return f
    }()

    private static let rfc3339Formatter: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.timeZone = TimeZone(identifier: "UTC")
        f.formatOptions = [.withInternetDateTime]  // no fractional seconds
        return f
    }()

    private static func endOfDay(for day: Date) -> Date {
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = TimeZone(identifier: "UTC")!
        let startOfDay = cal.startOfDay(for: day)
        // 23:59:00 UTC of that day.
        return startOfDay.addingTimeInterval(86_340)
    }

    private static func formatValue(_ value: Double, metric: HealthMetric) -> String {
        switch metric {
        case .steps, .restingHeartRate, .heartRate, .sleep, .activeEnergy:
            // All whole-number-ish; drop the decimal when integral.
            return value == value.rounded() ? String(Int(value)) : String(format: "%.1f", value)
        }
    }
}

// MARK: - HealthKit query internals (iOS only)

#if os(iOS)
    extension HealthKitCaptureService {
        fileprivate static func dayBounds(for day: Date) -> (Date, Date) {
            let cal = Calendar.current
            let start = cal.startOfDay(for: day)
            let end = cal.date(byAdding: .day, value: 1, to: start) ?? day
            return (start, end)
        }

        static func quantityType(for metric: HealthMetric) -> HKQuantityType? {
            let identifier: HKQuantityTypeIdentifier?
            switch metric {
            case .steps: identifier = .stepCount
            case .restingHeartRate: identifier = .restingHeartRate
            case .heartRate: identifier = .heartRate
            case .activeEnergy: identifier = .activeEnergyBurned
            case .sleep: identifier = nil
            }
            return identifier.flatMap(HKQuantityType.quantityType(forIdentifier:))
        }

        static func hkUnit(for metric: HealthMetric) -> HKUnit {
            switch metric {
            case .steps: return .count()
            case .restingHeartRate, .heartRate: return HKUnit.count().unitDivided(by: .minute())
            case .activeEnergy: return .kilocalorie()
            case .sleep: return .minute()
            }
        }

        static func allReadTypes() -> [HKObjectType] {
            var types: [HKObjectType] = []
            for metric in HealthMetric.allCases where metric != .sleep {
                if let t = quantityType(for: metric) { types.append(t) }
            }
            if let sleep = HKCategoryType.categoryType(forIdentifier: .sleepAnalysis) {
                types.append(sleep)
            }
            return types
        }

        fileprivate func sumQuantity(metric: HealthMetric, predicate: NSPredicate) async -> Double? {
            guard let type = Self.quantityType(for: metric) else { return nil }
            let unit = Self.hkUnit(for: metric)
            return await withCheckedContinuation { continuation in
                let query = HKStatisticsQuery(
                    quantityType: type, quantitySamplePredicate: predicate,
                    options: .cumulativeSum
                ) { _, stats, _ in
                    continuation.resume(returning: stats?.sumQuantity()?.doubleValue(for: unit))
                }
                healthStore.execute(query)
            }
        }

        fileprivate func averageQuantity(metric: HealthMetric, predicate: NSPredicate) async -> Double?
        {
            guard let type = Self.quantityType(for: metric) else { return nil }
            let unit = Self.hkUnit(for: metric)
            return await withCheckedContinuation { continuation in
                let query = HKStatisticsQuery(
                    quantityType: type, quantitySamplePredicate: predicate,
                    options: .discreteAverage
                ) { _, stats, _ in
                    continuation.resume(
                        returning: stats?.averageQuantity()?.doubleValue(for: unit))
                }
                healthStore.execute(query)
            }
        }

        fileprivate func sleepAsleepMinutes(predicate: NSPredicate) async -> Double? {
            guard let type = HKCategoryType.categoryType(forIdentifier: .sleepAnalysis) else {
                return nil
            }
            return await withCheckedContinuation { continuation in
                let query = HKSampleQuery(
                    sampleType: type, predicate: predicate, limit: HKObjectQueryNoLimit,
                    sortDescriptors: nil
                ) { _, samples, _ in
                    guard let categorySamples = samples as? [HKCategorySample] else {
                        continuation.resume(returning: nil)
                        return
                    }
                    let asleepValues: Set<Int> = [
                        HKCategoryValueSleepAnalysis.asleepUnspecified.rawValue,
                        HKCategoryValueSleepAnalysis.asleepCore.rawValue,
                        HKCategoryValueSleepAnalysis.asleepDeep.rawValue,
                        HKCategoryValueSleepAnalysis.asleepREM.rawValue,
                    ]
                    let seconds = categorySamples
                        .filter { asleepValues.contains($0.value) }
                        .reduce(0.0) { $0 + $1.endDate.timeIntervalSince($1.startDate) }
                    continuation.resume(returning: seconds > 0 ? seconds / 60.0 : nil)
                }
                healthStore.execute(query)
            }
        }

        // TODO(background): HKObserverQuery + enableBackgroundDelivery for
        // passive daily sync is intentionally deferred — capture-on-demand is
        // the shipped path. Background delivery needs the background-mode
        // capability and a stable server contract, and is a separate slice.
    }
#endif
