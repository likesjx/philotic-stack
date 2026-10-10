import XCTest
@testable import PhiloticApp

@MainActor
final class VoiceStartupTests: XCTestCase {
    func testCancelledPCMStartNeverAcquiresCapture() async {
        let voice = VoiceController()
        let attempt = Task { await voice.startPCMStreaming() }
        attempt.cancel()
        let stream = await attempt.value
        XCTAssertNil(stream)
        XCTAssertFalse(voice.isCapturingPCM)
        XCTAssertFalse(voice.isRecording)
        XCTAssertFalse(voice.isListening)
        XCTAssertNil(voice.voiceError)
    }

    func testCancellationWhilePCMPermissionPendingNeverStartsEngine() async {
        var permission: CheckedContinuation<Bool, Never>?
        let voice = VoiceController(available: true, onDevice: true,
                                    speechAuthorization: { true }, microphoneAuthorization: {
            await withCheckedContinuation { permission = $0 }
        })
        let attempt = Task { await voice.startPCMStreaming() }
        while permission == nil { await Task.yield() }
        attempt.cancel()
        permission?.resume(returning: true)
        let stream = await attempt.value
        XCTAssertNil(stream)
        XCTAssertFalse(voice.isCapturingPCM)
        XCTAssertNil(voice.voiceError)
    }

    func testRepeatedStartAndStopWhilePCMPermissionPending() async {
        var asks = 0
        var permission: CheckedContinuation<Bool, Never>?
        let voice = VoiceController(available: true, onDevice: true,
                                    speechAuthorization: { true }, microphoneAuthorization: {
            asks += 1
            return await withCheckedContinuation { permission = $0 }
        })
        let attempt = Task { await voice.startPCMStreaming() }
        while permission == nil { await Task.yield() }
        let repeated = await voice.startPCMStreaming()
        XCTAssertNil(repeated)
        XCTAssertEqual(asks, 1)
        voice.stopPCMStreaming()
        voice.voiceError = "newer surface state"
        permission?.resume(returning: false)
        let stream = await attempt.value
        XCTAssertNil(stream)
        XCTAssertFalse(voice.isCapturingPCM)
        XCTAssertEqual(voice.voiceError, "newer surface state")
    }

    func testStalePCMLeaseCleanupCannotCancelReplacementPermissionWait() async {
        var permissions: [CheckedContinuation<Bool, Never>] = []
        let voice = VoiceController(available: true, onDevice: true,
                                    speechAuthorization: { true }, microphoneAuthorization: {
            await withCheckedContinuation { permissions.append($0) }
        })
        let oldLease = UUID()
        let old = Task { await voice.startPCMStreaming(lease: oldLease) }
        while permissions.count < 1 { await Task.yield() }
        voice.stopPCMStreaming(lease: oldLease)
        let newLease = UUID()
        let replacement = Task { await voice.startPCMStreaming(lease: newLease) }
        while permissions.count < 2 { await Task.yield() }
        voice.stopPCMStreaming(lease: oldLease)
        permissions[0].resume(returning: true)
        let oldStream = await old.value
        XCTAssertNil(oldStream)
        // If stale cleanup revoked the newer wait, its denial would be ignored.
        permissions[1].resume(returning: false)
        let replacementStream = await replacement.value
        XCTAssertNil(replacementStream)
        XCTAssertTrue(voice.voiceError?.contains("Microphone permission was denied") == true)
        XCTAssertFalse(voice.isCapturingPCM)
    }
}
