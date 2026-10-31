import AmuxCore
import Foundation

/// A pairing invitation exactly as it arrived, and nothing more.
///
/// It names the machine that offered it and carries the one-shot secret that
/// proves possession of the offer. Holding it is not pairing: nothing is
/// trusted until the person on this phone looks at the machine's fingerprint
/// and says yes.
public struct PairingInvitation: Hashable, Sendable, CustomStringConvertible {
    public let host: HostId
    /// The relay the machine names, or nothing where it names none. A machine
    /// with no account still issues invitations; its addresses are the whole
    /// of what they carry.
    public let cloudURL: String?
    /// Where the machine says it can be dialled directly. Empty for one only
    /// the relay can see — which is the difference between an invitation this
    /// phone can take up on its own and one that needs an account.
    public let addrs: [String]
    public let secret: [UInt8]
    /// The offer the machine wrote, whole, as the link carried it once the
    /// URL's own encoding is undone.
    ///
    /// Kept whole rather than rebuilt from the parts above: it is what the
    /// runtime reads to authenticate, and a payload this phone reassembled
    /// would lose any field this build has no name for. The parts are read out
    /// of it only so the app can refuse a malformed link before it becomes a
    /// screen.
    public let payload: String

    public init(
        host: HostId, cloudURL: String?, addrs: [String], secret: [UInt8], payload: String
    ) {
        self.host = host
        self.cloudURL = cloudURL
        self.addrs = addrs
        self.secret = secret
        self.payload = payload
    }

    /// Whether taking this invitation up needs an account.
    ///
    /// An invitation carrying addresses is dialled on the network this phone
    /// is on and needs nothing else. One carrying none names a machine only
    /// the relay has seen, and there is no relay without an account.
    public var needsAnAccount: Bool { addrs.isEmpty }

    /// The link itself, as the runtime reads it.
    public var link: String {
        let encoded = Data(payload.utf8).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
        return "\(Self.scheme)://pair?payload=\(encoded)"
    }

    /// The invitation is a secret, so it prints as the machine it came from
    /// and no more; a description that carried the secret would put it into
    /// every log and report that ever mentioned this value.
    public var description: String { "invitation from \(host)" }
}

extension PairingInvitation {
    /// The scheme the CLI's pairing links use.
    public static let scheme = "amux"

    /// Reads a pairing link, or refuses it.
    ///
    /// Everything about the link is checked here rather than on the page it
    /// leads to: a malformed invitation must not become a confirmation screen
    /// that cannot say who it is confirming.
    public init?(link url: URL) {
        guard url.scheme == Self.scheme, url.host == "pair",
            let payload = URLComponents(url: url, resolvingAgainstBaseURL: false)?
                .queryItems?.first(where: { $0.name == "payload" })?.value
        else { return nil }
        self.init(payload: payload)
    }

    /// Reads the payload the `amux pair --qr` link carries: the JSON the host
    /// wrote, in URL-safe base64 without padding.
    init?(payload: String) {
        guard let json = Data(base64URLEncoded: payload),
            let offer = String(data: json, encoding: .utf8),
            let wire = try? JSONDecoder().decode(Wire.self, from: json),
            let host = HostId(wire.hostID)
        else { return nil }
        // The base64 is the URL's, not the machine's: what the machine wrote
        // and what the runtime parses is the JSON inside it.
        self.init(
            host: host, cloudURL: wire.cloudURL, addrs: wire.addrs,
            secret: wire.secret, payload: offer)
    }

    private struct Wire: Decodable {
        let hostID: String
        /// Absent where the machine has no account to name one with.
        let cloudURL: String?
        /// Empty where the machine has no address to name.
        let addrs: [String]
        let secret: [UInt8]

        enum CodingKeys: String, CodingKey {
            case hostID = "host_id"
            case cloudURL = "cloud_url"
            case addrs
            case secret
        }
    }
}

extension Data {
    /// Base64 as a URL carries it: the two substituted characters put back and
    /// the padding the encoder dropped restored.
    init?(base64URLEncoded text: String) {
        var standard = text.replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        let remainder = standard.count % 4
        if remainder > 0 { standard += String(repeating: "=", count: 4 - remainder) }
        guard let data = Data(base64Encoded: standard) else { return nil }
        self = data
    }
}
