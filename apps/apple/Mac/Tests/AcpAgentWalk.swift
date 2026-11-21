import AmuxCore
import AmuxValues
import AmuxWords
import AppKit
import SwiftUI
import XCTest

/// An agent that speaks ACP, started from the Mac and chatted with: the
/// fake ACP agent configured on a daemon of the test's own as a person adds
/// one under `[agents.<id>]`, beside a second one that is signed out there.
/// New Agent lists both by the names the config gives them and starts each
/// by its id; a chat names its agent as the host does, says in the agent's
/// own words how to sign in, and offers no terminal to attach to.
///
/// The app's window is drawn but never put on screen. With
/// `AMUX_MAC_CAPTURES` naming a folder, what it drew is kept there as PNG
/// files for a person to look at.
@MainActor
final class AcpAgentWalk: XCTestCase {
    private var daemon: ScratchDaemon?

    override func setUp() async throws {
        // The window server draws a window only for an app that may own
        // one; an accessory has no Dock icon and is never made active.
        let capturing = ProcessInfo.processInfo.environment["AMUX_MAC_CAPTURES"] != nil
        NSApplication.shared.setActivationPolicy(capturing ? .accessory : .prohibited)
    }

    override func tearDown() async throws {
        daemon?.stop()
        daemon = nil
    }

    func testAnAcpAgentIsListedStartedByIdAndNamedAsItsHostNamesIt() async throws {
        let scripts = try Self.scripts()
        let daemon = try ScratchDaemon(
            extra: """
                [agents.helper]
                name = "Helper"
                command = "\(ScratchDaemon.fakeAcp.path)"
                args = ["acp"]
                env = { AMUX_FAKE_SCRIPT = "\(scripts.helper.path)" }

                [agents.sleepy]
                name = "Sleepy"
                command = "\(ScratchDaemon.fakeAcp.path)"
                args = ["acp"]
                env = { AMUX_FAKE_SCRIPT = "\(scripts.sleepy.path)" }

                """)
        self.daemon = daemon
        setenv("AMUX_CONFIG", daemon.config.path, 1)
        defer { unsetenv("AMUX_CONFIG") }

        let model = AppModel()
        await model.start()
        XCTAssertEqual(model.phase, .attached)
        try await until("setting up") { model.setup?.step != nil }
        while model.setup?.step != nil { model.setup?.later() }
        model.setup?.close()

        let stage = Stage(AppWindow(model: model), size: CGSize(width: 1100, height: 720))
        defer { stage.close() }
        let stores = try XCTUnwrap(model.stores)
        try await until("this Mac lists both agents") {
            let offered = stores.hosts.local?.providers.map(\.provider) ?? []
            return offered.contains("helper") && offered.contains("sleepy")
        }

        // Sleepy is signed out, which its host learns from the first
        // conversation it refuses: its chat says so in its own words.
        let sleepy = try await start("sleepy", saying: "Are you there?", in: model)
        try await until("Sleepy's sign-in notice") { model.chat?.frame?.signIn != nil }
        XCTAssertEqual(model.chat?.frame?.provider.name, "Sleepy")
        XCTAssertEqual(model.chat?.frame?.signIn?.providerName, "Sleepy")
        XCTAssertEqual(model.chat?.frame?.signIn?.hint, "Run `fake-acp login` in the terminal")
        try await until("the fleet lists Sleepy") { model.row(sleepy) != nil }
        try await keep(stage, "mac-acp-chat-signed-out")
        XCTAssertFalse(model.attachable(sleepy), "it has no terminal of its own")

        // New Agent lists every agent this Mac offers, by the names its
        // config gives them, Sleepy signed out there: asking what it offers
        // as the page opens reads what its refused conversation showed.
        model.newAgent()
        let starting = try XCTUnwrap(model.starting)
        try await until("this Mac says Sleepy is signed out") {
            starting.agents.first { $0.provider == "sleepy" }?.signedIn == false
        }
        let names = starting.agents.map(\.name)
        XCTAssertEqual(names.prefix(2), ["Claude", "Codex"], "Claude and Codex lead")
        XCTAssertEqual(names.suffix(2), ["Helper", "Sleepy"], "the config's agents close, in its order")
        starting.choose(provider: "sleepy")
        starting.choose(directory: daemon.home.appending(path: "src/amux").path)
        try await keep(stage, "mac-new-agent-signed-out")

        // Helper starts on its own defaults, having listed no models yet,
        // and answers.
        let helper = try await start("helper", saying: "Say hello", in: model)
        try await until("Helper's reply") {
            guard let chat = model.chat else { return false }
            return chat.ids.contains { id in
                if case .prose(let text, _, _)? = chat.row(id)?.kind {
                    return ChatWords.text(of: text).contains("Hello from Helper")
                }
                return false
            }
        }
        try await until("the fleet lists Helper") { model.row(helper) != nil }
        let frame = try XCTUnwrap(model.chat?.frame)
        XCTAssertEqual(frame.provider.name, "Helper")
        XCTAssertFalse(frame.capabilities.attachable)
        XCTAssertFalse(model.attachable(helper))
        try await keep(stage, "mac-acp-chat")
    }

    /// Starts an agent of `provider` from New Agent with a first message,
    /// as a person does, and answers it once its chat is open.
    private func start(_ provider: String, saying prompt: String, in model: AppModel) async throws -> AgentKey {
        let folder = try XCTUnwrap(daemon).home.appending(path: "src/amux")
        let stores = try XCTUnwrap(model.stores)
        model.newAgent()
        let starting = try XCTUnwrap(model.starting)
        try await until("New Agent lists \(provider)") { starting.agents.contains { $0.provider == provider } }
        starting.choose(provider: provider)
        starting.choose(directory: folder.path)
        try await until("what \(provider) offers") { starting.offer == .offered }
        if starting.agents.first(where: { $0.provider == provider })?.signedIn != false {
            XCTAssertEqual(starting.settings?.models, [], "nothing listed before its first conversation")
        }
        starting.prompt = prompt
        XCTAssertTrue(stores.startAgent())
        try await until("\(provider) started") { stores.newAgent.created != nil }
        let agent = try XCTUnwrap(stores.newAgent.created)
        model.started(agent)
        try await until("its chat") { model.chat?.frame?.agent == agent }
        return agent
    }

    // MARK: - The agents

    /// A script the fake plays for each agent: Helper answers its first
    /// prompt; Sleepy is signed out.
    private static func scripts() throws -> (helper: URL, sleepy: URL) {
        let dir = try ScratchDaemon.directory()
        let helper = dir.appending(path: "helper.json")
        let sleepy = dir.appending(path: "sleepy.json")
        try #"""
        {"steps": [{"text": {"chunks": ["Hello from Helper. ", "I read the folder and it builds."]}}, "turn_end"]}
        """#.write(to: helper, atomically: true, encoding: .utf8)
        try #"""
        {"signed_out": true, "steps": []}
        """#.write(to: sleepy, atomically: true, encoding: .utf8)
        return (helper, sleepy)
    }

    // MARK: - Drawing

    /// With `AMUX_MAC_CAPTURES` set, keeps a photograph of the window as it
    /// stands there under `name`. Drawing a view into a bitmap leaves a
    /// scrolling list and the glass blank, so the window is put past the
    /// edge of every display, behind everything, and the window server
    /// photographs it, as the storybook's captures are taken.
    private func keep(_ stage: Stage, _ name: String) async throws {
        guard let out = ProcessInfo.processInfo.environment["AMUX_MAC_CAPTURES"] else { return }
        let window = stage.window
        window.setFrameOrigin(NSPoint(x: -20000, y: -20000))
        XCTAssertEqual(window.frame.origin.x, -20000, "the window stays past the edge of every display")
        window.orderBack(nil)
        defer { window.orderOut(nil) }
        try await Task.sleep(for: .milliseconds(1200))
        let photograph = unsafeBitCast(
            dlsym(UnsafeMutableRawPointer(bitPattern: -2), "CGWindowListCreateImage"),
            to: (@convention(c) (CGRect, UInt32, UInt32, UInt32) -> Unmanaged<CGImage>?).self)
        let includingWindow: UInt32 = 1 << 3
        let framing: UInt32 = 1 << 0 | 1 << 3
        let image = try XCTUnwrap(
            photograph(.null, includingWindow, UInt32(window.windowNumber), framing)?.takeRetainedValue())
        let folder = URL(filePath: out)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        try XCTUnwrap(NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:]))
            .write(to: folder.appending(path: "\(name).png"))
    }

    private func until(_ what: String, _ check: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(30)
        while !check() {
            if Date() > deadline { throw Waited(what: what) }
            try await Task.sleep(for: .milliseconds(50))
        }
    }

    private struct Waited: LocalizedError {
        let what: String
        var errorDescription: String? { "never saw \(what)" }
    }
}
