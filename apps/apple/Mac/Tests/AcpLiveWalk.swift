import AmuxCore
import AmuxValues
import AmuxWords
import AppKit
import SwiftUI
import XCTest

/// A built-in agent that speaks ACP, run for real from the Mac: created
/// from New Agent, asked to run a command, which it asks permission for and
/// the Mac allows, then its model, thought level and mode changed from the
/// chat's settings and a second prompt answered on the new model.
///
/// It drives the person's own installed agent under their own sign-in, so
/// it runs only when `AMUX_MAC_LIVE_AGENT` names the agent (`antigravity`,
/// `opencode` or `grok`); every other run skips it. The daemon is a scratch
/// one, and the agent works in the scratch home's repository; only the
/// agent's own process sees the person's home, where it keeps its sign-in.
/// Nothing writes the agent's own settings. The window is never put on
/// screen; with `AMUX_MAC_CAPTURES` naming a folder, what it drew is kept
/// there as PNG files. xcodebuild hands a test only the variables it is
/// given with a `TEST_RUNNER_` prefix, the PATH the agent needs included:
///
///     TEST_RUNNER_AMUX_MAC_LIVE_AGENT=antigravity \
///     TEST_RUNNER_AMUX_MAC_CAPTURES=/tmp/mac TEST_RUNNER_PATH="$PATH" just mac test
@MainActor
final class AcpLiveWalk: XCTestCase {
    private var daemon: ScratchDaemon?

    override func setUp() async throws {
        let capturing = ProcessInfo.processInfo.environment["AMUX_MAC_CAPTURES"] != nil
        NSApplication.shared.setActivationPolicy(capturing ? .accessory : .prohibited)
    }

    override func tearDown() async throws {
        daemon?.stop()
        daemon = nil
    }

    func testABuiltInAgentAsksIsAllowedAndChangesItsSettingsFromTheMac() async throws {
        let environment = ProcessInfo.processInfo.environment
        guard let agent = environment["AMUX_MAC_LIVE_AGENT"], !agent.isEmpty else {
            throw XCTSkip("set AMUX_MAC_LIVE_AGENT to an installed agent's id to run it for real")
        }
        let command = try XCTUnwrap(Self.builtIns[agent], "no built-in agent \(agent)")
        let program = try XCTUnwrap(Self.found(command.program), "\(command.program) is not on the PATH")
        let home = try XCTUnwrap(environment["HOME"])
        let path = environment["PATH"] ?? ""
        let daemon = try ScratchDaemon(
            extra: """
                [agents.\(agent)]
                command = "\(program)"
                env = { HOME = "\(home)", PATH = "\(path)" }

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
        try await until("this Mac offers \(agent)", within: 60) {
            stores.hosts.local?.providers.contains { $0.provider == agent } ?? false
        }

        // Created from New Agent, its first message asks for a command.
        model.newAgent()
        let starting = try XCTUnwrap(model.starting)
        try await until("New Agent lists \(agent)") { starting.agents.contains { $0.provider == agent } }
        starting.choose(provider: agent)
        starting.choose(directory: daemon.home.appending(path: "src/amux").path)
        try await until("what \(agent) offers", within: 60) { starting.offer == .offered }
        starting.prompt = "Run the shell command `ls` in the current folder with your shell tool, then reply with just the word done."
        XCTAssertTrue(stores.startAgent())
        try await until("\(agent) started", within: 60) { stores.newAgent.created != nil }
        let key = try XCTUnwrap(stores.newAgent.created)
        model.started(key)
        try await until("its chat") { model.chat?.frame?.agent == key }
        let chat = try XCTUnwrap(model.chat)
        XCTAssertEqual(chat.frame?.provider.name, command.name)

        // The agent asks before it runs it; Allow goes from the Mac.
        try await until("the permission card", within: 240) { chat.ask != nil }
        try await keep(stage, "mac-live-\(agent)-approval")
        let card = try XCTUnwrap(chat.ask)
        let allow = try XCTUnwrap(card.choices.firstIndex { $0.primary }, "no choice leads")
        XCTAssertEqual(card.choices[allow].outcome, .allowOnce)
        chat.answer(choice: allow, note: nil)
        try await until("the turn's end", within: 240) {
            chat.ask == nil && chat.frame?.composer.activity == nil && Self.said(chat, "done")
        }
        try await keep(stage, "mac-live-\(agent)-replied")

        // Each setting the agent offers changed from the chat's settings,
        // and shown once the agent reports it.
        let settings = try XCTUnwrap(chat.settings)
        if let next = settings.models.first(where: { !$0.current && !$0.unlisted }) {
            chat.change(.model(next.value))
            try await until("the model \(next.value)", within: 60) {
                chat.settings?.models.first { $0.current }?.value == next.value
            }
        }
        if let next = chat.settings?.efforts.first(where: { !$0.current && !$0.unlisted }) {
            chat.change(.effort(next.value))
            try await until("the thought level \(next.value)", within: 60) {
                chat.settings?.efforts.first { $0.current }?.value == next.value
            }
        }
        if let next = chat.settings?.modes.first(where: { !$0.current && !$0.unlisted }) {
            chat.change(.mode(next.value))
            try await until("the mode \(next.value)", within: 60) {
                chat.settings?.modes.first { $0.current }?.value == next.value
            }
        }
        try await keep(stage, "mac-live-\(agent)-settings")

        // A prompt on the new settings is answered.
        chat.draft = "Reply with just the word ok."
        chat.send()
        try await until("the second reply", within: 240) {
            chat.frame?.composer.activity == nil && Self.said(chat, "ok")
        }
        try await keep(stage, "mac-live-\(agent)-changed")
    }

    // MARK: - The agents

    private struct BuiltIn {
        let program: String
        let name: String
    }

    /// The built-ins by id: the program a person has installed and the name
    /// the host gives the agent.
    private static let builtIns: [String: BuiltIn] = [
        "antigravity": BuiltIn(program: "agy", name: "Antigravity"),
        "opencode": BuiltIn(program: "opencode", name: "OpenCode"),
        "grok": BuiltIn(program: "grok", name: "Grok"),
    ]

    /// `program` on the test's PATH, as an absolute path.
    private static func found(_ program: String) -> String? {
        let path = ProcessInfo.processInfo.environment["PATH"] ?? ""
        return path.split(separator: ":")
            .map { URL(filePath: String($0)).appending(path: program).path }
            .first { FileManager.default.isExecutableFile(atPath: $0) }
    }

    /// Some prose row of the chat holds `word`, in any case.
    private static func said(_ chat: MacChat, _ word: String) -> Bool {
        chat.ids.contains { id in
            if case .prose(let text, _, _)? = chat.row(id)?.kind {
                return ChatWords.text(of: text).lowercased().contains(word)
            }
            return false
        }
    }

    // MARK: - Drawing

    /// With `AMUX_MAC_CAPTURES` set, keeps a photograph of the window as it
    /// stands there under `name`, taken past the edge of every display.
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

    private func until(_ what: String, within seconds: TimeInterval = 30, _ check: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !check() {
            if Date() > deadline { throw Waited(what: what) }
            try await Task.sleep(for: .milliseconds(100))
        }
    }

    private struct Waited: LocalizedError {
        let what: String
        var errorDescription: String? { "never saw \(what)" }
    }
}
