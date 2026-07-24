// KeychainStore.swift
// Minimal generic-password Keychain wrapper for the app's secrets: the edge
// bearer token and the device enrollment private key. These are real
// credentials (the token authenticates every edge connection), so they must
// not sit in plaintext UserDefaults, which is unencrypted at rest and
// captured in device backups.

import Foundation
import Security

enum KeychainStore {
    /// Keychain service namespace for all PhiloticApp items.
    static let service = "com.philotic.apple"

    /// Reads the item stored under `key`, or `nil` when absent/unreadable.
    static func data(forKey key: String) -> Data? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        guard status == errSecSuccess else { return nil }
        return result as? Data
    }

    /// Inserts or updates the item stored under `key`.
    @discardableResult
    static func set(_ data: Data, forKey key: String) -> Bool {
        let base: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
        ]
        let update: [String: Any] = [kSecValueData as String: data]
        let status = SecItemUpdate(base as CFDictionary, update as CFDictionary)
        if status == errSecItemNotFound {
            var insert = base
            insert[kSecValueData as String] = data
            // Reachable after first unlock (the client reconnects in the
            // background) but never migrated to another device via backup.
            insert[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
            return SecItemAdd(insert as CFDictionary, nil) == errSecSuccess
        }
        return status == errSecSuccess
    }

    /// Removes the item stored under `key` (missing items are fine).
    static func remove(forKey key: String) {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
        ]
        SecItemDelete(query as CFDictionary)
    }
}
