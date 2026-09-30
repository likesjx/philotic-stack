// Read-only foreground capture. Preview is local and ephemeral; only an
// explicit share of that exact preview may reach LifeGraph.
import Foundation
import Observation
import PhiloticKit

enum HealthMetric: String, CaseIterable, Identifiable, Sendable {
    case steps = "step_count"
    case restingHeartRate = "resting_heart_rate"
    case heartRate = "heart_rate"
    case sleep = "sleep_asleep_duration"
    case activeEnergy = "active_energy_burned"

    var id: String { rawValue }
    var displayName: String {
        switch self {
        case .steps: return "Steps"
        case .restingHeartRate: return "Resting Heart Rate"
        case .heartRate: return "Heart Rate (avg)"
        case .sleep: return "Sleep (asleep)"
        case .activeEnergy: return "Active Energy"
        }
    }
    var systemImage: String {
        switch self {
        case .steps: return "figure.walk"
        case .restingHeartRate: return "heart"
        case .heartRate: return "waveform.path.ecg"
        case .sleep: return "bed.double"
        case .activeEnergy: return "flame"
        }
    }
    var unit: String {
        switch self {
        case .steps: return "count"
        case .restingHeartRate, .heartRate: return "count/min"
        case .sleep: return "min"
        case .activeEnergy: return "kcal"
        }
    }
}

enum HealthReadWindow: Int, CaseIterable, Identifiable {
    case yesterday = 1
    case week = 7
    var id: Int { rawValue }
    var title: String { self == .yesterday ? "Yesterday" : "Last 7 completed days" }
}

/// nil means no readable samples, never proof of zero activity or denial.
@MainActor
protocol HealthDataReading {
    var isAvailable: Bool { get }
    func requestAuthorization(for metrics: Set<HealthMetric>) async throws
    func aggregate(_ metric: HealthMetric, interval: DateInterval) async throws -> Double?
}

enum HealthCaptureError: LocalizedError {
    case unavailable, authorizationFailed, invalidValue
    var errorDescription: String? {
        switch self {
        case .unavailable:
            return "Apple Health is unavailable on this device. No data was read or sent."
        case .authorizationFailed:
            return "The Apple Health permission request did not complete. Try again."
        case .invalidValue:
            return "Apple Health returned an invalid reading. Nothing was prepared for sharing."
        }
    }
}

struct HealthPreviewItem: Identifiable {
    let observation: LifeObservation
    var id: String { observation.observationId }
}

@MainActor
@Observable
final class HealthKitCaptureService {
    static let observedBy = "edge:ios-healthkit"
    // Do not migrate the old all-enabled default or persist health previews.
    var enabledMetrics: Set<HealthMetric> = [] {
        didSet { if oldValue != enabledMetrics { discardPreview() } }
    }
    var window: HealthReadWindow = .yesterday {
        didSet { if oldValue != window { discardPreview() } }
    }
    private(set) var isReading = false
    private(set) var isSharing = false
    private(set) var previewID: UUID?
    private(set) var previewItems: [HealthPreviewItem] = []
    private(set) var missingCount = 0
    private(set) var statusMessage: String?
    private(set) var lastError: String?
    var isAvailable: Bool { reader.isAvailable }
    var isBusy: Bool { isReading || isSharing }

    @ObservationIgnored private let reader: any HealthDataReading
    @ObservationIgnored private let post:
        ([LifeObservation], URL, String) async throws -> ObserveResult
    @ObservationIgnored private var revision = UUID()

    init(
        reader: (any HealthDataReading)? = nil,
        post: @escaping ([LifeObservation], URL, String) async throws -> ObserveResult = {
            try await LifeGraphClient().postObservations($0, baseURL: $1, bearerToken: $2)
        }
    ) {
        self.reader = reader ?? DeviceHealthReader()
        self.post = post
    }

    func discardPreview() {
        revision = UUID()
        previewID = nil
        previewItems = []
        missingCount = 0
        statusMessage = nil
        lastError = nil
    }

    /// Authorization completion does NOT indicate read permission. HealthKit
    /// makes denial indistinguishable from an empty store. No upload or
    /// app-owned disk writes occur while preparing a preview.
    func preparePreview(now: Date = Date(), calendar: Calendar = .current) async {
        guard !isBusy else { return }
        discardPreview()
        guard isAvailable else {
            lastError = HealthCaptureError.unavailable.localizedDescription
            return
        }
        guard !enabledMetrics.isEmpty else {
            statusMessage = "Choose at least one metric to preview."
            return
        }
        isReading = true
        defer { isReading = false }
        let requestRevision = revision
        let metrics = enabledMetrics
        let intervals = Self.completedDays(before: now, count: window.rawValue, calendar: calendar)
        do {
            try await reader.requestAuthorization(for: metrics)
            var items: [HealthPreviewItem] = []
            var missing = 0
            for interval in intervals {
                for metric in HealthMetric.allCases where metrics.contains(metric) {
                    try Task.checkCancellation()
                    guard revision == requestRevision else { return }
                    guard let value = try await reader.aggregate(metric, interval: interval) else {
                        missing += 1
                        continue
                    }
                    guard value.isFinite, value >= 0 else { throw HealthCaptureError.invalidValue }
                    items.append(
                        HealthPreviewItem(
                            observation: Self.observation(
                                metric: metric, value: value, interval: interval,
                                capturedAt: now, timeZone: calendar.timeZone)))
                }
            }
            try Task.checkCancellation()
            guard revision == requestRevision else { return }
            previewItems = items
            previewID = items.isEmpty ? nil : UUID()
            missingCount = missing
            statusMessage =
                items.isEmpty
                ? "No readable data. The selected days may be empty or access may be off in Apple Health."
                : "Preview ready. Nothing has left this device."
        } catch is CancellationError {
            if revision == requestRevision { discardPreview() }
        } catch {
            if revision == requestRevision {
                // Do not echo health data or identifiers from framework errors.
                lastError =
                    "Could not read Apple Health. Check access in Health and try again. Nothing was sent."
            }
        }
    }

    /// UI confirmation carries preview identity and destination. A preview is
    /// single-attempt: partial/unknown writes are never automatically retried.
    func sharePreview(id: UUID, baseURL: URL, bearerToken: String) async {
        guard !isBusy, previewID == id, !previewItems.isEmpty else { return }
        guard !bearerToken.isEmpty else { return }
        let observations = previewItems.map(\.observation)
        let requestRevision = revision
        isSharing = true
        lastError = nil
        statusMessage = nil
        previewID = nil
        defer { isSharing = false }
        do {
            let result = try await post(observations, baseURL, bearerToken)
            guard revision == requestRevision else { return }
            let acknowledged = Set(
                result.results.compactMap { item -> String? in
                    guard item.status == "proposed" else { return nil }
                    return item.observationId
                })
            if result.status == "ok", result.results.count == observations.count,
                result.results.allSatisfy({ $0.status == "proposed" }),
                acknowledged == Set(observations.map(\.observationId))
            {
                statusMessage =
                    "Server acknowledged \(observations.count) health summaries. Agent recall is not yet verified."
            } else {
                lastError =
                    "The server did not confirm the full batch. Some summaries may be stored. Check LifeGraph before sending again."
            }
        } catch {
            guard revision == requestRevision else { return }
            lastError =
                "Sharing was not confirmed. Some summaries may have arrived; check LifeGraph before sending again."
        }
    }

    static func completedDays(before now: Date, count: Int, calendar: Calendar) -> [DateInterval] {
        let today = calendar.startOfDay(for: now)
        return (1...min(7, max(1, count))).compactMap { offset in
            guard let day = calendar.date(byAdding: .day, value: -offset, to: today) else {
                return nil
            }
            return calendar.dateInterval(of: .day, for: day)
        }
    }

    /// Union clipped asleep intervals: crossing midnight and duplicate sources
    /// must not lose sleep or double-count overlapping stages/samples.
    nonisolated static func asleepMinutes(_ intervals: [DateInterval], within day: DateInterval)
        -> Double?
    {
        let clipped = intervals.compactMap { interval -> DateInterval? in
            let start = max(interval.start, day.start)
            let end = min(interval.end, day.end)
            return end > start ? DateInterval(start: start, end: end) : nil
        }.sorted { $0.start < $1.start }
        guard var current = clipped.first else { return nil }
        var seconds = 0.0
        for interval in clipped.dropFirst() {
            if interval.start <= current.end {
                current = DateInterval(start: current.start, end: max(current.end, interval.end))
            } else {
                seconds += current.duration
                current = interval
            }
        }
        return (seconds + current.duration) / 60
    }

    static func observation(
        metric: HealthMetric, value: Double, interval: DateInterval,
        capturedAt: Date, timeZone: TimeZone
    ) -> LifeObservation {
        let timestamp = ISO8601DateFormatter()
        let start = timestamp.string(from: interval.start)
        let end = timestamp.string(from: interval.end)
        let captured = timestamp.string(from: capturedAt)
        let valueText = String(format: "%.1f", locale: Locale(identifier: "en_US_POSIX"), value)
        // Unique claim per snapshot: life.observe does not update existing
        // claim summaries. Keep facts in text until server metadata persists.
        let summary =
            "\(metric.displayName): \(valueText) \(metric.unit); "
            + "completed local day [\(start), \(end)) (\(timeZone.identifier)); "
            + "read from Apple Health at \(captured). Available samples only, not a clinical assessment."
        return LifeObservation(
            observationId: "obs-health-\(UUID().uuidString)",
            evidence: EvidencePacket(
                packetId: "pkt-\(UUID().uuidString)",
                claimRef: GraphRecordRef(
                    id: "signal:health-snapshot:\(UUID().uuidString)", label: "Signal",
                    datasource: "memgraph"
                ),
                claimSummary: summary,
                sourceRefs: [
                    SourceRef(
                        sourceId: observedBy, sourceKind: "imported_record",
                        reliability: Reliability(score: 0.9, basis: "imported_authority"))
                ],
                confidence: 0.9, validationState: "proposed", observedAt: captured,
                sourceReliability: 0.9, adjudicationStatus: "not_needed",
                metadata: [
                    "metric": .string(metric.rawValue), "unit": .string(metric.unit),
                    "value": .number(value),
                    "period_start": .string(start), "period_end": .string(end),
                    "time_zone": .string(timeZone.identifier), "snapshot_only": .bool(true),
                ]),
            observedBy: observedBy, observedRole: "sensor")
    }
}
