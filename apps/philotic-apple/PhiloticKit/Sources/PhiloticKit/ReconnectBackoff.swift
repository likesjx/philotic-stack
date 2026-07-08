// ReconnectBackoff.swift
// Pure exponential backoff calculator used by `EdgeClient`'s reconnect loop.
// Kept side-effect-free so it can be unit tested without any networking.

import Foundation

/// Exponential backoff with a cap and optional jitter, used to space out
/// `EdgeClient` reconnect attempts.
public struct ReconnectBackoff: Sendable, Equatable {
    /// Delay before the first retry.
    public var initial: TimeInterval
    /// Multiplier applied to the delay after each attempt.
    public var multiplier: Double
    /// Upper bound on the computed delay.
    public var cap: TimeInterval
    /// Fractional jitter applied to the delay (0 = none, 0.2 = +/-20%).
    public var jitter: Double

    public init(
        initial: TimeInterval = 0.5,
        multiplier: Double = 2.0,
        cap: TimeInterval = 30.0,
        jitter: Double = 0.2
    ) {
        self.initial = initial
        self.multiplier = multiplier
        self.cap = cap
        self.jitter = jitter
    }

    /// The base (pre-jitter) delay for a given zero-indexed attempt number.
    /// `attempt` 0 is the first reconnect try after the initial disconnect.
    public func baseDelay(forAttempt attempt: Int) -> TimeInterval {
        guard attempt > 0 else { return min(initial, cap) }
        let scaled = initial * pow(multiplier, Double(attempt))
        return min(scaled, cap)
    }

    /// The delay for a given attempt, with jitter applied via the supplied
    /// deterministic unit-interval sample (`unitJitter` in `[0, 1)`) so the
    /// result is reproducible in tests. Pass a random value in production.
    public func delay(forAttempt attempt: Int, unitJitter: Double = Double.random(in: 0..<1)) -> TimeInterval {
        let base = baseDelay(forAttempt: attempt)
        guard jitter > 0 else { return base }
        // Map unitJitter in [0, 1) to a factor in [1 - jitter, 1 + jitter).
        let factor = 1.0 - jitter + (unitJitter * 2.0 * jitter)
        return max(0, base * factor)
    }
}
