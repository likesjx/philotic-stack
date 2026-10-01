import AuthenticationServices
import PhiloticKit
import SwiftUI

@MainActor
final class NativeCortexSignIn: NSObject, ASWebAuthenticationPresentationContextProviding {
    private var browser: ASWebAuthenticationSession?
    private var continuation: CheckedContinuation<URL, Error>?
    private var timeout: Task<Void, Never>?
    private var attempt: NativeSignInAttempt?
    private var generation = UUID()

    func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        #if os(iOS)
        return UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            .flatMap(\.windows).first(where: \.isKeyWindow) ?? ASPresentationAnchor()
        #else
        return NSApp.keyWindow ?? NSApp.windows.first ?? ASPresentationAnchor()
        #endif
    }

    func cancel() {
        generation = UUID()
        attempt?.cancel(); attempt = nil
        browser?.cancel()
        finish(.failure(CancellationError()))
    }

    private func finish(_ result: Result<URL, Error>) {
        let pending = continuation; continuation = nil
        timeout?.cancel(); timeout = nil; browser = nil
        pending?.resume(with: result)
    }

    func signIn() async throws -> String {
        cancel()
        #if os(iOS)
        let attempt = try NativeSignInAttempt(client: .iOS)
        #else
        let attempt = try NativeSignInAttempt(client: .mac)
        #endif
        self.attempt = attempt
        let generation = generation
        defer {
            attempt.cancel()
            if self.attempt === attempt { self.attempt = nil }
        }
        let callback: URL = try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                self.continuation = continuation
                let browser = ASWebAuthenticationSession(url: attempt.authorizationURL,
                    callbackURLScheme: attempt.client.rawValue) { [weak self] url, _ in
                    Task { @MainActor in
                        guard let self, self.generation == generation else { return }
                        if let url { self.finish(.success(url)) }
                        else { self.finish(.failure(CancellationError())) }
                    }
                }
                browser.presentationContextProvider = self
                self.browser = browser
                if !browser.start() { finish(.failure(NativeCortexClient.Failure.unavailable)); return }
                timeout = Task { [weak self] in
                    try? await Task.sleep(for: .seconds(300))
                    guard !Task.isCancelled else { return }
                    self?.cancel()
                }
            }
        } onCancel: {
            Task { @MainActor in
                if self.generation == generation { self.cancel() }
            }
        }
        try Task.checkCancellation()
        let client = NativeCortexClient()
        let token = try await client.exchange(attempt.consume(callback))
        guard self.generation == generation, !Task.isCancelled else {
            await client.logout(token: token)
            throw CancellationError()
        }
        return token
    }
}
