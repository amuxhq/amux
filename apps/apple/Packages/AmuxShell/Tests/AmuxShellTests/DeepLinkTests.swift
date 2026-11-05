import AmuxCore
import XCTest

@testable import AmuxShell

@MainActor
final class DeepLinkTests: XCTestCase {
    /// A link of the shape a host's QR code holds: the cloud's pairing page
    /// with the host's invitation after the `#`, its bytes in URL-safe base64
    /// without padding. Pinned as a whole string, the same one the host's own
    /// encoder is pinned to, so a change to how the host writes an invitation
    /// breaks here rather than on a phone that cannot read a QR code any more.
    private let link = URL(string:
        "https://amux.sh/p#ATofnSwAAEAAgAAAAAAAAAEBAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4fICMp")!

    func testAPairingLinkReadsAsTheOfferTheHostWrote() throws {
        let invitation = try XCTUnwrap(PairingInvitation(link: link))
        XCTAssertEqual(invitation.host, HostId("3a1f9d2c-0000-4000-8000-000000000001"))
        // The runtime is handed the link that arrived, whole.
        XCTAssertEqual(invitation.link, link.absoluteString)
    }

    /// A machine on a home network names its address in its invitation, and
    /// one only the relay has seen names none: that difference is the whole
    /// of what a phone needs to know before it decides whether an account is
    /// required to take the invitation up.
    private let nearby = URL(string:
        "https://amux.sh/p#ATofnSwAAEAAgAAAAAAAAAEBAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4fICMpwAACBA")!

    func testAnInvitationWithAnAddressNeedsNoAccount() throws {
        let invitation = try XCTUnwrap(PairingInvitation(link: nearby))
        XCTAssertEqual(invitation.addrs, ["192.0.2.4:9001"])
        XCTAssertFalse(invitation.needsAnAccount)
    }

    /// The relay-only invitation: a machine the relay has seen and this
    /// network has not. There is nothing here to dial, so there is nothing to
    /// do about it without an account.
    func testAnInvitationWithNoAddressNeedsAnAccount() throws {
        let invitation = try XCTUnwrap(PairingInvitation(link: link))
        XCTAssertEqual(invitation.addrs, [])
        XCTAssertTrue(invitation.needsAnAccount)
    }

    func testAPairingLinkPairsWithNobodyAndAsksInstead() throws {
        let router = Router()
        let invitation = try XCTUnwrap(router.open(link))

        // The link raises a sheet that names the machine and waits. Trust is
        // committed by the person looking at the fingerprint, never by the
        // arrival of a URL anybody could send.
        XCTAssertEqual(router.pairing, .link(invitation))
        XCTAssertEqual(router.path, [])
    }

    func testAnInvitationDoesNotPrintItsSecret() throws {
        let invitation = try XCTUnwrap(PairingInvitation(link: link))
        XCTAssertEqual(invitation.description, "invitation from 3a1f9d2c-0000-4000-8000-000000000001")
    }

    /// Whichever cloud's pairing page the link is, amux.sh's or a self-hosted
    /// one's, the invitation after the `#` reads the same.
    func testAnyCloudsPairingPageReadsAsTheSameOffer() throws {
        let payload = try XCTUnwrap(link.fragment)
        let amux = try XCTUnwrap(PairingInvitation(link: link))
        for text in [
            "https://cloud.example.com/p#\(payload)",
            "http://localhost:8080/p#\(payload)",
        ] {
            let page = try XCTUnwrap(PairingInvitation(link: URL(string: text)!), text)
            XCTAssertEqual(page.host, amux.host, text)
            XCTAssertEqual(page.addrs, amux.addrs, text)
            XCTAssertEqual(page.link, text)
        }
        // The same invitation anywhere but the pairing page is no link.
        for text in [
            "https://amux.sh/get#\(payload)",
            "https://amux.sh/p/extra#\(payload)",
            "https://amux.sh/#\(payload)",
            "ftp://amux.sh/p#\(payload)",
            "amux://pair?payload=\(payload)",
        ] {
            XCTAssertNil(PairingInvitation(link: URL(string: text)!), text)
        }
    }

    /// A universal link reaches the router whether it started the app or
    /// found it running.
    func testTheCloudsPairingPageRaisesTheSheet() throws {
        let router = Router()
        let invitation = try XCTUnwrap(router.open(link))
        XCTAssertEqual(router.pairing, .link(invitation))
        XCTAssertEqual(router.path, [])
    }

    func testALinkThatCannotBeReadIsRefusedRatherThanShown() {
        let router = Router()
        for text in [
            // The cloud's other pages, which carry no invitation.
            "https://amux.sh/pair?payload=abc",
            // The pairing page with no invitation after its `#`.
            "https://amux.sh/p",
            "https://amux.sh/p#",
            // The pairing page with something that is not base64.
            "https://amux.sh/p#not-base64!!",
            // Base64 that is not an invitation: JSON, as hosts once wrote.
            "https://amux.sh/p#eyJoZWxsbyI6IndvcmxkIn0",
            "https://amux.sh/p?payload=eyJoZWxsbyI6IndvcmxkIn0",
            // An invitation of a later version.
            "https://amux.sh/p#AjofnSwAAEAAgAAAAAAAAAEBAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4fICMpwAACBA",
            // One cut short before its port ends.
            "https://amux.sh/p#ATofnSwAAEAAgAAAAAAAAAEBAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4fICM",
            // One whose last address is cut short.
            "https://amux.sh/p#ATofnSwAAEAAgAAAAAAAAAEBAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4fICMpwAAC",
            // This app's scheme, nothing it knows how to do.
            "amux://unknown",
            // A sign-in callback: the sign-in's own browser session takes it.
            "amux://callback?code=abc&state=xyz",
            "amux://pair",
        ] {
            let url = URL(string: text)!
            XCTAssertNil(PairingInvitation(link: url), "\(text) should not read as a link")
            XCTAssertNil(router.open(url), "\(text) should lead nowhere")
        }
        XCTAssertEqual(router.path, [])
        XCTAssertEqual(router.tab, .agents)
    }

    func testBase64AsAURLCarriesIt() {
        // The two characters a URL substitutes, and the padding it drops.
        XCTAssertEqual(Data(base64URLEncoded: "-_-_"), Data([0xfb, 0xff, 0xbf]))
        XCTAssertEqual(Data(base64URLEncoded: "YQ"), Data("a".utf8))
        XCTAssertEqual(Data(base64URLEncoded: "YWI"), Data("ab".utf8))
        XCTAssertEqual(Data(base64URLEncoded: "YWJj"), Data("abc".utf8))
        XCTAssertNil(Data(base64URLEncoded: "not base64"))
    }

    /// A link handed over during a cold start, before this phone has signed in.
    ///
    /// The sheet is raised straight away — it has to be there for the
    /// launch to land on — and it pairs with nobody, because arriving is not
    /// confirming. The invitation keeps the link it arrived as, which is what
    /// the runtime reads to authenticate.
    func testAColdLaunchedLinkKeepsTheLinkItArrivedAs() throws {
        let router = Router()
        let invitation = try XCTUnwrap(router.open(link))

        XCTAssertEqual(router.pairing, .link(invitation))
        XCTAssertEqual(invitation.link, link.absoluteString)
    }

    /// A sign-in in between does not lose the invitation.
    ///
    /// Signing in swaps the stores under the page. The page has been asked
    /// nothing yet, because a signed-out phone has no runtime to ask the
    /// machine with, so the arrival of an account is what puts the invitation
    /// to the machine — once, and once more for a second account, because
    /// trust is per account.
    func testAnInvitationIsPutToEachAccountOnceAndSurvivesSigningIn() {
        var asked = LinkAsked()
        let signedOut = AccountId("signed-out")
        let ada = AccountId("ada")
        let grace = AccountId("grace")

        XCTAssertTrue(asked.shouldAsk(signedOut))
        // Nothing could be asked while signed out, so nothing is recorded and
        // the invitation is still unspent.
        XCTAssertTrue(asked.shouldAsk(signedOut))

        asked.asked(ada)
        XCTAssertFalse(asked.shouldAsk(ada), "a redraw asked the machine again")
        XCTAssertTrue(asked.shouldAsk(grace), "a second account inherited the first's answer")
    }
}
