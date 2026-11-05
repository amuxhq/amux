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
    /// Where the machine says it can be dialled directly, as `ip:port`. Empty
    /// for one only the relay can see — which is the difference between an
    /// invitation this phone can take up on its own and one that needs an
    /// account.
    public let addrs: [String]
    /// The link itself, exactly as it arrived: what the runtime reads to
    /// authenticate. The parts above are read out of it only so the app can
    /// refuse a malformed link before it becomes a screen.
    public let link: String

    public init(host: HostId, addrs: [String], link: String) {
        self.host = host
        self.addrs = addrs
        self.link = link
    }

    /// Whether taking this invitation up needs an account.
    ///
    /// An invitation carrying addresses is dialled on the network this phone
    /// is on and needs nothing else. One carrying none names no address to
    /// dial, so unless discovery finds the machine nearby it is taken up
    /// through the relay, and there is no relay without an account.
    public var needsAnAccount: Bool { addrs.isEmpty }

    /// The invitation is a secret, so it prints as the machine it came from
    /// and no more; a description that carried the secret would put it into
    /// every log and report that ever mentioned this value.
    public var description: String { "invitation from \(host)" }
}

extension PairingInvitation {
    /// The path of the cloud's pairing page, whose fragment carries the
    /// invitation: `https://amux.sh/p#<invitation>`.
    public static let page = "/p"

    /// The one invitation layout a machine writes.
    static let version: UInt8 = 1

    /// Reads a pairing link, or refuses it.
    ///
    /// The QR code a host shows is the cloud's page,
    /// `https://<cloud>/p#<invitation>`, so a phone without the app lands on
    /// the page that sends it to the App Store; with the app installed the
    /// system hands the link straight here. The invitation sits after the
    /// `#`, which a browser never sends, so the page never sees it. A
    /// self-hosted cloud's page is read the same way: whichever cloud's page
    /// it is, the invitation is the host's own.
    ///
    /// Everything about the link is checked here rather than on the page it
    /// leads to: a malformed invitation must not become a confirmation screen
    /// that cannot say who it is confirming.
    public init?(link url: URL) {
        guard url.scheme == "https" || url.scheme == "http",
            let host = url.host, !host.isEmpty, url.path == Self.page,
            let fragment = url.fragment, let bytes = Data(base64URLEncoded: fragment)
        else { return nil }
        self.init(bytes: [UInt8](bytes), link: url.absoluteString)
    }

    /// Reads the invitation's bytes, in URL-safe base64 without padding in
    /// the link: a version (1), the host id (16), the one-shot secret (32),
    /// the port (two, big-endian) and any number of IPv4 addresses (four
    /// each), all at that port.
    init?(bytes: [UInt8], link: String) {
        let head = 1 + 16 + 32 + 2
        guard bytes.count >= head, bytes[0] == Self.version, (bytes.count - head) % 4 == 0,
            let host = HostId(bytes: Array(bytes[1..<17]))
        else { return nil }
        let port = UInt16(bytes[head - 2]) << 8 | UInt16(bytes[head - 1])
        let addrs = stride(from: head, to: bytes.count, by: 4).map {
            "\(bytes[$0]).\(bytes[$0 + 1]).\(bytes[$0 + 2]).\(bytes[$0 + 3]):\(port)"
        }
        guard port != 0 || addrs.isEmpty else { return nil }
        self.init(host: host, addrs: addrs, link: link)
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
