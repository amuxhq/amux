import AmuxCore
import XCTest

@testable import AmuxShell

@MainActor
final class DeepLinkTests: XCTestCase {
    /// A link of the shape `amux pair --qr` prints: the host's JSON offer in
    /// URL-safe base64 without padding. Pinned as a whole string so a change
    /// to how the host writes an invitation breaks here rather than on a
    /// phone that cannot read a QR code any more.
    private let link = URL(string: """
        amux://pair?payload=eyJob3N0X2lkIjoiM2ExZjlkMmMtMDAwMC00MDAwLTgwMDAtMDAwMDAwMDAwMDAxIiwic2VjcmV0IjpbMSwyLDMs\
        NCw1LDYsNyw4LDksMTAsMTEsMTIsMTMsMTQsMTUsMTYsMTcsMTgsMTksMjAsMjEsMjIsMjMsMjQsMjUsMjYsMjcs\
        MjgsMjksMzAsMzEsMzJdLCJhZGRycyI6W10sImNsb3VkX3VybCI6Imh0dHBzOi8vYW11eC5zaCJ9
        """)!

    func testAPairingLinkReadsAsTheOfferTheHostWrote() throws {
        let invitation = try XCTUnwrap(PairingInvitation(link: link))
        XCTAssertEqual(invitation.host, HostId("3a1f9d2c-0000-4000-8000-000000000001"))
        XCTAssertEqual(invitation.cloudURL, "https://amux.sh")
        XCTAssertEqual(invitation.secret, Array(UInt8(1)...UInt8(32)))
    }

    /// A machine with no account names no address in its invitation, and one
    /// found on a shared network names them: that difference is the whole of
    /// what a phone needs to know before it decides whether an account is
    /// required to take the invitation up.
    private let nearby = URL(string: """
        amux://pair?payload=eyJob3N0X2lkIjoiM2ExZjlkMmMtMDAwMC00MDAwLTgwMDAtMDAwMDAwMDAwMDAxIiwic2VjcmV0IjpbMSwy\
        LDMsNCw1LDYsNyw4LDksMTAsMTEsMTIsMTMsMTQsMTUsMTYsMTcsMTgsMTksMjAsMjEsMjIsMjMsMjQsMjUs\
        MjYsMjcsMjgsMjksMzAsMzEsMzJdLCJhZGRycyI6WyIxOTIuMC4yLjQ6OTAwMSJdfQ
        """)!

    func testAnInvitationWithAnAddressNeedsNoAccount() throws {
        let invitation = try XCTUnwrap(PairingInvitation(link: nearby))
        XCTAssertEqual(invitation.addrs, ["192.0.2.4:9001"])
        XCTAssertNil(invitation.cloudURL)
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

    func testALinkThatCannotBeReadIsRefusedRatherThanShown() {
        let router = Router()
        for text in [
            // Not this app's scheme.
            "https://amux.sh/pair?payload=abc",
            // This app's scheme, nothing it knows how to do.
            "amux://unknown",
            // A sign-in callback: the sign-in's own browser session takes it.
            "amux://callback?code=abc&state=xyz",
            // A pairing link with no offer in it.
            "amux://pair",
            // An offer that is not base64.
            "amux://pair?payload=%25%25%25",
            // Base64 that is not the JSON a host writes.
            "amux://pair?payload=eyJoZWxsbyI6IndvcmxkIn0",
            // The JSON a host writes, with something that is not a host id.
            "amux://pair?payload=eyJob3N0X2lkIjoibm90LWEtdXVpZCIsImNsb3VkX3VybCI6IiIsInNlY3JldCI6W119",
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
    /// confirming. The invitation keeps the whole offer the machine wrote,
    /// which is what the runtime reads to authenticate; an offer rebuilt from
    /// the parts read out of it would lose any field this build has no name
    /// for. The base64 around it belongs to the URL and is undone here.
    func testAColdLaunchedLinkKeepsTheOfferItArrivedWith() throws {
        let router = Router()
        let invitation = try XCTUnwrap(router.open(link))

        XCTAssertEqual(router.pairing, .link(invitation))
        let encoded = try XCTUnwrap(URLComponents(url: link, resolvingAgainstBaseURL: false)?
            .queryItems?.first(where: { $0.name == "payload" })?.value)
        XCTAssertEqual(
            invitation.payload,
            String(data: try XCTUnwrap(Data(base64URLEncoded: encoded)), encoding: .utf8))
        XCTAssertTrue(invitation.payload.hasPrefix("{"),
                      "the offer handed on is the machine's own JSON")
        // The runtime reads the link itself: rebuilt from the offer, it is the
        // link that arrived, unpadded as the machine wrote it.
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

    /// A malformed link is refused before it can become a screen, so no page
    /// ever has to say it cannot name who it is confirming.
    func testAnUnreadableLinkIsNotAConfirmation() {
        let router = Router()
        for text in [
            "amux://pair",
            "amux://pair?payload=not-base64!!",
            "amux://pair?payload=eyJoZWxsbyI6IndvcmxkIn0",
            "amux://elsewhere?payload=abc",
        ] {
            XCTAssertNil(router.open(URL(string: text)!), text)
        }
        XCTAssertEqual(router.path, [])
    }

}
