// ReconnectBackoffTests.swift
// Pure unit tests for EdgeClient's reconnect backoff calculator.

import XCTest

@testable import PhiloticKit

final class ReconnectBackoffTests: XCTestCase {
    func testFirstAttemptUsesInitialDelay() {
        let backoff = ReconnectBackoff(initial: 0.5, multiplier: 2.0, cap: 30.0, jitter: 0)
        XCTAssertEqual(backoff.baseDelay(forAttempt: 0), 0.5)
    }

    func testDelayGrowsExponentially() {
        let backoff = ReconnectBackoff(initial: 1.0, multiplier: 2.0, cap: 1000.0, jitter: 0)
        XCTAssertEqual(backoff.baseDelay(forAttempt: 0), 1.0)
        XCTAssertEqual(backoff.baseDelay(forAttempt: 1), 2.0)
        XCTAssertEqual(backoff.baseDelay(forAttempt: 2), 4.0)
        XCTAssertEqual(backoff.baseDelay(forAttempt: 3), 8.0)
    }

    func testDelayIsCapped() {
        let backoff = ReconnectBackoff(initial: 1.0, multiplier: 2.0, cap: 5.0, jitter: 0)
        XCTAssertEqual(backoff.baseDelay(forAttempt: 10), 5.0)
    }

    func testJitterStaysWithinBounds() {
        let backoff = ReconnectBackoff(initial: 10.0, multiplier: 2.0, cap: 100.0, jitter: 0.2)
        let base = backoff.baseDelay(forAttempt: 2)
        let low = backoff.delay(forAttempt: 2, unitJitter: 0)
        let high = backoff.delay(forAttempt: 2, unitJitter: 0.999999)
        XCTAssertEqual(low, base * 0.8, accuracy: 0.0001)
        XCTAssertEqual(high, base * 1.2, accuracy: 0.001)
    }

    func testZeroJitterIsDeterministic() {
        let backoff = ReconnectBackoff(initial: 2.0, multiplier: 3.0, cap: 50.0, jitter: 0)
        XCTAssertEqual(backoff.delay(forAttempt: 1, unitJitter: 0.5), backoff.baseDelay(forAttempt: 1))
    }
}
