import Foundation
import XCTest
@testable import PhiloticApp

@MainActor
final class VoicePrivacyBoundaryTests: XCTestCase {
    private func session() -> ChatSessionManager {
        ChatSessionManager(
            conversationStore: ConversationStore(fileURL: FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)),
            initialSettings: ConnectionSettings(), startsReconnectTriggers: false)
    }

    func testRawStreamingAndHandsFreeDenyBeforeCapture() async {
        let session = session()
        await session.startVoiceStreaming()
        XCTAssertFalse(session.voiceController.isCapturingPCM)
        XCTAssertTrue(session.voiceController.voiceError?.contains("privacy") == true)
        await session.startConversation()
        XCTAssertFalse(session.isConversationActive)
        XCTAssertFalse(session.voiceController.isCapturingPCM)
        XCTAssertTrue(session.voiceController.voiceError?.contains("private local transcription") == true)
        XCTAssertTrue(session.transcribeOnDevice)
    }

    func testUploadDenialDeletesSyntheticRecordingWithoutSending() async throws {
        let session = session()
        let file = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try Data("synthetic recording".utf8).write(to: file)
        await session.sendVoiceRecording(fileURL: file)
        XCTAssertFalse(FileManager.default.fileExists(atPath: file.path))
        XCTAssertFalse(session.isSendingVoice)
        XCTAssertTrue(session.voiceController.voiceError?.contains("privacy") == true)
    }
}
