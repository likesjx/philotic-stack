// DeviceIdentity.swift
// Per-device enrollment keypair: Curve25519 key generated on first use and
// persisted to the Keychain. Keys written to UserDefaults by older builds
// are migrated to the Keychain on first access.

import CryptoKit
import Foundation

public enum DeviceIdentity {
    private static let privateKeyDefaultsKey = "com.philotic.apple.devicePrivateKey"
    private static let privateKeyKeychainKey = "com.philotic.apple.devicePrivateKey"

    /// Returns the device's Curve25519 key agreement keypair, generating and
    /// persisting one (in the Keychain) on first call.
    public static func keyPair(defaults: UserDefaults = .standard) -> Curve25519.KeyAgreement.PrivateKey {
        if let data = KeychainStore.data(forKey: privateKeyKeychainKey),
            let key = try? Curve25519.KeyAgreement.PrivateKey(rawRepresentation: data)
        {
            return key
        }
        // Legacy location: migrate a key persisted by pre-Keychain builds so
        // the device identity (and therefore its enrollment) is preserved.
        if let legacy = defaults.data(forKey: privateKeyDefaultsKey),
            let key = try? Curve25519.KeyAgreement.PrivateKey(rawRepresentation: legacy)
        {
            KeychainStore.set(legacy, forKey: privateKeyKeychainKey)
            defaults.removeObject(forKey: privateKeyDefaultsKey)
            return key
        }
        let newKey = Curve25519.KeyAgreement.PrivateKey()
        KeychainStore.set(newKey.rawRepresentation, forKey: privateKeyKeychainKey)
        return newKey
    }

    /// Base64-encoded public key suitable for `EnrollmentRequest.devicePubkeyB64`.
    public static func publicKeyBase64(defaults: UserDefaults = .standard) -> String {
        keyPair(defaults: defaults).publicKey.rawRepresentation.base64EncodedString()
    }

    #if os(iOS)
    public static let platform = "ios"
    #else
    public static let platform = "macos"
    #endif
}
