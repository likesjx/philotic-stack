// VoiceController.swift
// Push-to-talk voice capture for the chat surface: raw audio recording
// (default — uploaded for hotel-side transcription), on-device dictation
// (optional), inline voice-reply playback, and text-to-speech fallback.
// macOS + iOS compatible: AVAudioSession is iOS-only, so session
// configuration is guarded accordingly. Speech/AVFoundation delegate and
// completion callbacks land on arbitrary threads, so every callback hops
// back to `@MainActor` before touching observable state.

import AVFoundation
import Foundation
import OSLog
import Observation
import Speech

/// Owns speech-to-text capture, inline voice-reply playback, and
/// text-to-speech fallback. A single instance is expected to live for the
/// lifetime of the chat session.
@MainActor
@Observable
public final class VoiceController: NSObject {
    static let pcmLog = Logger(subsystem: "com.philotic.apple", category: "pcm")
    /// Live partial transcription while ``isListening``; holds the final
    /// transcript momentarily after ``stopListening()`` before being reset.
    public private(set) var transcript: String = ""
    /// True while actively capturing microphone audio for dictation.
    public private(set) var isListening: Bool = false
    /// True while capturing raw audio to a container via ``startRecording()``
    /// (HTTP-upload fallback path — batch transcription wants a container).
    public private(set) var isRecording: Bool = false
    /// True while capturing raw PCM via ``startPCMStreaming()`` (default WS
    /// streaming mic mode — realtime STT consumes raw PCM, not AAC).
    public private(set) var isCapturingPCM: Bool = false
    /// True while a voice reply or fallback utterance is audible.
    public private(set) var isPlaying: Bool = false
    /// Last voice-related failure, suitable for display in the UI. Cleared
    /// at the start of the next ``startListening()`` attempt.
    public var voiceError: String?

    private let speechRecognizer: SFSpeechRecognizer?
    private var audioEngine: AVAudioEngine?
    private var recognitionRequest: SFSpeechAudioBufferRecognitionRequest?
    private var recognitionTask: SFSpeechRecognitionTask?

    private var audioRecorder: AVAudioRecorder?
    private var recordingURL: URL?

    private var pcmEngine: AVAudioEngine?
    private var pcmContinuation: AsyncStream<Data>.Continuation?
    private var pcmAccumulator: PCMFrameAccumulator?

    private var audioPlayer: AVAudioPlayer?
    private let speechSynthesizer = AVSpeechSynthesizer()

    /// MIME type of files produced by ``startRecording()`` (.m4a / AAC).
    public static let recordingMimeType = "audio/mp4"
    /// MIME type of frames produced by ``startPCMStreaming()`` — raw
    /// little-endian signed 16-bit PCM, 16 kHz, mono, no container.
    public static let pcmStreamMimeType = "audio/pcm;rate=16000;channels=1"

    public override init() {
        self.speechRecognizer = SFSpeechRecognizer(locale: Locale.current) ?? SFSpeechRecognizer()
        super.init()
    }

    // MARK: - Speech-to-text

    /// Requests authorization (speech + microphone) if needed, then starts
    /// streaming on-device dictation into ``transcript``. On authorization
    /// denial or engine failure, sets ``voiceError`` and leaves
    /// ``isListening`` false.
    public func startListening() async {
        guard !isListening else { return }
        voiceError = nil
        transcript = ""

        guard let speechRecognizer, speechRecognizer.isAvailable else {
            voiceError = "Speech recognition is not available on this device right now."
            return
        }

        guard await Self.requestSpeechAuthorization() else {
            voiceError = "Speech recognition permission was denied. Enable it in Settings to send voice messages."
            return
        }
        guard await Self.requestMicrophoneAuthorization() else {
            voiceError = "Microphone permission was denied. Enable it in Settings to send voice messages."
            return
        }

        #if os(iOS)
        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.record, mode: .measurement, options: .duckOthers)
            try session.setActive(true, options: .notifyOthersOnDeactivation)
        } catch {
            voiceError = "Could not configure the audio session: \(error.localizedDescription)"
            return
        }
        #endif

        let engine = AVAudioEngine()
        let request = SFSpeechAudioBufferRecognitionRequest()
        request.shouldReportPartialResults = true
        // Prefer on-device recognition when the locale supports it (privacy
        // + works offline); fall back to the default (server-assisted)
        // recognizer otherwise rather than failing outright.
        if speechRecognizer.supportsOnDeviceRecognition {
            request.requiresOnDeviceRecognition = true
        }

        let inputNode = engine.inputNode
        let recordingFormat = inputNode.outputFormat(forBus: 0)
        inputNode.installTap(onBus: 0, bufferSize: 1024, format: recordingFormat) { buffer, _ in
            request.append(buffer)
        }

        engine.prepare()
        do {
            try engine.start()
        } catch {
            inputNode.removeTap(onBus: 0)
            voiceError = "Could not start the audio engine: \(error.localizedDescription)"
            return
        }

        audioEngine = engine
        recognitionRequest = request
        isListening = true

        recognitionTask = speechRecognizer.recognitionTask(with: request) { [weak self] result, error in
            // Extract Sendable primitives here, on whatever thread this
            // fires on, then hop to MainActor before touching self.
            let text = result?.bestTranscription.formattedString
            let isFinal = result?.isFinal ?? false
            let errorDescription = error?.localizedDescription
            Task { @MainActor [weak self] in
                self?.handleRecognitionUpdate(text: text, isFinal: isFinal, errorDescription: errorDescription)
            }
        }
    }

    private func handleRecognitionUpdate(text: String?, isFinal: Bool, errorDescription: String?) {
        guard isListening else { return }
        if let text {
            transcript = text
        }
        if isFinal {
            teardownAudioCapture()
        } else if let errorDescription {
            // A non-final error usually means the task ended abnormally
            // (e.g. no audio input); surface it and tear down.
            voiceError = errorDescription
            teardownAudioCapture()
        }
    }

    /// Ends capture and returns the transcript captured so far (best-effort;
    /// does not block for the recognizer's final result).
    @discardableResult
    public func stopListening() -> String {
        guard isListening else { return transcript }
        recognitionRequest?.endAudio()
        teardownAudioCapture()
        let finalText = transcript
        transcript = ""
        return finalText
    }

    private func teardownAudioCapture() {
        audioEngine?.stop()
        audioEngine?.inputNode.removeTap(onBus: 0)
        audioEngine = nil
        recognitionTask?.cancel()
        recognitionTask = nil
        recognitionRequest = nil
        isListening = false
        #if os(iOS)
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        #endif
    }

    private static func requestSpeechAuthorization() async -> Bool {
        await withCheckedContinuation { continuation in
            SFSpeechRecognizer.requestAuthorization { status in
                continuation.resume(returning: status == .authorized)
            }
        }
    }

    private static func requestMicrophoneAuthorization() async -> Bool {
        await withCheckedContinuation { continuation in
            AVCaptureDevice.requestAccess(for: .audio) { granted in
                continuation.resume(returning: granted)
            }
        }
    }

    // MARK: - Raw audio recording (default mic mode)

    /// Requests microphone authorization if needed, then starts recording
    /// raw audio to a temporary `.m4a` (AAC, 44.1 kHz, mono, ~64 kbps) for
    /// upload to the hotel's transcription pipeline. On denial or recorder
    /// failure, sets ``voiceError`` and leaves ``isRecording`` false.
    public func startRecording() async {
        guard !isRecording, !isListening else { return }
        voiceError = nil

        guard await Self.requestMicrophoneAuthorization() else {
            voiceError = "Microphone permission was denied. Enable it in Settings to send voice messages."
            return
        }

        #if os(iOS)
        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.record, mode: .default, options: .duckOthers)
            try session.setActive(true, options: .notifyOthersOnDeactivation)
        } catch {
            voiceError = "Could not configure the audio session: \(error.localizedDescription)"
            return
        }
        #endif

        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("philotic-voice-\(UUID().uuidString)")
            .appendingPathExtension("m4a")

        let settings: [String: Any] = [
            AVFormatIDKey: Int(kAudioFormatMPEG4AAC),
            AVSampleRateKey: 44_100.0,
            AVNumberOfChannelsKey: 1,
            AVEncoderBitRateKey: 64_000,
        ]

        do {
            let recorder = try AVAudioRecorder(url: url, settings: settings)
            guard recorder.record() else {
                voiceError = "Could not start recording."
                return
            }
            audioRecorder = recorder
            recordingURL = url
            isRecording = true
        } catch {
            voiceError = "Could not start recording: \(error.localizedDescription)"
        }
    }

    /// Stops recording and returns the temp file URL of the captured audio,
    /// or nil if nothing was being recorded. The caller owns the file and
    /// should delete it after upload.
    public func stopRecording() -> URL? {
        guard isRecording else { return nil }
        audioRecorder?.stop()
        audioRecorder = nil
        isRecording = false
        let url = recordingURL
        recordingURL = nil
        #if os(iOS)
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        #endif
        return url
    }

    /// Aborts an in-progress recording, discarding the temp file.
    public func cancelRecording() {
        guard let url = stopRecording() else { return }
        try? FileManager.default.removeItem(at: url)
    }

    // NOTE: the previous m4a "tail-follower" streaming capture
    // (startStreamingRecording / stopStreamingRecording) was retired when
    // the WS streaming path moved to raw PCM (realtime STT consumes PCM,
    // not AAC containers). The HTTP-upload fallback uses the plain
    // startRecording()/stopRecording() pair above.

    // MARK: - PCM streaming capture (realtime STT)

    /// Starts a raw-PCM capture and returns an `AsyncStream` of ~250ms
    /// frames of 16 kHz mono s16le audio (``pcmStreamMimeType``), suitable
    /// for the server's realtime STT pipeline. The tap converts each engine
    /// buffer via `AVAudioConverter` and accumulates bytes into fixed-size
    /// frames; ``stopPCMStreaming()`` flushes the remainder and finishes
    /// the stream.
    public func startPCMStreaming() async -> AsyncStream<Data>? {
        guard !isCapturingPCM, !isRecording, !isListening else { return nil }
        voiceError = nil

        guard await Self.requestMicrophoneAuthorization() else {
            voiceError = "Microphone permission was denied. Enable it in Settings to send voice messages."
            return nil
        }

        #if os(iOS)
        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.record, mode: .measurement, options: .duckOthers)
            try session.setActive(true, options: .notifyOthersOnDeactivation)
        } catch {
            voiceError = "Could not configure the audio session: \(error.localizedDescription)"
            return nil
        }
        #endif

        let engine = AVAudioEngine()
        let inputNode = engine.inputNode

        // Echo cancellation: Apple's voice processing (AEC/noise suppression)
        // so the agent's own audio doesn't transcribe itself in conversation
        // mode. MUST happen before the format is read and the tap installed —
        // enabling voice processing changes the node's I/O format.
        // Voice processing (AEC) is DISABLED: on macOS, enabling it on an
        // input-only engine (no configured output node) zeroes the captured
        // audio entirely — every frame arrives as digital silence (observed
        // live 2026-07-11, RMS 0.00000 on all frames). Echo protection is
        // the VAD's sustained-onset barge-in threshold until a proper
        // duplex-engine AEC configuration lands (route agent playback
        // through the same engine's output, then enable VP on both nodes).
        // try inputNode.setVoiceProcessingEnabled(true)
        Self.pcmLog.info("voice processing disabled (input-only engine)")

        let inputFormat = inputNode.outputFormat(forBus: 0)
        Self.pcmLog.info(
            "input format rate=\(inputFormat.sampleRate) ch=\(inputFormat.channelCount)")
        guard
            inputFormat.sampleRate > 0,
            let targetFormat = AVAudioFormat(
                commonFormat: .pcmFormatInt16,
                sampleRate: 16_000,
                channels: 1,
                interleaved: true
            ),
            let converter = AVAudioConverter(from: inputFormat, to: targetFormat)
        else {
            voiceError = "Could not configure PCM conversion for this input device."
            Self.pcmLog.error("PCM converter setup failed (rate=\(inputFormat.sampleRate))")
            return nil
        }

        let (stream, continuation) = AsyncStream<Data>.makeStream()
        // 250ms @ 16 kHz mono s16le = 16000 * 0.25 * 2 bytes.
        let accumulator = PCMFrameAccumulator(frameSize: 8_000)

        inputNode.installTap(onBus: 0, bufferSize: 4096, format: inputFormat) { buffer, _ in
            // Runs on the audio render thread: convert to 16k mono Int16,
            // slice into fixed frames, and yield (the continuation is
            // Sendable; observable state is never touched from here).
            let ratio = targetFormat.sampleRate / inputFormat.sampleRate
            let capacity = AVAudioFrameCount((Double(buffer.frameLength) * ratio).rounded(.up)) + 64
            guard let outBuffer = AVAudioPCMBuffer(pcmFormat: targetFormat, frameCapacity: capacity)
            else { return }

            var fed = false
            let status = converter.convert(to: outBuffer, error: nil) { _, outStatus in
                if fed {
                    outStatus.pointee = .noDataNow
                    return nil
                }
                fed = true
                outStatus.pointee = .haveData
                return buffer
            }
            guard status != .error, outBuffer.frameLength > 0,
                let channelData = outBuffer.int16ChannelData
            else { return }

            let byteCount = Int(outBuffer.frameLength) * MemoryLayout<Int16>.size
            let converted = Data(bytes: channelData[0], count: byteCount)
            for frame in accumulator.append(converted) {
                continuation.yield(frame)
            }
        }

        engine.prepare()
        do {
            try engine.start()
            Self.pcmLog.info("PCM engine started")
        } catch {
            inputNode.removeTap(onBus: 0)
            continuation.finish()
            voiceError = "Could not start the audio engine: \(error.localizedDescription)"
            Self.pcmLog.error("PCM engine start failed: \(error.localizedDescription)")
            return nil
        }

        pcmEngine = engine
        pcmContinuation = continuation
        pcmAccumulator = accumulator
        isCapturingPCM = true
        return stream
    }

    /// Stops PCM capture: removes the tap, stops the engine, flushes the
    /// accumulator's remaining sub-frame bytes as a final (short) frame,
    /// and finishes the stream.
    public func stopPCMStreaming() {
        guard isCapturingPCM else { return }
        pcmEngine?.inputNode.removeTap(onBus: 0)
        pcmEngine?.stop()
        pcmEngine = nil

        if let tail = pcmAccumulator?.drain(), !tail.isEmpty {
            pcmContinuation?.yield(tail)
        }
        pcmAccumulator = nil
        pcmContinuation?.finish()
        pcmContinuation = nil
        isCapturingPCM = false

        #if os(iOS)
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        #endif
    }

    // MARK: - Voice-reply playback

    /// Decodes and plays a whole-reply (non-chunked) voice message,
    /// stopping any in-flight playback — including a chunked queue — first
    /// (stop-and-replace semantics).
    public func play(base64: String, mimeType: String) {
        stopPlayback()
        guard let data = Data(base64Encoded: base64) else {
            voiceError = "Could not decode the voice reply audio."
            return
        }
        startPlayer(with: data, chunked: false)
    }

    /// Stops any in-flight voice-reply playback, discarding queued chunks.
    public func stopPlayback() {
        chunkQueue.removeAll()
        isChunkedPlayback = false
        audioPlayer?.stop()
        audioPlayer = nil
        isPlaying = false
    }

    // MARK: - Chunked voice-reply playback

    /// True while agent reply audio is audible or queued — the barge-in
    /// condition for conversation mode (stricter VAD onset applies).
    public var hasPendingReplyAudio: Bool {
        isPlaying || !chunkQueue.isEmpty
    }

    /// FIFO of decoded audio chunks awaiting playback (per-sentence TTS).
    private var chunkQueue: [Data] = []
    /// True while the active `audioPlayer` (or an empty-queue lull between
    /// chunks) belongs to a chunked reply — the finish handler chains to the
    /// next queued chunk instead of just stopping.
    private var isChunkedPlayback = false

    /// Enqueues one decoded chunk of a streamed voice reply, starting
    /// playback immediately if nothing is playing. Chunks play back-to-back
    /// via delegate chaining (sentence boundaries tolerate the tiny gap);
    /// chunks arriving while playing simply append to the queue.
    public func enqueueReplyChunk(base64: String, mimeType: String) {
        guard let data = Data(base64Encoded: base64) else {
            voiceError = "Could not decode a voice reply chunk."
            return
        }
        chunkQueue.append(data)
        isChunkedPlayback = true
        if !isPlaying {
            playNextChunk()
        }
    }

    /// Drops all queued chunks and stops chunked playback — used when a new
    /// turn's chunk 0 arrives and anything still queued is stale.
    public func resetReplyChunkQueue() {
        guard isChunkedPlayback else { return }
        stopPlayback()
    }

    private func playNextChunk() {
        guard !chunkQueue.isEmpty else {
            // Queue drained but the turn may still be streaming: stay in
            // chunked mode so a late chunk resumes playback seamlessly.
            return
        }
        let data = chunkQueue.removeFirst()
        startPlayer(with: data, chunked: true)
    }

    /// Shared AVAudioPlayer spin-up for whole and chunked replies.
    private func startPlayer(with data: Data, chunked: Bool) {
        #if os(iOS)
        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.playback, mode: .default)
            try session.setActive(true)
        } catch {
            voiceError = "Could not configure audio playback: \(error.localizedDescription)"
            return
        }
        #endif

        do {
            let player = try AVAudioPlayer(data: data)
            player.delegate = self
            audioPlayer = player
            isChunkedPlayback = chunked
            isPlaying = player.play()
        } catch {
            voiceError = "Could not play the voice reply: \(error.localizedDescription)"
            if chunked {
                // Skip the undecodable chunk rather than stalling the queue.
                playNextChunk()
            }
        }
    }

    /// Delegate-chained continuation: called on the main actor when the
    /// current player finishes (or fails to decode mid-flight).
    fileprivate func handlePlaybackFinished(errorDescription: String? = nil) {
        audioPlayer = nil
        isPlaying = false
        if let errorDescription {
            voiceError = errorDescription
        }
        if isChunkedPlayback {
            playNextChunk()
        }
    }

    // MARK: - Fallback TTS

    /// Speaks `text` via on-device text-to-speech, used when a voice turn's
    /// server-side audio reply hasn't arrived within the fallback window.
    public func speakFallback(text: String) {
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        let utterance = AVSpeechUtterance(string: text)
        utterance.rate = AVSpeechUtteranceDefaultSpeechRate
        speechSynthesizer.speak(utterance)
    }

    /// Cancels a scheduled/in-progress fallback utterance (e.g. because the
    /// real voice reply arrived in the meantime).
    public func cancelFallback() {
        guard speechSynthesizer.isSpeaking else { return }
        speechSynthesizer.stopSpeaking(at: .immediate)
    }
}

extension VoiceController: AVAudioPlayerDelegate {
    public nonisolated func audioPlayerDidFinishPlaying(_ player: AVAudioPlayer, successfully flag: Bool) {
        Task { @MainActor [weak self] in
            self?.handlePlaybackFinished()
        }
    }

    public nonisolated func audioPlayerDecodeErrorDidOccur(_ player: AVAudioPlayer, error: Error?) {
        let description = error?.localizedDescription ?? "Voice reply playback failed."
        Task { @MainActor [weak self] in
            self?.handlePlaybackFinished(errorDescription: description)
        }
    }
}

/// Lock-protected byte accumulator that slices an incoming PCM byte stream
/// into fixed-size frames. Written from the audio render thread (the tap),
/// drained from the main actor on stop — hence the lock.
final class PCMFrameAccumulator: @unchecked Sendable {
    private let lock = NSLock()
    private var buffer = Data()
    private let frameSize: Int

    init(frameSize: Int) {
        self.frameSize = frameSize
    }

    /// Appends `data` and returns every complete frame now available.
    func append(_ data: Data) -> [Data] {
        lock.lock()
        defer { lock.unlock() }
        buffer.append(data)
        var frames: [Data] = []
        while buffer.count >= frameSize {
            frames.append(buffer.prefix(frameSize))
            buffer.removeFirst(frameSize)
        }
        return frames
    }

    /// Returns (and clears) any remaining sub-frame bytes.
    func drain() -> Data {
        lock.lock()
        defer { lock.unlock() }
        let tail = buffer
        buffer = Data()
        return tail
    }
}
