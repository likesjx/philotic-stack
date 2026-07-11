// VoiceActivityDetectorTests.swift
// Segmentation tests over synthetic PCM s16le frames: onset with pre-roll,
// trailing-silence end, short-blip rejection, and the stricter sustained
// (barge-in) onset.

import XCTest

@testable import PhiloticKit

final class VoiceActivityDetectorTests: XCTestCase {
    /// A synthetic 250ms-equivalent frame of constant-amplitude samples.
    /// RMS of constant amplitude A is exactly A/32768 — so `amplitude: 0`
    /// is silence and `amplitude: 8000` (~0.24) is unambiguous speech
    /// against the default 0.015 minimum threshold.
    private func frame(amplitude: Int16, samples: Int = 4000) -> Data {
        var data = Data(capacity: samples * 2)
        withUnsafeBytes(of: amplitude.littleEndian) { bytes in
            for _ in 0..<samples {
                data.append(contentsOf: bytes)
            }
        }
        return data
    }

    private var loud: Data { frame(amplitude: 8000) }

    func testRMSOfConstantAmplitude() {
        XCTAssertEqual(VoiceActivityDetector.rms(ofPCMS16LE: frame(amplitude: 0)), 0)
        XCTAssertEqual(
            VoiceActivityDetector.rms(ofPCMS16LE: frame(amplitude: 8000)),
            8000.0 / 32768.0,
            accuracy: 1e-9
        )
        XCTAssertEqual(VoiceActivityDetector.rms(ofPCMS16LE: Data()), 0)
    }

    func testDefaultConfigFrameQuantization() {
        let config = VADConfig()
        XCTAssertEqual(config.preRollFrames, 2)  // 0.5s / 0.25s
        XCTAssertEqual(config.trailingSilenceFrames, 4)  // ceil(0.9 / 0.25)
        XCTAssertEqual(config.minUtteranceSpeechFrames, 2)  // ceil(0.4 / 0.25)
        XCTAssertEqual(config.sustainedOnsetFrames, 2)  // ceil(0.3 / 0.25)
    }

    func testOnsetIncludesPreRoll() {
        var vad = VoiceActivityDetector()
        // Distinguishable quiet frames (all well below the 0.015 floor).
        let quiet1 = frame(amplitude: 1)
        let quiet2 = frame(amplitude: 2)
        let quiet3 = frame(amplitude: 3)

        XCTAssertEqual(vad.process(frame: quiet1), [])
        XCTAssertEqual(vad.process(frame: quiet2), [])
        XCTAssertEqual(vad.process(frame: quiet3), [])

        let events = vad.process(frame: loud)
        // Pre-roll ring holds the last 2 idle frames; onset frame rides last.
        XCTAssertEqual(events, [.utteranceStarted(preRollFrames: [quiet2, quiet3, loud])])
    }

    func testFramesInsideUtteranceAreContinued() {
        var vad = VoiceActivityDetector()
        _ = vad.process(frame: loud)  // onset
        XCTAssertEqual(vad.process(frame: loud), [.utteranceContinued(frame: loud)])
        // Brief silence inside the utterance is still forwarded.
        let quiet = frame(amplitude: 0)
        XCTAssertEqual(vad.process(frame: quiet), [.utteranceContinued(frame: quiet)])
        XCTAssertEqual(vad.process(frame: loud), [.utteranceContinued(frame: loud)])
    }

    func testTrailingSilenceEndsValidUtterance() {
        var vad = VoiceActivityDetector()
        let quiet = frame(amplitude: 0)
        _ = vad.process(frame: loud)  // onset (1 speech frame)
        _ = vad.process(frame: loud)  // 2 speech frames >= minUtteranceSpeechFrames

        // 4 trailing-silence frames close the utterance (still forwarded).
        for _ in 0..<3 {
            XCTAssertEqual(vad.process(frame: quiet), [.utteranceContinued(frame: quiet)])
        }
        XCTAssertEqual(
            vad.process(frame: quiet),
            [.utteranceContinued(frame: quiet), .utteranceEnded(validUtterance: true)]
        )
    }

    func testShortBlipIsRejected() {
        var vad = VoiceActivityDetector()
        let quiet = frame(amplitude: 0)
        _ = vad.process(frame: loud)  // a single 250ms cough (< 400ms minimum)
        for _ in 0..<3 {
            _ = vad.process(frame: quiet)
        }
        let events = vad.process(frame: quiet)
        XCTAssertEqual(events.last, .utteranceEnded(validUtterance: false))
    }

    func testSilenceInsideUtteranceDoesNotEndItEarly() {
        var vad = VoiceActivityDetector()
        let quiet = frame(amplitude: 0)
        _ = vad.process(frame: loud)
        // 3 silent frames (< 4-frame trailing threshold), then speech resumes.
        for _ in 0..<3 {
            _ = vad.process(frame: quiet)
        }
        XCTAssertEqual(vad.process(frame: loud), [.utteranceContinued(frame: loud)])
        // Silence run must have reset: 3 more silent frames still don't end it.
        for _ in 0..<3 {
            XCTAssertEqual(vad.process(frame: quiet), [.utteranceContinued(frame: quiet)])
        }
    }

    func testSustainedOnsetRequiresConsecutiveSpeechFrames() {
        var vad = VoiceActivityDetector()
        let quiet = frame(amplitude: 0)

        // One loud frame is not enough under the barge-in rule…
        XCTAssertEqual(vad.process(frame: loud, requireSustainedOnset: true), [])
        // …and an interrupting silent frame resets the candidate.
        XCTAssertEqual(vad.process(frame: quiet, requireSustainedOnset: true), [])
        XCTAssertEqual(vad.process(frame: loud, requireSustainedOnset: true), [])

        // Two consecutive loud frames trigger the onset; the earlier blip
        // spilled into the pre-roll ring so its audio is not lost.
        let events = vad.process(frame: loud, requireSustainedOnset: true)
        guard case .utteranceStarted(let preRoll)? = events.first else {
            return XCTFail("expected utteranceStarted, got \(events)")
        }
        XCTAssertEqual(preRoll.suffix(2), [loud, loud])
        XCTAssertTrue(preRoll.count >= 3, "spilled blip + onset frames expected in pre-roll")
    }

    func testAdaptiveThresholdRisesWithNoiseFloor() {
        // Pin the config explicitly: this test exercises the adaptive
        // mechanism, and must not silently change meaning when the
        // (live-tuned) default floor moves.
        let config = VADConfig(noiseFloorMultiplier: 3.0, minSpeechRMS: 0.015)
        var vad = VoiceActivityDetector(config: config)
        // Silent room: the absolute minimum applies.
        XCTAssertEqual(vad.speechThreshold, config.minSpeechRMS)

        // Ambient noise just below the threshold (RMS ≈ 0.0122) seeds the
        // floor; 3x floor (≈ 0.0366) then exceeds the absolute minimum, so
        // the same ambient level can never trigger an onset.
        let ambient = frame(amplitude: 400)
        XCTAssertEqual(vad.process(frame: ambient), [])
        XCTAssertGreaterThan(vad.speechThreshold, config.minSpeechRMS)
        XCTAssertEqual(vad.process(frame: ambient), [], "ambient noise must stay sub-threshold")
    }
}
