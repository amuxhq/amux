import Foundation

/// A link an agent wrote that this phone will open when it is tapped: a web
/// page or an email address, and nothing else.
///
/// Whatever an agent writes may have been put in its mouth by a page or a
/// file it read, and a tap hands the link to whichever app claims its
/// scheme, this one included: an `amux://pair` link would start pairing
/// with a machine somebody else chose, and another app's link performs that
/// app's action. Those schemes are drawn as text and do nothing.
public struct OpenableLink: Equatable, Sendable {
    public let url: URL

    private static let schemes: Set<String> = ["https", "http", "mailto"]

    public init?(_ url: URL) {
        guard let scheme = url.scheme?.lowercased(), Self.schemes.contains(scheme) else {
            return nil
        }
        self.url = url
    }

    public init?(string: String) {
        guard let url = URL(string: string) else { return nil }
        self.init(url)
    }
}
