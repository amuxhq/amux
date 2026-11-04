import AmuxCore
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
            Task {
                // A daemon that does not answer does not hold the app open.
                await withTaskGroup(of: Void.self) { group in
                    group.addTask { await setup.closeForQuit() }
                    group.addTask { try? await Task.sleep(for: .seconds(2)) }
                    await group.next()
                    group.cancelAll()
                }
                sender.reply(toApplicationShouldTerminate: true)
            }
            return .terminateLater
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        MainActor.assumeIsolated { Self.model?.applyUpdate() }
    }
}
