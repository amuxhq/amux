import AmuxCore
import AmuxMacScreens
import AmuxValues
import SwiftUI

/// The Mac app: a client of the amux daemon on this Mac and nothing more.
///
/// Its window sets a profile up the first time (the daemon's steps, drawn
/// natively), then lists the profile's agents; Settings changes this Mac's
/// everyday settings through the daemon.
@main
struct AmuxMacApp: App {
    @NSApplicationDelegateAdaptor private var delegate: AppDelegate
    @State private var model: AppModel

    init() {
        let model = AppModel()
        _model = State(initialValue: model)
        AppDelegate.model = model
        // Started with the app, not with a window: a launch that restores no
        // window still attaches, and a window opened later shows it.
        Task { await model.start() }
    }

    var body: some Scene {
        WindowGroup("Amux") {
            AppWindow(model: model)
                .frame(minWidth: 680, minHeight: 560)
        }
        .windowResizability(.contentMinSize)
        .commands { AppCommands(model: model) }
        Settings {
            SettingsWindow(model: model)
        }
    }
}

/// Applies a staged update as the app quits, however it quits, and closes
/// a pairing window setting up holds open first.
final class AppDelegate: NSObject, NSApplicationDelegate {
    @MainActor static var model: AppModel?

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        MainActor.assumeIsolated {
            guard let setup = Self.model?.setup else { return .terminateNow }
            // Whichever comes first, the window closed or two seconds, lets
            // the app go: a daemon that does not answer does not hold it
            // open. Closing ignores cancellation, so nothing waits for it
            // to end once the time is up.
            let reply = QuitReply(sender)
            Task { @MainActor in
                await setup.closeForQuit()
                reply.send()
            }
            Task { @MainActor in
                try? await Task.sleep(for: .seconds(2))
                reply.send()
            }
            return .terminateLater
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        MainActor.assumeIsolated { Self.model?.applyUpdate() }
    }
}

/// Answers a deferred quit once, whichever of its answers comes first.
@MainActor
private final class QuitReply {
    private var sender: NSApplication?

    init(_ sender: NSApplication) { self.sender = sender }

    func send() {
        sender?.reply(toApplicationShouldTerminate: true)
        sender = nil
    }
}

/// The menu bar's commands, each with the shortcut the tools beside this
/// app use for it.
private struct AppCommands: Commands {
    @Bindable var model: AppModel

    var body: some Commands {
        // One window holds the whole app, so ⌘N starts an agent.
        CommandGroup(replacing: .newItem) {
            Button("New Agent") { model.newAgent() }
                .keyboardShortcut("n")
                .disabled(!ready || (model.place.surface == .agents && model.place.agent == nil))
        }
        CommandGroup(before: .toolbar) {
            Button(model.place.sidebar ? "Hide Sidebar" : "Show Sidebar") { model.place.sidebar.toggle() }
                .keyboardShortcut("s", modifiers: [.control, .command])
            Button(model.place.changes ? "Hide Changes" : "Show Changes") { model.place.changes.toggle() }
                .keyboardShortcut("i", modifiers: [.option, .command])
                .disabled(!ready || model.place.agent == nil)
            Divider()
            Button("Agents") { model.place.surface = .agents }
                .keyboardShortcut("1")
            Button("Hosts") { model.place.surface = .hosts }
                .keyboardShortcut("2")
            Divider()
        }
        CommandMenu("Go") {
            Button("Back") { model.place.goBack(where: exists) }
                .keyboardShortcut("[")
                .disabled(!model.place.canGoBack)
            Button("Forward") { model.place.goForward(where: exists) }
                .keyboardShortcut("]")
                .disabled(!model.place.canGoForward)
        }
        CommandMenu("Agent") {
            Button("Open in Terminal") { if let agent = model.place.agent { model.openTerminal(agent) } }
                .keyboardShortcut("t", modifiers: [.shift, .command])
                .disabled(!ready || model.place.agent == nil)
        }
    }

    private var ready: Bool { model.stores != nil && model.setup?.content == nil }

    private func exists(_ agent: AgentKey) -> Bool { model.stores?.fleet.row(agent) != nil }
}
