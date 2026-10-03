import CryptoKit
import Foundation

/// Local export identity: no credentials, node content or bearer token in the
/// marker. A completed reminder is a review signal, never graph-write authority.
public struct OpenLoopReminderLink: Equatable, Sendable {
    public let marker: String

    public init(hotelURL: URL, nodeID: String) throws {
        guard let parts = URLComponents(url: hotelURL, resolvingAgainstBaseURL: false),
              ["http", "https"].contains(parts.scheme?.lowercased() ?? ""),
              let host = parts.host, !host.isEmpty,
              parts.user == nil, parts.password == nil, parts.query == nil, parts.fragment == nil,
              nodeID.hasPrefix("life:"), nodeID.count > 5
        else { throw Failure.invalidIdentity }
        let port = parts.port ?? (parts.scheme?.lowercased() == "https" ? 443 : 80)
        let scope = [parts.scheme!.lowercased(), host.lowercased(), String(port),
            parts.path.trimmingCharacters(in: CharacterSet(charactersIn: "/")), nodeID]
        let data = try JSONSerialization.data(withJSONObject: scope)
        marker = "Philotic LifeGraph link: " + SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }

    public func matches(notes: String?) -> Bool {
        notes?.components(separatedBy: .newlines).contains(marker) == true
    }

    public enum Failure: Error { case invalidIdentity }
}
