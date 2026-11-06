import CryptoKit
import Foundation

/// What a push says once it is opened: which agent on which host, its name,
/// and what it wants of the person.
public struct OpenedPush: Decodable, Equatable, Sendable {
    public var host: String
    public var agent: String
    public var name: String
    public var notice: PushNotice

    public init(host: String, agent: String, name: String, notice: PushNotice) {
        self.host = host
        self.agent = agent
        self.name = name
        self.notice = notice
    }
}

/// Opening what a daemon sealed to this phone.
///
/// HPKE in base mode: the 32-byte encapsulated key, then the ciphertext, as
/// base64. A push the wrong key or a stale key can't open comes back nil, and
/// the plain alert the service sent stays on show.
public enum SealedPush {
    /// Binds a sealed push to what it is for. The daemon seals with the same
    /// words.
    private static let info = Data("amux push 1".utf8)
    private static let encapsulatedKeySize = 32

    public static func open(
        _ sealed: String, with key: Curve25519.KeyAgreement.PrivateKey
    ) -> OpenedPush? {
        guard let plain = plaintext(sealed, with: key) else { return nil }
        return try? JSONDecoder().decode(OpenedPush.self, from: plain)
    }

    /// The sealed JSON, undecoded.
    public static func plaintext(
        _ sealed: String, with key: Curve25519.KeyAgreement.PrivateKey
    ) -> Data? {
        guard let bytes = Data(base64Encoded: sealed), bytes.count > encapsulatedKeySize
        else { return nil }
        return try? {
            var recipient = try HPKE.Recipient(
                privateKey: key, ciphersuite: .Curve25519_SHA256_ChachaPoly, info: info,
                encapsulatedKey: Data(bytes.prefix(encapsulatedKeySize)))
            return try recipient.open(Data(bytes.dropFirst(encapsulatedKeySize)))
        }()
    }

    /// Opens the sealed text of a push's payload, with the key of the profile
    /// it is addressed to. A key that can't be read, as before the first
    /// unlock after a restart, leaves it closed.
    public static func open(_ payload: [AnyHashable: Any], keys: any PushKeys) -> OpenedPush? {
        guard let amux = payload["amux"] as? [String: Any],
              let to = amux["to"] as? String, let sealed = amux["sealed"] as? String,
              let key = try? keys.key(for: to) else { return nil }
        return open(sealed, with: key)
    }
}
