import AmuxCore
import AmuxDesign
import AmuxFeatures
import SwiftUI
import XCTest

@testable import AmuxTestSupport

/// The door is a protocol between a Swift app and a Rust driver, so what it
/// puts on the wire is pinned here rather than left to whatever Codable does
/// with an enum this week.
final class DoorTests: XCTestCase {
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    private func wire(_ request: DoorRequest) throws -> [String: Any] {
        let data = try encoder.encode(request)
        return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    func testEveryRequestSurvivesTheWire() throws {
        let requests: [DoorRequest] = [
            .open(screen: "home", fixture: nil),
            .open(screen: "home", fixture: "home-quiet"),
            .cloud(CloudScript()),
            .store(StoreScript()),
            .calls,
            .accounts,
            .connect(relay: "http://127.0.0.1:8080", token: "bearer", user: "ada", tier: .pro),
            .connect(relay: "http://127.0.0.1:8080", token: "bearer", user: "ada", tier: .free),
            .addAccount(user: "work", token: "another-bearer"),
            .awaitReconciled(seconds: 90),
            .conversation(agent: "00000000-0000-0000-0000-000000000001"),
            .appearance(.dark),
            .dynamicType("accessibility3"),
            .assist(motion: true, transparency: true),
            .settle,
            .query,
            .capture(path: "/tmp/home.png"),
            .tap(identifier: "home.row.aurora"),
            .tap(identifier: "home.row.aurora", after: 0.6),
            .choose(label: "Rename"),
            .perform(identifier: "review.line.0.0.2", action: "Select line"),
            .type(identifier: "composer.field", text: "hello"),
            .search(text: "auth"),
            .paste(identifier: "composer.field", text: "a paste worth naming"),
            .scroll(direction: "up"),
            .scroll(direction: "bottom", identifier: "chat.settings.workmode.plan"),
            .attach(
                agent: "6f1c1f8e-0000-4000-8000-000000000001", kind: "image",
                name: "screenshot.png", mime: "image/png", base64: "iVBORw0KGgo="),
            .pair(qr: #"{"host_id":"…","cloud_url":"http://127.0.0.1:9","secret":[1]}"#),
            .pairByCode(host: "9f1c1f8e-0000-4000-8000-000000000002", pin: "419507"),
            .awaitAgent(agent: "6f1c1f8e-0000-4000-8000-000000000001", seconds: 120),
            .screenshot,
            .holdBackground,
            .awaitBackground(seconds: 10),
            .push(path: "/tmp/needs-you.apns"),
            .shutdown,
        ]
        for request in requests {
            let round = try decoder.decode(DoorRequest.self, from: encoder.encode(request))
            XCTAssertEqual(round, request)
        }
    }

    /// What the account service and the App Store will answer is written by
    /// hand, by somebody driving the app from another language. So a request
    /// says only what it is changing, everything else keeps the answer most
    /// states want, and none of it is spelled in Swift's own encoding of a
    /// nested enum.
    func testAScriptSaysOnlyWhatItChanges() throws {
        let refusing = Data(
            #"{"kind":"cloud","cloud":{"signIn":"refused","reason":"no such account"}}"#.utf8)
        guard case .cloud(let script) = try decoder.decode(DoorRequest.self, from: refusing) else {
            return XCTFail("that was not a cloud request")
        }
        XCTAssertEqual(script.state.signIn, .refused("no such account"))
        XCTAssertEqual(script.state.entitlement, .active(grant: .purchased(.web), renews: nil))
        XCTAssertEqual(script.state.token, "scripted-connect-token")

        // An account with nothing bought is refused a relay credential, which
        // is said by naming the token as nothing rather than by leaving it out.
        let gated = Data(
            #"{"kind":"cloud","cloud":{"entitlement":"none","token":null}}"#.utf8)
        guard case .cloud(let closed) = try decoder.decode(DoorRequest.self, from: gated) else {
            return XCTFail("that was not a cloud request")
        }
        XCTAssertEqual(closed.state.entitlement, .none)
        XCTAssertNil(closed.state.token)

        let pending = Data(#"{"kind":"store","store":{"purchase":"pending"}}"#.utf8)
        guard case .store(let store) = try decoder.decode(DoorRequest.self, from: pending) else {
            return XCTFail("that was not a store request")
        }
        XCTAssertEqual(store.state.purchase, .pending)
        XCTAssertEqual(store.state.plans, ScriptedStoreState.offered)
    }

    func testRequestsAreTaggedAndFlat() throws {
        let opened = try wire(.open(screen: "home", fixture: "home-quiet"))
        XCTAssertEqual(opened["kind"] as? String, "open")
        XCTAssertEqual(opened["screen"] as? String, "home")
        XCTAssertEqual(opened["fixture"] as? String, "home-quiet")

        // A fixture the request does not name is left out rather than sent as
        // null, so the driver's own requests read the same as the app's.
        XCTAssertNil(try wire(.open(screen: "home", fixture: nil))["fixture"])

        XCTAssertEqual(try wire(.appearance(.dark))["appearance"] as? String, "dark")
        XCTAssertEqual(try wire(.dynamicType("accessibility3"))["size"] as? String, "accessibility3")
        let assist = try wire(.assist(motion: true, transparency: false))
        XCTAssertEqual(assist["motion"] as? Bool, true)
        XCTAssertEqual(assist["transparency"] as? Bool, false)
        XCTAssertEqual(try wire(.capture(path: "/tmp/x.png"))["path"] as? String, "/tmp/x.png")
        XCTAssertEqual(try wire(.settle)["kind"] as? String, "settle")
        XCTAssertEqual(try wire(.awaitReconciled(seconds: 90))["seconds"] as? Double, 90)
        XCTAssertEqual(try wire(.pair(qr: "payload"))["qr"] as? String, "payload")
        let code = try wire(.pairByCode(host: "workstation", pin: "419507"))
        XCTAssertEqual(code["host"] as? String, "workstation")
        XCTAssertEqual(code["pin"] as? String, "419507")
        let pasting = try wire(.paste(identifier: "composer.field", text: "a long paste"))
        XCTAssertEqual(pasting["identifier"] as? String, "composer.field")
        XCTAssertEqual(pasting["text"] as? String, "a long paste")
        // The kind of attachment travels under its own name rather than under
        // the request's: every request in this door already spells its own
        // kind, and two "kind" fields in one flat object cannot both be read.
        let attaching = try wire(.attach(
            agent: "aurora", kind: "file", name: "parser.rs", mime: "text/x-rust",
            base64: "cGFyc2Vy"))
        XCTAssertEqual(attaching["kind"] as? String, "attach")
        XCTAssertEqual(attaching["attachment"] as? String, "file")
        XCTAssertEqual(attaching["name"] as? String, "parser.rs")
        XCTAssertEqual(attaching["mime"] as? String, "text/x-rust")
        XCTAssertEqual(attaching["base64"] as? String, "cGFyc2Vy")
    }

    func testAnUnknownRequestIsRefused() {
        let unknown = Data(#"{"kind":"levitate"}"#.utf8)
        XCTAssertThrowsError(try decoder.decode(DoorRequest.self, from: unknown))
    }

    @MainActor
    func testEveryReplySurvivesTheWire() throws {
        let state = VisibleState(
            screen: "home",
            typeSize: "large",
            voiceOver: true,
            elements: [VisibleElement(
                identifier: "home.title", label: "Agents", value: nil,
                frame: VisibleFrame(x: 16, y: 64, width: 200, height: 32), enabled: true)],
            reconciled: true, unconfirmed: 3)
        let replies: [DoorReply] = [
            .ack,
            .state(state),
            .conversation(ConversationReading(agent: "helper", frame: nil, rows: [], ask: nil)),
            .captured(path: "/tmp/home.png", width: 1206, height: 2622, scale: 3),
            .paired(host: "workstation"),
            .error("unimplemented: home"),
        ]
        for reply in replies {
            let round = try decoder.decode(DoorReply.self, from: encoder.encode(reply))
            XCTAssertEqual(round, reply)
        }
    }

    func testACaptureNamesItsSizeInPixels() throws {
        let data = try encoder.encode(
            DoorReply.captured(path: "/tmp/home.png", width: 1206, height: 2622, scale: 3))
        let wire = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(wire["kind"] as? String, "captured")
        XCTAssertEqual(wire["width"] as? Int, 1206)
        XCTAssertEqual(wire["height"] as? Int, 2622)
        XCTAssertEqual(wire["scale"] as? Int, 3)
    }

    func testTheReadinessFileNamesThePort() throws {
        let ready = Door.Ready(port: 51201, pid: 4242)
        let round = try decoder.decode(Door.Ready.self, from: encoder.encode(ready))
        XCTAssertEqual(round, ready)
        XCTAssertEqual(Door.readyArgument, "amux-door-ready")
    }

    /// A door request names a type size in words.
    func testTypeSizesAreDoorNames() throws {
        XCTAssertEqual(DynamicTypeSize(doorName: "large"), .large)
        XCTAssertEqual(DynamicTypeSize(doorName: "accessibility5"), .accessibility5)
        XCTAssertNil(DynamicTypeSize(doorName: "enormous"))
    }
}
