import AmuxCore
import AmuxDesign
import SwiftUI
import UIKit
import UserNotifications

@main
struct AmuxApp: App {
    @UIApplicationDelegateAdaptor private var delegate: AppDelegate

    init() {
        // First, before anything this app does: the mark that divides the
        // system's share of a launch from this app's.
        Signposts.emit(.appEntered)
        #if AMUX_DEBUG_TOOLS
        DoorServer.freshStartIfRequested()
        DoorServer.startIfRequested()
        #endif
    }

    var body: some Scene {
        WindowGroup {
            hostedRoot
        }
    }

    @ViewBuilder private var hostedRoot: some View {
        if Self.drawsItsOwnRoot {
            RootView().readingAssistiveSettings()
        } else {
            Color.clear
        }
    }

    /// Whether this launch is the app, rather than a host for something that
    /// renders roots of its own. An app-hosted unit test still launches the
    /// application's scene, and so does an Xcode preview; constructing the
    /// real composition for either would open stores and start the runtime
    /// only to leave them behind an invisible window.
    static var drawsItsOwnRoot: Bool {
        #if AMUX_DEBUG_TOOLS
        let environment = ProcessInfo.processInfo.environment
        return environment["AMUX_COMPONENT_SNAPSHOTS"] != "1"
            && environment["XCODE_RUNNING_FOR_PREVIEWS"] != "1"
        #else
        return true
        #endif
    }
}

/// Where the system hands the app what arrives while it is not on screen: a
/// "needs you" notification that wakes it in the background, and the tap on
/// one. It holds the app's composition, so a push that launches the app
/// before any window exists still has the runtime to warm its chat with.
@MainActor
final class AppDelegate: NSObject, UIApplicationDelegate, ObservableObject,
    UNUserNotificationCenterDelegate {
    /// Built as the app finishes launching, or by whatever asks for it first.
    lazy var composition = Composition()
    /// The background time held while the runtime gets ready for the app
    /// to be suspended.
    private var suspendTime = UIBackgroundTaskIdentifier.invalid

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions options: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        UNUserNotificationCenter.current().delegate = self
        // Built here rather than when the root view first asks, so the
        // runtime starts reading the store and dialling hosts while the
        // system is still connecting the scene: a launch that has to reach a
        // host gets there that much sooner.
        if AmuxApp.drawsItsOwnRoot { _ = composition }
        NotificationCenter.default.addObserver(
            forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.suspending() }
        }
        return true
    }

    /// The app was put away, and the system suspends it shortly. Before it
    /// does, every profile's store is made whole on the drive, under
    /// background time asked for that long, should the system end the app
    /// while it is suspended.
    private func suspending() {
        suspended()
        let held = UIApplication.shared.beginBackgroundTask(withName: "suspending") { [weak self] in
            self?.suspended()
        }
        suspendTime = held
        Task {
            await composition.runtime.suspending()
            if suspendTime == held { suspended() }
        }
    }

    private func suspended() {
        guard suspendTime != .invalid else { return }
        UIApplication.shared.endBackgroundTask(suspendTime)
        suspendTime = .invalid
    }

    /// Brings the chat the notification names current, and nothing else,
    /// before the app is put away again.
    func application(
        _ application: UIApplication,
        didReceiveRemoteNotification payload: [AnyHashable: Any]
    ) async -> UIBackgroundFetchResult {
        guard let agent = PushPayload.agent(payload) else { return .noData }
        Signposts.emit(.pushWoke)
        let background = application.applicationState == .background
        let warmed = await composition.runtime.warm(agent, inBackground: background)
        // The system suspends a woken app again once this returns.
        if background { await composition.runtime.suspending() }
        switch warmed {
        case .current:
            Signposts.emit(.pushCurrent)
            return .newData
        case .behind:
            Signposts.emit(.pushBehind)
            return .failed
        case .unknownHost, .offScreen:
            Signposts.emit(.pushBehind)
            return .noData
        }
    }

    /// A tap on the notification opens the chat it names, under the account
    /// whose profile trusts its host; coming to the foreground lists every
    /// agent again.
    ///
    /// The completion handler's form, finished on the main actor: UIKit
    /// takes the async form's return as the handler being called, off the
    /// main thread, and a tap on a real notification aborted the app there.
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
        withCompletionHandler completion: @escaping @Sendable () -> Void
    ) {
        guard let agent = PushPayload.agent(response.notification.request.content.userInfo)
        else { return completion() }
        Task { @MainActor in
            await openPushed(agent)
            completion()
        }
    }

    private func openPushed(_ agent: AgentKey) async {
        await composition.open(pushed: agent)
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter, willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .list]
    }
}
