import Network
import XCTest
import dnssd

@testable import AmuxCore

/// What the browser reads off the network, and what it refuses to read.
///
/// The browsing itself needs a network with a machine on it and belongs to the
/// journey that runs one. What is checked here is everything the browser
/// decides on its own: which advertisement is amux's, which resolved address
/// the shared library can dial, and which browser state means a person said no.
final class DiscoveryTests: XCTestCase {
    private let host = HostId(UUID(uuidString: "6D7A4B1E-3C2F-4A58-9B0D-1E2F3A4B5C6D")!)

    func testAnAdvertisementIsReadFromItsOwnRecord() {
        let record = NWTXTRecord(["hid": host.description, "v": "7"])
        let claim = LocalDiscovery.claim(from: record)
        XCTAssertEqual(claim?.host, host)
        XCTAssertEqual(claim?.version, 7)
    }

    func testTheRecordAMacAdvertisesIsRead() throws {
        // The Mac writes this record and its own tests pin the same file, so a
        // spelling the Mac's parser tolerates and Foundation's does not fails
        // here instead of hiding every Mac from every phone.
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<7 { root.deleteLastPathComponent() }
        let url = root.appendingPathComponent("crates/node/tests/fixtures/discovery/advertisement.json")
        let fixture = try JSONDecoder().decode(AdvertisedRecord.self, from: Data(contentsOf: url))

        let claim = LocalDiscovery.claim(from: NWTXTRecord(fixture.txt))
        XCTAssertEqual(claim?.host, HostId(try XCTUnwrap(UUID(uuidString: fixture.hostId))))
        XCTAssertEqual(claim?.version, fixture.version)
        XCTAssertEqual(claim?.name, fixture.txt["name"])
    }

    @MainActor
    func testARenamedInstanceIsOfferedByTheNameItsRecordCarries() {
        // A responder that meets a second "kitchen" on the network renames
        // this one "kitchen (2)". There is one kitchen, and its record says
        // what it is called.
        let browser = Browser()
        browser.discovery.saw([browser.sighting(instance: "kitchen (2)", routes: ["wifi"])])
        browser.lookups[0].settle("192.168.1.24:41234")
        XCTAssertEqual(browser.handedOver.last, [browser.found("192.168.1.24:41234")])
    }

    func testAnIncompleteOrUnreadableRecordIsPassedOver() {
        // Some other service answering on the same name, or an amux old enough
        // to have advertised itself differently. Neither is guessed at.
        for record in [
            NWTXTRecord(["v": "7"]),
            NWTXTRecord(["hid": host.description]),
            NWTXTRecord(["hid": "kitchen", "v": "7"]),
            NWTXTRecord(["hid": host.description, "v": "soon"]),
        ] {
            XCTAssertNil(LocalDiscovery.claim(from: record))
        }
    }

    func testAResolvedAddressIsSpelledAsTheLibraryParsesOne() {
        XCTAssertEqual(
            LocalDiscovery.address(of: .hostPort(host: "192.168.1.24", port: 41234)),
            "192.168.1.24:41234")
        XCTAssertEqual(
            LocalDiscovery.address(of: .hostPort(host: "2001:db8::5", port: 443)),
            "[2001:db8::5]:443")
    }

    func testAnInterfaceScopeIsNotPartOfTheAddress() {
        // The system appends the interface an address was learned on. The
        // library parses an address and a port and has nowhere to put one.
        let scoped = NWEndpoint.Host.ipv4(IPv4Address("192.168.1.24%en0")!)
        XCTAssertEqual(
            LocalDiscovery.address(of: .hostPort(host: scoped, port: 41234)),
            "192.168.1.24:41234")
    }

    func testAnAddressThatCannotBeDialledIsNotOffered() {
        // A link-local address means nothing without the interface it was
        // learned on, and a name is what was being resolved in the first place.
        XCTAssertNil(LocalDiscovery.address(of: .hostPort(host: "fe80::1%en0", port: 41234)))
        XCTAssertNil(LocalDiscovery.address(of: .hostPort(host: .name("kitchen.local", nil), port: 41234)))
        XCTAssertNil(LocalDiscovery.address(of: .service(
            name: "kitchen", type: LocalDiscovery.service, domain: "local.", interface: nil)))
    }

    func testARefusalIsToldApartFromANetworkThatIsNotReadyYet() {
        let denied = NWError.dns(DNSServiceErrorType(kDNSServiceErr_PolicyDenied))
        XCTAssertEqual(LocalDiscovery.permission(for: .waiting(denied)), .denied)
        XCTAssertEqual(LocalDiscovery.permission(for: .failed(denied)), .denied)
        XCTAssertEqual(LocalDiscovery.permission(for: .ready), .granted)
        // Nothing has been said yet, which is not the same as having been told no.
        XCTAssertEqual(LocalDiscovery.permission(for: .setup), .unknown)
        XCTAssertEqual(
            LocalDiscovery.permission(for: .waiting(.posix(.ENETDOWN))), .unknown)
    }

    @MainActor
    func testARefusalStaysUntilABrowserIsReady() {
        let browser = Browser()
        var told: [LocalNetworkPermission] = []
        browser.discovery.permissionChanged = { told.append($0) }
        let denied = NWError.dns(DNSServiceErrorType(kDNSServiceErr_PolicyDenied))
        browser.discovery.browserSaid(.waiting(denied))
        browser.discovery.browserSaid(.cancelled)
        browser.discovery.browserSaid(.setup)
        XCTAssertEqual(browser.discovery.permission, .denied)
        browser.discovery.stop()
        XCTAssertEqual(browser.discovery.permission, .denied, "the card outlives the browser")
        browser.discovery.browserSaid(.ready)
        XCTAssertEqual(told, [.denied, .granted])
    }

    @MainActor
    func testARouteThatGivesNoAddressInTimeGivesWayToTheNext() {
        // A Mac's own loopback is one route a simulator sees, and a lookup
        // over it can wait forever for a record nobody answers for. The
        // machine is still reached over the next route.
        let browser = Browser()
        browser.discovery.saw([browser.sighting(routes: ["loopback", "wifi"])])
        XCTAssertEqual(browser.lookups.map(\.route), ["loopback"])
        XCTAssertEqual(browser.lookups[0].within, LocalDiscovery.resolveTimeout)

        browser.lookups[0].settle(nil)
        XCTAssertEqual(browser.lookups.map(\.route), ["loopback", "wifi"])
        browser.lookups[1].settle("192.168.1.24:41234")

        XCTAssertEqual(browser.handedOver.last, [browser.found("192.168.1.24:41234")])
    }

    @MainActor
    func testAMachineNoRouteResolvedIsTriedAgainAfterAPause() {
        let browser = Browser()
        browser.discovery.saw([browser.sighting(routes: ["wifi"])])
        browser.lookups[0].settle(nil)
        XCTAssertEqual(browser.pauses, [LocalDiscovery.retryDelay])
        XCTAssertEqual(browser.lookups.count, 1)

        browser.resume()
        XCTAssertEqual(browser.lookups.map(\.route), ["wifi", "wifi"])
        browser.lookups[1].settle("192.168.1.24:41234")
        XCTAssertEqual(browser.handedOver.last, [browser.found("192.168.1.24:41234")])
    }

    @MainActor
    func testAMachineIsResolvedAgainWhenWhatItAdvertisesChanges() {
        // The same name coming back with a different record is a machine that
        // restarted, and may be listening somewhere else now.
        let browser = Browser()
        browser.discovery.saw([browser.sighting(routes: ["wifi"])])
        browser.lookups[0].settle("192.168.1.24:41234")

        browser.discovery.saw([browser.sighting(routes: ["wifi"])])
        XCTAssertEqual(browser.lookups.count, 1, "an unchanged advertisement was looked up again")

        browser.discovery.saw([browser.sighting(version: 8, routes: ["wifi"])])
        XCTAssertEqual(browser.lookups.count, 2)
        browser.lookups[1].settle("192.168.1.24:50000")
        XCTAssertEqual(
            browser.handedOver.last, [browser.found("192.168.1.24:50000", version: 8)])

        browser.discovery.saw([browser.sighting(version: 8, routes: ["wifi", "ethernet"])])
        XCTAssertEqual(browser.lookups.count, 3, "a newly seen interface was not tried")
    }

    @MainActor
    func testAReportFromABrowserThatWasStoppedIsIgnored() {
        // The browser reports off the main thread and its reports hop to
        // the main actor; one in flight when browsing stops lands after.
        let browser = Browser()
        let stale = browser.discovery.browse - 1
        browser.discovery.saw([browser.sighting(routes: ["wifi"])], browse: stale)
        XCTAssertTrue(browser.lookups.isEmpty, "a stopped browser's sighting was looked up")
        XCTAssertTrue(browser.handedOver.isEmpty)

        browser.discovery.saw([browser.sighting(routes: ["wifi"])])
        XCTAssertEqual(browser.lookups.map(\.route), ["wifi"])

        // Nor does its state lift a refusal the current browser stands by.
        let denied = NWError.dns(DNSServiceErrorType(kDNSServiceErr_PolicyDenied))
        browser.discovery.browserSaid(.waiting(denied))
        browser.discovery.browserSaid(.ready, browse: stale)
        XCTAssertEqual(browser.discovery.permission, .denied)
    }

    @MainActor
    func testAnAnswerForALookupThatWasReplacedIsIgnored() {
        let browser = Browser()
        browser.discovery.saw([browser.sighting(routes: ["wifi"])])
        browser.discovery.saw([browser.sighting(version: 8, routes: ["wifi"])])
        XCTAssertTrue(browser.lookups[0].abandoned)

        browser.lookups[0].settle("192.168.1.24:41234")
        XCTAssertEqual(browser.handedOver.last, [])
        browser.lookups[1].settle("192.168.1.24:50000")
        XCTAssertEqual(
            browser.handedOver.last, [browser.found("192.168.1.24:50000", version: 8)])
    }

    @MainActor
    func testAMachineThatLeavesIsWithdrawnAndItsLookupAbandoned() {
        let browser = Browser()
        browser.discovery.saw([browser.sighting(routes: ["wifi"])])
        browser.lookups[0].settle("192.168.1.24:41234")
        browser.discovery.saw([browser.sighting(version: 8, routes: ["wifi"])])

        browser.discovery.saw([])
        XCTAssertTrue(browser.lookups[1].abandoned)
        XCTAssertEqual(browser.handedOver.last, [])
        browser.resume()
        XCTAssertEqual(browser.lookups.count, 2)
    }

    @MainActor
    func testStoppingDoesNotWithdrawWhatWasFound() {
        // The library is told separately that the app went away, and closes
        // its direct links itself. Withdrawing here as well would leave a
        // phone coming back with an empty network until the browser has found
        // the same machines a second time.
        var handedOver: [[Found]] = []
        let discovery = LocalDiscovery { handedOver.append($0) }
        discovery.stop()
        XCTAssertEqual(handedOver.count, 0)
    }
}

/// A browser whose lookups and pauses the test settles by hand.
@MainActor
private final class Browser {
    final class Lookup {
        let route: String
        let within: TimeInterval
        let settle: @MainActor (String?) -> Void
        var abandoned = false

        init(route: String, within: TimeInterval, settle: @escaping @MainActor (String?) -> Void) {
            self.route = route
            self.within = within
            self.settle = settle
        }
    }

    let host = HostId(UUID(uuidString: "6D7A4B1E-3C2F-4A58-9B0D-1E2F3A4B5C6D")!)
    var lookups: [Lookup] = []
    var pauses: [TimeInterval] = []
    var handedOver: [[Found]] = []
    private var paused: [@MainActor () -> Void] = []
    private(set) lazy var discovery = LocalDiscovery(
        only: nil,
        resolve: { [unowned self] route, within, settle in
            let lookup = Lookup(route: Self.name(of: route), within: within, settle: settle)
            self.lookups.append(lookup)
            return { lookup.abandoned = true }
        },
        after: { [unowned self] delay, work in
            self.pauses.append(delay)
            self.paused.append(work)
        },
        handOver: { [unowned self] in self.handedOver.append($0) })

    func sighting(
        version: UInt32 = 7, instance: String = "kitchen", routes: [String]
    ) -> LocalDiscovery.Sighting {
        LocalDiscovery.Sighting(
            endpoint: .service(name: instance, type: LocalDiscovery.service, domain: "local.", interface: nil),
            record: NWTXTRecord(["hid": host.description, "v": String(version), "name": "kitchen"]),
            routes: routes.map { .hostPort(host: .name($0, nil), port: 1) })
    }

    func found(_ address: String, version: UInt32 = 7) -> Found {
        Found(hostId: host.bytes, name: "kitchen", version: version, addrs: [address], scope: "")
    }

    func resume() {
        let work = paused
        paused = []
        for run in work { run() }
    }

    private static func name(of route: NWEndpoint) -> String {
        guard case .hostPort(.name(let name, _), _) = route else { return "\(route)" }
        return name
    }
}

private struct AdvertisedRecord: Decodable {
    let hostId: String
    let version: UInt32
    let txt: [String: String]

    enum CodingKeys: String, CodingKey {
        case hostId = "host_id", version, txt
    }
}
