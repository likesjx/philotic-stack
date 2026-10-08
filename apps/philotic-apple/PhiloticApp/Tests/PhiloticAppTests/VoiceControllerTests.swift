import AVFoundation
import XCTest

@testable import PhiloticApp

@MainActor
final class VoiceControllerTests: XCTestCase {
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
