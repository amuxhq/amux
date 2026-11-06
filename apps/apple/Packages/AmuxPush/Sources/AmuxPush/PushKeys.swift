import CryptoKit
import Foundation
import Security

/// The private keys pushes are sealed to, one per profile, by the profile's
/// host id, which is what a push's `to` names.
public protocol PushKeys: Sendable {
    /// The key for `host`, if this phone holds one.
    func key(for host: String) throws -> Curve25519.KeyAgreement.PrivateKey?
    /// Keeps a new key for `host`, in place of any it had.
    func makeKey(for host: String) throws -> Curve25519.KeyAgreement.PrivateKey
    /// Forgets `host`'s key. One that was never there is no failure.
    func removeKey(for host: String) throws
}

/// The keychain would not do what was asked, with its status code.
public struct PushKeyFailure: Error, Equatable, Sendable {
    public let status: Int32
}

/// Push keys in a keychain group the app and its notification extension share.
///
/// The extension is a separate process that cannot see the app's files, so
/// this is the one place both can read. A key is readable after the first
/// unlock, because a push arrives on a locked phone, and stays on this device:
/// a restored phone has other identities and pairs again.
public struct PushKeychain: PushKeys {
    private static let service = "sh.amux.push"

    private let group: String

    /// The group this bundle's Info.plist names. The team's prefix is a build
    /// setting, since it isn't written in this repository, so its full name
    /// can't be known here. A bundle naming none reads and keeps nothing.
    public init() {
        group = Bundle.main.object(forInfoDictionaryKey: "AmuxPushKeychainGroup") as? String ?? ""
    }

    private func query(for host: String) -> [CFString: Any] {
        [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: Self.service,
            kSecAttrAccount: host.lowercased(),
            kSecAttrAccessGroup: group,
            kSecUseDataProtectionKeychain: true,
        ]
    }

    public func key(for host: String) throws -> Curve25519.KeyAgreement.PrivateKey? {
        var query = query(for: host)
        query[kSecReturnData] = true
        var found: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &found)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let raw = found as? Data else {
            throw PushKeyFailure(status: status)
        }
        return try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: raw)
    }

    public func makeKey(for host: String) throws -> Curve25519.KeyAgreement.PrivateKey {
        try removeKey(for: host)
        let key = Curve25519.KeyAgreement.PrivateKey()
        var item = query(for: host)
        item[kSecValueData] = key.rawRepresentation
        item[kSecAttrAccessible] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let status = SecItemAdd(item as CFDictionary, nil)
        guard status == errSecSuccess else { throw PushKeyFailure(status: status) }
        return key
    }

    public func removeKey(for host: String) throws {
        let status = SecItemDelete(query(for: host) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw PushKeyFailure(status: status)
        }
    }
}
