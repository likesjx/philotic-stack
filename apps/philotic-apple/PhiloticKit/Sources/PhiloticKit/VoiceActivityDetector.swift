// VoiceActivityDetector.swift
// Pure energy-based VAD state machine for hands-free conversation mode.
// Consumes fixed-duration PCM s16le frames (the app feeds it the same
// ~250ms frames it streams to the hotel) and emits utterance segmentation
// events. Kept in PhiloticKit — free of AVFoundation — so it is unit
// testable with synthetic frames.

import Foundation

/// Tunable thresholds for ``VoiceActivityDetector``. All defaults are
/// starting points expected to be tuned live.
public struct VADConfig: Equatable, Sendable {
    /// Duration of each frame fed to `process` (the app uses 250ms frames).
    public var frameDuration: TimeInterval
    /// Speech = RMS above rolling-noise-floor × this multiplier.
    public var noiseFloorMultiplier: Double
    /// Absolute minimum speech RMS (normalized 0…1, where 1.0 = full-scale
    /// Int16). Guards near-silent rooms where floor × multiplier ≈ 0.
    /// 0.006 ≈ -44 dBFS — deliberately permissive: Apple voice processing
    /// attenuates input noticeably, and a missed onset is worse than an
    /// occasional false one (short utterances are discarded anyway).
    public var minSpeechRMS: Double
    /// EMA weight for adapting the noise floor on idle non-speech frames.
    public var noiseFloorAlpha: Double
    /// Audio retained from before speech onset so first words aren't clipped.
    public var preRoll: TimeInterval
    /// Trailing silence that closes an utterance.
    public var trailingSilence: TimeInterval
    /// Minimum accumulated speech for a valid utterance (rejects coughs).
    public var minUtteranceSpeech: TimeInterval
    /// Stricter onset requirement used while agent audio is playing
    /// (barge-in): speech must be sustained this long to resist echo leakage.
    public var sustainedOnset: TimeInterval

    public init(
        frameDuration: TimeInterval = 0.25,
        noiseFloorMultiplier: Double = 2.5,
        minSpeechRMS: Double = 0.006,
        noiseFloorAlpha: Double = 0.05,
        preRoll: TimeInterval = 0.5,
        trailingSilence: TimeInterval = 0.9,
        minUtteranceSpeech: TimeInterval = 0.4,
        sustainedOnset: TimeInterval = 0.3
    ) {
        self.frameDuration = frameDuration
        self.noiseFloorMultiplier = noiseFloorMultiplier
        self.minSpeechRMS = minSpeechRMS
        self.noiseFloorAlpha = noiseFloorAlpha
        self.preRoll = preRoll
        self.trailingSilence = trailingSilence
        self.minUtteranceSpeech = minUtteranceSpeech
        self.sustainedOnset = sustainedOnset
    }

    // Frame-quantized thresholds (rounded up, minimum 1 frame).

    var preRollFrames: Int { frames(for: preRoll) }
    var trailingSilenceFrames: Int { frames(for: trailingSilence) }
    var minUtteranceSpeechFrames: Int { frames(for: minUtteranceSpeech) }
    var sustainedOnsetFrames: Int { frames(for: sustainedOnset) }

    private func frames(for interval: TimeInterval) -> Int {
        max(1, Int((interval / frameDuration).rounded(.up)))
    }
}

/// Segmentation event emitted by ``VoiceActivityDetector/process(frame:requireSustainedOnset:)``.
public enum VADEvent: Equatable, Sendable {
    /// Speech onset. `preRollFrames` is everything buffered before/at onset
    /// (pre-roll ring + onset frame(s), oldest first) — send these first.
    case utteranceStarted(preRollFrames: [Data])
    /// A frame inside an open utterance (send it; trailing-silence frames
    /// are included so the server hears a natural tail).
    case utteranceContinued(frame: Data)
    /// Trailing silence closed the utterance. `validUtterance` is false when
    /// accumulated speech was too short (a cough/blip) — discard the stream
    /// (`audio_stream_end(cancel: true)`) instead of submitting a turn.
    case utteranceEnded(validUtterance: Bool)
}

/// Energy (RMS) voice-activity detector with an adaptive noise floor.
/// Feed every capture frame in order; between utterances frames only feed
/// the pre-roll ring and the noise floor.
public struct VoiceActivityDetector: Sendable {
    public let config: VADConfig

    private var noiseFloor: Double = 0
    /// Idle pre-roll ring (most recent `preRollFrames` idle frames).
    private var ring: [Data] = []
    /// Consecutive speech frames counting toward a sustained onset.
    private var pendingOnset: [Data] = []
    private var inUtterance = false
    private var silenceRun = 0
    private var speechFrameCount = 0

    public init(config: VADConfig = VADConfig()) {
        self.config = config
    }

    /// Current speech threshold (adaptive floor × multiplier, clamped to
    /// the absolute minimum).
    public var speechThreshold: Double {
        max(noiseFloor * config.noiseFloorMultiplier, config.minSpeechRMS)
    }

    /// Processes one PCM s16le frame. `requireSustainedOnset` selects the
    /// stricter barge-in onset (use while agent audio is playing).
    public mutating func process(frame: Data, requireSustainedOnset: Bool = false) -> [VADEvent] {
        let rms = Self.rms(ofPCMS16LE: frame)
        let isSpeech = rms >= speechThreshold

        if inUtterance {
            var events: [VADEvent] = [.utteranceContinued(frame: frame)]
            if isSpeech {
                silenceRun = 0
                speechFrameCount += 1
            } else {
                silenceRun += 1
                if silenceRun >= config.trailingSilenceFrames {
                    let valid = speechFrameCount >= config.minUtteranceSpeechFrames
                    inUtterance = false
                    silenceRun = 0
                    speechFrameCount = 0
                    events.append(.utteranceEnded(validUtterance: valid))
                }
            }
            return events
        }

        // Idle.
        if isSpeech {
            pendingOnset.append(frame)
            let needed = requireSustainedOnset ? config.sustainedOnsetFrames : 1
            guard pendingOnset.count >= needed else { return [] }
            let preRoll = ring + pendingOnset
            ring.removeAll()
            speechFrameCount = pendingOnset.count
            pendingOnset.removeAll()
            inUtterance = true
            silenceRun = 0
            return [.utteranceStarted(preRollFrames: preRoll)]
        }

        // Idle non-speech: an unsustained onset candidate was a blip — spill
        // it into the ring so a real onset moments later still carries it.
        if !pendingOnset.isEmpty {
            ring.append(contentsOf: pendingOnset)
            pendingOnset.removeAll()
        }
        ring.append(frame)
        while ring.count > config.preRollFrames {
            ring.removeFirst()
        }
        // Adapt the noise floor only on idle non-speech frames, so speech
        // never poisons it.
        if noiseFloor == 0 {
            noiseFloor = rms
        } else {
            noiseFloor = (1 - config.noiseFloorAlpha) * noiseFloor + config.noiseFloorAlpha * rms
        }
        return []
    }

    /// RMS of little-endian signed 16-bit PCM, normalized to 0…1.
    public static func rms(ofPCMS16LE data: Data) -> Double {
        let sampleCount = data.count / MemoryLayout<Int16>.size
        guard sampleCount > 0 else { return 0 }
        var sum: Double = 0
        data.withUnsafeBytes { raw in
            for sample in raw.bindMemory(to: Int16.self) {
                let normalized = Double(Int16(littleEndian: sample)) / 32768.0
                sum += normalized * normalized
            }
        }
        return (sum / Double(sampleCount)).squareRoot()
    }
}
