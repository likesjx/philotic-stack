import AVFoundation
import XCTest

@testable import PhiloticApp

@MainActor
final class VoiceControllerTests: XCTestCase {
    func testUnsupportedLocalRecognitionNeverRequestsPermissions() async {
        var asked = false
        let controller = VoiceController(available: true, onDevice: false,
            speechAuthorization: { asked = true; return true },
            microphoneAuthorization: { asked = true; return true })
        await controller.startListening()
        XCTAssertFalse(asked)
        XCTAssertFalse(controller.isListening)
        XCTAssertTrue(controller.voiceError?.contains("external recognizer") == true)
    }

    func testSpeechPermissionDenialDoesNotRequestMicrophone() async {
        var askedMicrophone = false
        let controller = VoiceController(available: true, onDevice: true,
            speechAuthorization: { false },
            microphoneAuthorization: { askedMicrophone = true; return true })
        await controller.startListening()
        XCTAssertFalse(askedMicrophone)
        XCTAssertFalse(controller.isListening)
        XCTAssertTrue(controller.voiceError?.contains("Speech recognition permission was denied") == true)
    }

    func testMicrophoneDenialLeavesCaptureIdle() async {
        let controller = VoiceController(available: true, onDevice: true,
            speechAuthorization: { true }, microphoneAuthorization: { false })
        await controller.startListening()
        XCTAssertFalse(controller.isListening)
        XCTAssertFalse(controller.isCapturingPCM)
        XCTAssertFalse(controller.isRecording)
        XCTAssertTrue(controller.voiceError?.contains("Microphone permission was denied") == true)
    }

    func testStopWhilePermissionPendingCannotStartMicrophone() async {
        var permission: CheckedContinuation<Bool, Never>?
        let controller = VoiceController(available: true, onDevice: true,
            speechAuthorization: { true },
            microphoneAuthorization: { await withCheckedContinuation { permission = $0 } })
        let start = Task { await controller.startListening() }
        // The continuation is set before the permission request suspends.
        while permission == nil { await Task.yield() }
        controller.stopListening()
        permission?.resume(returning: true)
        await start.value
        XCTAssertFalse(controller.isListening)
        XCTAssertNil(controller.voiceError)
    }

    func testCancelledPlayerCallbackCannotReplaceCurrentError() async throws {
        let controller = VoiceController()
        controller.stopPlayback()
        controller.voiceError = "current failure"
        // A player need not play to reproduce a delegate callback queued
        // before stop/replacement. No microphone or audio device is used.
        let stale = AVAudioPlayer()
        controller.audioPlayerDecodeErrorDidOccur(
            stale, error: NSError(domain: "stale", code: 1))
        await Task.yield()
        XCTAssertEqual(controller.voiceError, "current failure")
        XCTAssertFalse(controller.hasPendingReplyAudio)
    }

    func testRepeatedStopAndInvalidChunksLeaveQueueIdle() {
        let controller = VoiceController()
        for _ in 0..<3 {
            controller.enqueueReplyChunk(base64: "!invalid!", mimeType: "audio/mpeg")
            controller.stopPlayback()
            controller.resetReplyChunkQueue()
            XCTAssertFalse(controller.hasPendingReplyAudio)
            XCTAssertFalse(controller.isPlaying)
        }
    }

    func testSyntheticPCMFramesAndTailAreDrainedOnce() {
        let accumulator = PCMFrameAccumulator(frameSize: 4)
        XCTAssertEqual(accumulator.append(Data([1, 2, 3])), [])
        XCTAssertEqual(accumulator.append(Data([4, 5, 6, 7, 8, 9])),
                       [Data([1, 2, 3, 4]), Data([5, 6, 7, 8])])
        XCTAssertEqual(accumulator.drain(), Data([9]))
        XCTAssertEqual(accumulator.drain(), Data())
    }
}
