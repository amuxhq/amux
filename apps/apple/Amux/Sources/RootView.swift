import AmuxCore
import AmuxDesign
import AmuxFeatures
import AmuxShell
import SwiftUI

/// The app's root.
///
/// A debug build shows whatever a driver has opened and the launch the
/// performance suite asked to time; anything else, and any build a person
/// installs, is the app itself.
struct RootView: View {
    @EnvironmentObject private var delegate: AppDelegate
    private var composition: Composition { delegate.composition }
    @Environment(\.scenePhase) private var phase

    var body: some View {
        scene
            // Whether anybody is looking at this phone is a fact about the app,
            // so it is said to the runtime rather than to any one screen. Put
            // away, the link is released at once: a socket left for the system
            // to freeze leaves every machine this phone was watching holding a
            // connection nobody is reading.
            .onChange(of: phase) { _, now in
                composition.runtime.setActive(now != .background)
            }
    }

    @ViewBuilder private var scene: some View {
        #if AMUX_DEBUG_TOOLS
        DrivenRoot { app }
            // What a driver queries and drives is the app itself.
            .onAppear {
                DoorHost.shared.adopt(delegate)
                // A link the launch carried goes through the same door the
                // system's own links go through, before anything else has
                // happened — which is what a cold start opened by a link is,
                // and the case where nobody has signed in yet.
                if let link = DoorHost.linkAsLaunchAsks { composition.router.open(link) }
            }
        #else
        app
        #endif
    }

    private var app: some View {
        ComposedShell(composition: composition)
            // What the app is wearing. Set here rather than inside a screen: it
            // is the whole app's, and a screen that carried it could not be
            // photographed in the other one.
            .modifier(ReportTools(composition: composition))
            .preferredColorScheme(composition.appearance?.colorScheme)
            .onOpenURL { composition.router.open($0) }
            .onAppear { Signposts.emitWhenPresented(.shellPresented) }
    }
}

/// The shell over the composition, or the store failure that replaces it.
///
/// A view of its own, handed only the composition, so that the root drawing
/// again leaves it alone. The root reads the scene's phase, which turns
/// active as the launch's first frame goes up; built inline, the shell was
/// handed new closures each time and rebuilt every screen under it, the
/// whole fleet included, inside that first frame.
private struct ComposedShell: View {
    let composition: Composition

    var body: some View {
        if let failure = composition.storeFailure {
            StoreFailureScreen(message: failure) {
                composition.runtime.relaunch()
            }
            .surface()
        } else {
            Shell(
                router: composition.router,
                accounts: composition.accounts,
                stores: composition.stores,
                signIn: composition.signIn,
                paywall: composition.paywall,
                deletion: composition.deletion,
                removal: composition.removal,
                appearance: composition.appearance,
                shareUsage: composition.shareUsage,
                report: { composition.beginReport() },
                actions: { composition.handle($0) }
            )
        }
    }
}
