import Foundation

#if os(iOS)
    import HealthKit
#endif

/// Only HealthKit boundary. Never writes samples, generates demo values,
/// advertises a remote tool, or registers background observers.
@MainActor
final class DeviceHealthReader: HealthDataReading {
    var isAvailable: Bool {
        #if os(iOS)
            HKHealthStore.isHealthDataAvailable()
        #else
            false
        #endif
    }

    #if os(iOS)
        private let store = HKHealthStore()

        static func readTypes(for metrics: Set<HealthMetric>) -> Set<HKObjectType> {
            Set(
                metrics.compactMap { metric -> HKObjectType? in
                    if metric == .sleep {
                        return HKCategoryType.categoryType(forIdentifier: .sleepAnalysis)
                    }
                    return quantityType(for: metric)
                })
        }

        private static func quantityType(for metric: HealthMetric) -> HKQuantityType? {
            switch metric {
            case .steps: return HKQuantityType.quantityType(forIdentifier: .stepCount)
            case .restingHeartRate:
                return HKQuantityType.quantityType(forIdentifier: .restingHeartRate)
            case .heartRate: return HKQuantityType.quantityType(forIdentifier: .heartRate)
            case .activeEnergy:
                return HKQuantityType.quantityType(forIdentifier: .activeEnergyBurned)
            case .sleep: return nil
            }
        }
    #endif

    func requestAuthorization(for metrics: Set<HealthMetric>) async throws {
        guard isAvailable else { throw HealthCaptureError.unavailable }
        #if os(iOS)
            let types = Self.readTypes(for: metrics)
            guard !types.isEmpty else { return }
            try await withCheckedThrowingContinuation {
                (continuation: CheckedContinuation<Void, Error>) in
                store.requestAuthorization(toShare: [], read: types) { completed, error in
                    if let error {
                        continuation.resume(throwing: error)
                    } else if completed {
                        continuation.resume()
                    } else {
                        continuation.resume(throwing: HealthCaptureError.authorizationFailed)
                    }
                }
            }
        #endif
    }

    func aggregate(_ metric: HealthMetric, interval: DateInterval) async throws -> Double? {
        guard isAvailable else { throw HealthCaptureError.unavailable }
        #if os(iOS)
            if metric == .sleep { return try await sleepMinutes(interval: interval) }
            guard let type = Self.quantityType(for: metric) else { return nil }
            let isCumulative = metric == .steps || metric == .activeEnergy
            let unit: HKUnit =
                metric == .steps
                ? .count()
                : metric == .activeEnergy
                    ? .kilocalorie() : HKUnit.count().unitDivided(by: .minute())
            let predicate = HKQuery.predicateForSamples(
                withStart: interval.start, end: interval.end,
                options: [.strictStartDate, .strictEndDate])
            return try await withCheckedThrowingContinuation { continuation in
                let query = HKStatisticsQuery(
                    quantityType: type, quantitySamplePredicate: predicate,
                    options: isCumulative ? .cumulativeSum : .discreteAverage
                ) { _, statistics, error in
                    if let error {
                        continuation.resume(throwing: error)
                        return
                    }
                    let quantity =
                        isCumulative ? statistics?.sumQuantity() : statistics?.averageQuantity()
                    continuation.resume(returning: quantity?.doubleValue(for: unit))
                }
                store.execute(query)
            }
        #else
            throw HealthCaptureError.unavailable
        #endif
    }

    #if os(iOS)
        private func sleepMinutes(interval: DateInterval) async throws -> Double? {
            guard let type = HKCategoryType.categoryType(forIdentifier: .sleepAnalysis) else {
                return nil
            }
            // Overlap predicate includes samples that began on the preceding day.
            let predicate = HKQuery.predicateForSamples(
                withStart: interval.start, end: interval.end, options: [])
            return try await withCheckedThrowingContinuation { continuation in
                let query = HKSampleQuery(
                    sampleType: type, predicate: predicate,
                    limit: HKObjectQueryNoLimit, sortDescriptors: nil
                ) { _, samples, error in
                    if let error {
                        continuation.resume(throwing: error)
                        return
                    }
                    let asleep: Set<Int> = [
                        HKCategoryValueSleepAnalysis.asleepUnspecified.rawValue,
                        HKCategoryValueSleepAnalysis.asleepCore.rawValue,
                        HKCategoryValueSleepAnalysis.asleepDeep.rawValue,
                        HKCategoryValueSleepAnalysis.asleepREM.rawValue,
                    ]
                    let intervals = (samples as? [HKCategorySample] ?? []).compactMap {
                        sample -> DateInterval? in
                        guard asleep.contains(sample.value), sample.endDate > sample.startDate
                        else {
                            return nil
                        }
                        return DateInterval(start: sample.startDate, end: sample.endDate)
                    }
                    continuation.resume(
                        returning: HealthKitCaptureService.asleepMinutes(
                            intervals, within: interval))
                }
                store.execute(query)
            }
        }
    #endif
}
