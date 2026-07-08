// ConnectionSettings.swift
// Persisted connection configuration: the anchor hotel URL and the edge
// bearer credentials obtained either via enrollment or pasted in manually.

import Foundation

/// Everything needed to reach and authenticate against a hotel's
/// `philotic-web` edge surface. Non-secret fields persist to `UserDefaults`;
/// the edge bearer token lives in the Keychain (see `ConnectionSettingsStore`).
public struct ConnectionSettings: Codable, Equatable, Sendable {
    /// Base URL of the hotel's `philotic-web`, e.g. "http://100.79.239.64:7700".
    public var anchorURLString: String
    /// Edge node id assigned at enrollment (or entered manually for testing).
    public var nodeId: String
    /// Edge bearer token presented on every connection.
    public var edgeToken: String
    /// Human-friendly device name advertised in `EdgeCapabilities`.
    public var deviceName: String

    public init(
        anchorURLString: String = "",
        nodeId: String = "",
        edgeToken: String = "",
        deviceName: String = ConnectionSettings.defaultDeviceName()
    ) {
        self.anchorURLString = anchorURLString
        self.nodeId = nodeId
        self.edgeToken = edgeToken
        self.deviceName = deviceName
    }

    public var anchorURL: URL? {
        guard !anchorURLString.isEmpty else { return nil }
        return URL(string: anchorURLString)
    }

    /// The edge WebSocket URL derived from the anchor URL
    /// (http(s)://host:port -> ws(s)://host:port/api/edge/ws).
    public var edgeWebSocketURL: URL? {
        guard let anchorURL, var components = URLComponents(url: anchorURL, resolvingAgainstBaseURL: false) else {
            return nil
        }
        switch components.scheme {
        case "https": components.scheme = "wss"
        default: components.scheme = "ws"
        }
        components.path = (components.path.hasSuffix("/") ? String(components.path.dropLast()) : components.path)
            + "/api/edge/ws"
        return components.url
    }

    public var isConfigured: Bool {
        anchorURL != nil && !nodeId.isEmpty && !edgeToken.isEmpty
    }

    #if os(iOS)
    public static func defaultDeviceName() -> String {
        "iOS Device"
    }
    #else
    public static func defaultDeviceName() -> String {
        "Mac"
    }
    #endif
}

/// Persists `ConnectionSettings`: non-secret fields to `UserDefaults`, the
/// edge bearer token to the Keychain. Settings blobs written by older builds
/// (which carried the token in the defaults blob) are migrated to the
/// Keychain on first load.
public enum ConnectionSettingsStore {
    private static let key = "com.philotic.apple.connectionSettings"
    private static let tokenKeychainKey = "com.philotic.apple.edgeToken"

    public static func load(defaults: UserDefaults = .standard) -> ConnectionSettings {
        guard let data = defaults.data(forKey: key),
            var decoded = try? JSONDecoder().decode(ConnectionSettings.self, from: data)
        else {
            return ConnectionSettings()
        }
        if decoded.edgeToken.isEmpty {
            if let tokenData = KeychainStore.data(forKey: tokenKeychainKey),
                let token = String(data: tokenData, encoding: .utf8)
            {
                decoded.edgeToken = token
            }
        } else {
            // Legacy blob with a plaintext token: re-save, which moves the
            // token to the Keychain and strips it from UserDefaults.
            save(decoded, defaults: defaults)
        }
        return decoded
    }

    public static func save(_ settings: ConnectionSettings, defaults: UserDefaults = .standard) {
        if settings.edgeToken.isEmpty {
            KeychainStore.remove(forKey: tokenKeychainKey)
        } else {
            KeychainStore.set(Data(settings.edgeToken.utf8), forKey: tokenKeychainKey)
        }
        var sanitized = settings
        sanitized.edgeToken = ""
        guard let data = try? JSONEncoder().encode(sanitized) else { return }
        defaults.set(data, forKey: key)
    }
}
