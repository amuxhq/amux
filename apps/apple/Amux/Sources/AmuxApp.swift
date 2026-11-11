import AmuxCore
import AmuxDesign
import AmuxPush
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
        // The bundled faces are registered with Core Text off the main
        // thread while UIKit connects the scene, so the shell's first frame
        // finds them ready instead of registering them in the middle of it.
        // Registration happens once; a screen that sets type first waits for
        // it rather than starting it again.
        DispatchQueue.global(qos: .userInitiated).async { BundledFonts.register() }
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
    /// renders roots of its own. An Xcode preview still launches the
    /// application's scene; constructing the real composition for it would
    /// open stores and start the runtime only to leave them behind an
    /// invisible window.
    static var drawsItsOwnRoot: Bool {
        #if AMUX_DEBUG_TOOLS
        let environment = ProcessInfo.processInfo.environment
        return environment["XCODE_RUNNING_FOR_PREVIEWS"] != "1"
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

    /// The platform's token for this phone, which comes at every launch that
    /// asks, since it can change.
    func application(
        _ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken token: Data
    ) {
        composition.pushes.token(token)
    }

    /// Brings the chat the notification names current, and nothing else,
    /// before the app is put away again. What the system hands over here is
    /// the push as it arrived, sealed, not the extension's rewrite.
    func application(
        _ application: UIApplication,
        didReceiveRemoteNotification payload: [AnyHashable: Any]
    ) async -> UIBackgroundFetchResult {
        guard let agent = PushPayload.agent(payload, keys: PushKeychain()) else { return .noData }
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
    /// agent again. One the extension couldn't open is tried here, since a
    /// phone that has since been unlocked can read its key, and one that
    /// still can't be opened names nobody, so the tap opens the fleet.
    ///
    /// The completion handler's form, finished on the main actor: UIKit
    /// takes the async form's return as the handler being called, off the
    /// main thread, and a tap on a real notification aborted the app there.
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
        withCompletionHandler completion: @escaping @Sendable () -> Void
    ) {
        let agent = PushPayload.agent(
            response.notification.request.content.userInfo, keys: PushKeychain())
        Task { @MainActor in
            await openPushed(agent)
            completion()
        }
    }

    private func openPushed(_ agent: AgentKey?) async {
        guard let agent else { return composition.router.openFleet() }
        await composition.open(pushed: agent)
    }

    /// A push about the chat somebody is reading tells them nothing the chat
    /// doesn't, so it stays quiet; any other shows over the app.
    ///
    /// The completion handler's form, for the same reason as a tap's.
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter, willPresent notification: UNNotification,
        withCompletionHandler completion: @escaping @Sendable (UNNotificationPresentationOptions) -> Void
    ) {
        let agent = PushPayload.agent(notification.request.content.userInfo, keys: PushKeychain())
        Task { @MainActor in
            let reading = agent != nil && agent == composition.router.chatOnScreen
            completion(reading ? [] : [.banner, .list])
        }
    }
}
