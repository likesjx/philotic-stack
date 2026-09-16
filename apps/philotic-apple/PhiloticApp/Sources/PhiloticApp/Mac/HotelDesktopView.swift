#if os(macOS)
import AppKit
import Observation
import SwiftUI
import WebKit

/// The web desktop owns its operator login. Device bearer credentials never
/// enter WebKit, JavaScript, URLs, or this store.
@MainActor @Observable
final class HotelDesktopSession: NSObject, WKNavigationDelegate {
    var address = UserDefaults.standard.string(forKey: "desktop.hotel.address") ?? ""
    private(set) var hotel: URL?
    private(set) var loading = false
    private(set) var error: String?
    @ObservationIgnored private(set) var webView: WKWebView?

    static func origin(_ text: String) -> URL? {
        guard var parts = URLComponents(string: text.trimmingCharacters(in: .whitespacesAndNewlines)),
              ["http", "https"].contains(parts.scheme?.lowercased() ?? ""),
              let host = parts.host, !host.isEmpty,
              parts.user == nil, parts.password == nil else { return nil }
        parts.scheme = parts.scheme?.lowercased()
        parts.host = host.lowercased()
        parts.path = "/"
        parts.query = nil
        parts.fragment = nil
        return parts.url
    }

    func attach() {
        guard let url = Self.origin(address) else {
            error = "Enter a hotel address beginning with https:// or http://, without embedded credentials."
            return
        }
        if hotel == url, let webView { error = nil; webView.reload(); return }
        webView?.stopLoading()
        webView?.navigationDelegate = nil
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .default()
        let browser = WKWebView(frame: .zero, configuration: configuration)
        browser.navigationDelegate = self
        browser.allowsBackForwardNavigationGestures = true
        webView = browser
        hotel = url
        address = url.absoluteString
        UserDefaults.standard.set(address, forKey: "desktop.hotel.address")
        error = nil
        loading = true
        browser.load(URLRequest(url: url))
    }

    func detach() {
        webView?.stopLoading()
        webView?.navigationDelegate = nil
        webView = nil
        hotel = nil
        loading = false
        error = nil
    }

    func webView(_ webView: WKWebView, didStartProvisionalNavigation navigation: WKNavigation!) {
        guard webView === self.webView else { return }
        loading = true
        error = nil
    }
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        guard webView === self.webView else { return }
        loading = false
        // The server's SPA fallback may answer missing .js assets with HTML:
        // navigation succeeds while the desktop remains blank. Inspect only
        // resource response types, never cookies or application contents.
        let probe = """
        (async () => {
          const scripts = Array.from(document.querySelectorAll('script[type="module"][src]'));
          for (const script of scripts) {
            const url = new URL(script.src, location.href);
            if (url.origin !== location.origin) continue;
            const response = await fetch(url, {method: 'HEAD'});
            if (!response.ok || (response.headers.get('content-type') || '').includes('text/html')) return true;
          }
          return false;
        })()
        """
        Task { @MainActor [weak self, weak webView] in
            guard let webView else { return }
            let missing = try? await webView.callAsyncJavaScript("return await \(probe)", arguments: [:], in: nil, contentWorld: .page)
            guard let self, webView === self.webView else { return }
            if missing as? Bool == true {
                self.error = "The hotel’s desktop bundle is incomplete: a JavaScript asset is missing or served as HTML. Repair the hotel’s web bundle, then Reload."
            }
        }
    }
    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        report(error, from: webView)
    }
    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        report(error, from: webView)
    }
    private func report(_ failure: Error, from browser: WKWebView) {
        guard browser === webView, (failure as NSError).code != NSURLErrorCancelled else { return }
        loading = false
        error = failure.localizedDescription
    }
    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        guard webView === self.webView else { return }
        loading = false
        error = "The desktop stopped responding. Reload to reconnect."
    }

    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction,
                 decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        guard let url = action.request.url else { decisionHandler(.cancel); return }
        // Subframes remain subject to WebKit's origin policies. Keep top-level
        // navigation attached to the selected hotel; explicit external links
        // open in the user's browser. Never forward hotel credentials.
        if action.targetFrame?.isMainFrame == false { decisionHandler(.allow); return }
        if Self.origin(url.absoluteString) == hotel {
            if action.targetFrame == nil {
                decisionHandler(.cancel)
                webView.load(action.request)
            } else { decisionHandler(.allow) }
            return
        }
        decisionHandler(.cancel)
        if action.navigationType == .linkActivated,
           ["https", "http"].contains(url.scheme?.lowercased() ?? "") {
            NSWorkspace.shared.open(url)
        } else {
            loading = false
            error = "This page tried to leave the selected hotel. Use Open in Browser for external sign-in."
        }
    }
}

private struct HotelWebView: NSViewRepresentable {
    let browser: WKWebView
    func makeNSView(context: Context) -> WKWebView { browser }
    func updateNSView(_ nsView: WKWebView, context: Context) {}
}

struct HotelDesktopView: View {
    @Bindable var desktop: HotelDesktopSession
    let suggestedAddress: String

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                TextField("Hotel URL", text: $desktop.address)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit { desktop.attach() }
                Button("Attach") { desktop.attach() }
                Button("Reload") { desktop.webView?.reload() }.disabled(desktop.hotel == nil)
                Button("Open in Browser") {
                    if let hotel = desktop.hotel { NSWorkspace.shared.open(hotel) }
                }.disabled(desktop.hotel == nil)
                Button("Detach") { desktop.detach() }.disabled(desktop.hotel == nil)
            }.padding(10)
            HStack {
                if desktop.loading { ProgressView().controlSize(.small) }
                Text(desktop.hotel.map { "Desktop · \($0.host ?? $0.absoluteString)" } ?? "No hotel attached")
                Spacer()
                Text("Operator login is managed by the hotel.")
            }.font(.caption).foregroundStyle(.secondary).padding(.horizontal, 10).padding(.bottom, 8)
            if let error = desktop.error {
                Text(error).font(.callout).foregroundStyle(.red).textSelection(.enabled).padding(10)
            }
            Divider()
            if let browser = desktop.webView {
                HotelWebView(browser: browser).id(ObjectIdentifier(browser))
            } else {
                ContentUnavailableView("Your hotel desktop", systemImage: "desktopcomputer",
                    description: Text("Attach to a hotel to open Philote Manager. Sign in using the desktop’s operator login. Detaching closes the view; use the desktop’s sign-out action to end its session."))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .onAppear {
            if desktop.address.isEmpty { desktop.address = suggestedAddress }
        }
    }
}
#endif
