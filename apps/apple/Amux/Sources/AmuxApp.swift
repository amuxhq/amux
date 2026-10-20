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
        DoorServer.startIfRequested()
        #endif
    }

    var body: some Scene {
        WindowGroup {
            hostedRoot
        }
    }

    @ViewBuilder private var hostedRoot: some View {
        #if AMUX_DEBUG_TOOLS
        // An app-hosted unit test still launches the application's scene, and
        // so does an Xcode preview. Both supply and render their own roots,
        // so constructing the real composition here would open stores and
        // start the runtime only to leave them behind an invisible window.
        let environment = ProcessInfo.processInfo.environment
        if environment["AMUX_COMPONENT_SNAPSHOTS"] == "1"
            || environment["XCODE_RUNNING_FOR_PREVIEWS"] == "1" {
            Color.clear
        } else {
            RootView().readingAssistiveSettings()
        }
        #else
        RootView().readingAssistiveSettings()
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
    /// Built when first asked for: by the root view, or by a push.
    lazy var composition = Composition()

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions options: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        UNUserNotificationCenter.current().delegate = self
        return true
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
        switch await composition.runtime.warm(agent, inBackground: background) {
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
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse
    ) async {
        guard let agent = PushPayload.agent(response.notification.request.content.userInfo)
        else { return }
        await openPushed(agent)
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
