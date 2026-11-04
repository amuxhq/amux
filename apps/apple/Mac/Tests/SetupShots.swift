import AmuxCore
import AmuxValues
import AppKit
import CoreImage
import SwiftUI
import XCTest

/// Setting up a Mac, walked through the app's own model against a daemon of
/// its own, each screen drawn into `target/mac-onboarding/` to look at.
///
/// The screens are drawn into windows that are never put on screen, and the
/// test runner may not become the active app, so a run shows nothing on the
/// desktop and takes no input from it. The phone is the one thing not real:
/// its pairing reaches the model as the statuses the daemon would report.
@MainActor
final class SetupShots: XCTestCase {
    private var daemon: ScratchDaemon?

    override func setUp() async throws {
        NSApplication.shared.setActivationPolicy(.prohibited)
    }

    override func tearDown() async throws {
        daemon?.stop()
        daemon = nil
    }

    func testEveryStepOfSettingUpDrawsAndWalksToTheMainWindow() async throws {
        let daemon = try ScratchDaemon()
        self.daemon = daemon
        setenv("AMUX_CONFIG", daemon.config.path, 1)
        defer { unsetenv("AMUX_CONFIG") }

        let model = AppModel()
        await model.start()
        XCTAssertEqual(model.phase, .attached)
        try await until("setting up opens at pairing") {
            model.setup?.step == .pair_phone && model.settings != nil
        }
        let setup = try XCTUnwrap(model.setup)
        XCTAssertNotNil(model.settings?.startAtLogin, "a supervised install can start at login")

        // The app's own window, up for the whole walk: the pairing screen
        // opens a real window from the daemon as it appears, with its PIN
        // and a QR code that reads back as the daemon's link.
        let stage = Stage(AppWindow(model: model))
        defer { stage.close() }
        try await until("the pairing window") { setup.pairing.window != nil }
        let window = try XCTUnwrap(setup.pairing.window)
        XCTAssertEqual(window.pin.count, 6)
        let pair = try await shoot("01-pair", stage)
        XCTAssertEqual(qr(in: pair), window.qrLink, "the code on screen is the daemon's link")

        setup.pairingMoved(PairingStatusView(active: true, joining: true, lockedOut: false, paired: nil))
        try await shoot("01b-pairing-joining", stage)
        let phone = PairedDevice(hostId: Array(repeating: 7, count: 16), name: "Jordan’s iPhone")
        setup.pairingMoved(PairingStatusView(active: false, joining: false, lockedOut: false, paired: phone))
        XCTAssertEqual(setup.step, .keep_reachable, "pairing ends on keeping it connected")
        XCTAssertTrue(setup.startAtLogin && setup.keepAwake, "both are on until the person answers")
        try await shoot("02-keep-connected", stage)

        // Later on each, which keeps every setting as it is: nothing here
        // writes a login item.
        setup.later()
        XCTAssertEqual(setup.step, .sign_in)
        XCTAssertEqual(setup.also, phone, "the phone just paired, signed out, is offered the account")
        try await shoot("03-sign-in", stage)
        setup.offersMoved([
            SignInOfferView(
                hostId: phone.hostId, hostName: phone.name, email: "jordan@example.com",
                accountName: "Jordan")
        ])
        try await shoot("03b-sign-in-offered", stage)
        setup.offersMoved([])

        setup.later()
        XCTAssertEqual(setup.step, .projects)
        try await until("the suggestions") { setup.projectsLoaded }
        XCTAssertEqual(
            setup.projects.map { URL(filePath: $0.path).lastPathComponent }, ["website", "amux"],
            "where Claude Code and Codex ran, newest first")
        try await shoot("04-projects", stage)

        setup.later()
        XCTAssertEqual(setup.step, .improve)
        try await shoot("05-improve", stage)
        setup.later()
        XCTAssertEqual(setup.step, .first_agent)
        try await shoot("06-first-agent", stage)
        setup.later()
        XCTAssertNil(setup.step)

        // Everything skipped is offered again on the main window.
        try await until("the last skip kept") {
            setup.state?.steps.contains { $0.step == .first_agent && $0.state == .skipped } ?? false
        }
        // No phone really paired, so the daemon still counts pairing as
        // needed beside the five skipped.
        XCTAssertEqual(
            setup.toFinish, 6,
            "\(setup.state?.steps.map { "\($0.step):\($0.state)" } ?? [])")
        try await shoot("07-main-finish-setting-up", stage)
        let settings = Stage(SettingsWindow(model: model), size: CGSize(width: 540, height: 760))
        defer { settings.close() }
        try await shoot("08-settings", settings)
        setup.finishSettingUp()
        XCTAssertEqual(setup.step, .pair_phone, "finishing starts at the first step skipped")
        setup.close()
        XCTAssertFalse(
            FileManager.default.fileExists(atPath: daemon.home.appending(path: "Library/LaunchAgents").path),
            "setting up wrote no login item")
    }

    /// What the Mac app does at launch when no amux answers: it installs the
    /// copy it carries and starts it, and adds no login item.
    func testInstallingStartsAmuxWithoutALoginItem() async throws {
        let scratch = try ScratchDaemon.directory()
        let home = scratch.appending(path: "home")
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        let saved = ProcessInfo.processInfo.environment["HOME"]
        setenv("HOME", home.path, 1)
        setenv("AMUX_TEST_DISCOVERY_MODE", "disabled", 1)
        unsetenv("AMUX_CONFIG")
        defer {
            let stopped = ScratchDaemon.run(
                home.appending(path: ".amux/bin/amux"), ["server", "stop"],
                environment: ["HOME": home.path])
            XCTAssertTrue(stopped.contains("Stopped amux"), stopped)
            if let saved { setenv("HOME", saved, 1) }
            unsetenv("AMUX_TEST_DISCOVERY_MODE")
        }
        let installed = try await Runtime.install(
            InstallOptions(bundled: ScratchDaemon.amux.path, config: nil))
        guard case .installed(_, let bundled) = installed else {
            return XCTFail("installed, not \(installed)")
        }
        XCTAssertTrue(bundled)
        let status = ScratchDaemon.run(
            home.appending(path: ".amux/bin/amux"), ["status"], environment: ["HOME": home.path])
        XCTAssertTrue(status.contains("is running"), status)
        XCTAssertTrue(status.contains("Does not start at login"), status)
        XCTAssertFalse(
            FileManager.default.fileExists(atPath: home.appending(path: "Library/LaunchAgents").path),
            "installing wrote no login item")
    }

    // MARK: - Drawing

    /// Draws what `stage` shows in the light and dark appearances into
    /// `target/mac-onboarding/<name>[-light].png`, answering the dark one.
    @discardableResult
    private func shoot(_ name: String, _ stage: Stage) async throws -> NSBitmapImageRep {
        var dark: NSBitmapImageRep?
        for (appearance, suffix) in [(NSAppearance.Name.darkAqua, ""), (.aqua, "-light")] {
            stage.window.appearance = NSAppearance(named: appearance)
            // What the view loads as it appears or changes (its pairing
            // window, its settings) lands before it is drawn.
            try await Task.sleep(for: .milliseconds(400))
            let host = stage.host
            host.layoutSubtreeIfNeeded()
            let rep = try XCTUnwrap(host.bitmapImageRepForCachingDisplay(in: host.bounds))
            host.cacheDisplay(in: host.bounds, to: rep)
            let png = try XCTUnwrap(rep.representation(using: .png, properties: [:]))
            try png.write(to: Self.shots.appending(path: "\(name)\(suffix).png"))
            if dark == nil { dark = rep }
        }
        return try XCTUnwrap(dark)
    }

    private func qr(in rep: NSBitmapImageRep) -> String? {
        guard let image = rep.cgImage else { return nil }
        let detector = CIDetector(
            ofType: CIDetectorTypeQRCode, context: nil,
            options: [CIDetectorAccuracy: CIDetectorAccuracyHigh])
        return detector?.features(in: CIImage(cgImage: image))
            .compactMap { ($0 as? CIQRCodeFeature)?.messageString }.first
    }

    private static var shots: URL {
        let dir = ScratchDaemon.repository.appending(path: "target/mac-onboarding")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    private func until(_ what: String, _ check: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(30)
        while !check() {
            if Date() > deadline { throw Waited(what: what) }
            try await Task.sleep(for: .milliseconds(50))
        }
    }

    private struct Waited: Error, CustomStringConvertible {
        let what: String
        var description: String { "never saw \(what)" }
    }
}

/// A supervised amux of the checkout's own build, on an installation and a
/// home of its own, so nothing it does reaches the person's: discovery is
/// off, updates are manual, and a login item, should one be written, lands
/// in the scratch home. Its home holds two repositories Claude Code and
/// Codex ran in, for the projects step to suggest.
final class ScratchDaemon {
    let dir: URL
    let home: URL
    let config: URL

    static var repository: URL {
        URL(filePath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
    }

    static var amux: URL { repository.appending(path: "target/debug/amux") }

    static func directory() throws -> URL {
        let dir = URL(filePath: NSTemporaryDirectory())
            .appending(path: "amux-mac-\(UUID().uuidString.prefix(8))")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.resolvingSymlinksInPath()
    }

    init() throws {
        dir = try Self.directory()
        home = dir.appending(path: "home")
        config = dir.appending(path: "config.toml")
        let projects = try seedSessions()
        let settings = """
            host_name = "Jordan’s MacBook Pro"
            root = "\(dir.appending(path: "data").path)"
            supervisor = true
            updates = "manual"
            keep_awake = false
            telemetry = false

            """
        try settings.write(to: config, atomically: true, encoding: .utf8)
        XCTAssertEqual(projects.count, 2)
        let started = Self.run(Self.amux, ["server", "start"], environment: environment)
        XCTAssertTrue(started.contains("Started"), started)
    }

    private var environment: [String: String] {
        ["HOME": home.path, "AMUX_CONFIG": config.path, "AMUX_TEST_DISCOVERY_MODE": "disabled"]
    }

    func stop() {
        Self.run(Self.amux, ["server", "stop"], environment: environment)
    }

    /// Two repositories in the home, and the session logs that say Codex
    /// ran in one two days ago and Claude Code in the other yesterday.
    private func seedSessions() throws -> [URL] {
        let files = FileManager.default
        let amux = home.appending(path: "src/amux")
        let website = home.appending(path: "src/website")
        for repository in [amux, website] {
            try files.createDirectory(
                at: repository.appending(path: ".git"), withIntermediateDirectories: true)
        }
        let claude = home.appending(path: ".claude/projects/-src-website")
        let codex = home.appending(path: ".codex/sessions/2026/11/03")
        for dir in [claude, codex] {
            try files.createDirectory(at: dir, withIntermediateDirectories: true)
        }
        let logs = [
            (claude.appending(path: "one.jsonl"), #"{"cwd":"\#(website.path)"}"#, 1.0),
            (
                codex.appending(path: "rollout-a.jsonl"),
                #"{"type":"session_meta","payload":{"cwd":"\#(amux.path)"}}"#, 2.0
            ),
        ]
        for (log, line, daysAgo) in logs {
            try (line + "\n").write(to: log, atomically: true, encoding: .utf8)
            try files.setAttributes(
                [.modificationDate: Date().addingTimeInterval(-daysAgo * 86_400)],
                ofItemAtPath: log.path)
        }
        return [amux, website]
    }

    /// Runs an amux command to its end with `environment` over the test's
    /// own, answering what it printed.
    @discardableResult
    static func run(_ binary: URL, _ arguments: [String], environment: [String: String]) -> String {
        let process = Process()
        process.executableURL = binary
        process.arguments = arguments
        var merged = ProcessInfo.processInfo.environment
        merged["AMUX_LOG"] = nil
        merged["AMUX_CONFIG"] = nil
        merged.merge(environment) { _, given in given }
        process.environment = merged
        // A file rather than a pipe: the daemon a start leaves running holds
        // what it inherited open, so a pipe would never end.
        let log = URL(filePath: NSTemporaryDirectory()).appending(path: "amux-run-\(UUID().uuidString)")
        FileManager.default.createFile(atPath: log.path, contents: nil)
        defer { try? FileManager.default.removeItem(at: log) }
        guard let output = try? FileHandle(forWritingTo: log) else { return "no log" }
        process.standardOutput = output
        process.standardError = output
        do {
            try process.run()
        } catch {
            return "could not run \(binary.path): \(error)"
        }
        process.waitUntilExit()
        try? output.close()
        return (try? String(contentsOf: log, encoding: .utf8)) ?? ""
    }
}

/// A window that is never ordered front, holding one view for as long as the
/// test keeps it, as the app's window holds its content: the view's screens
/// appear and disappear only as what it shows changes, never per drawing.
@MainActor
private final class Stage {
    let window: NSWindow
    let host: NSView

    init(_ view: some View, size: CGSize = CGSize(width: 900, height: 640)) {
        let host = NSHostingView(
            rootView: view
                .frame(width: size.width, height: size.height)
                .background(Color(nsColor: .windowBackgroundColor)))
        host.frame = CGRect(origin: .zero, size: size)
        window = NSWindow(contentRect: host.frame, styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = host
        host.layoutSubtreeIfNeeded()
        self.host = host
    }

    func close() {
        window.contentView = nil
        window.close()
    }
}
