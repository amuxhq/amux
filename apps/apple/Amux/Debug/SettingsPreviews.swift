import AmuxCore
import AmuxDesign
import AmuxFeatures
import SwiftUI

// The settings sheet, signing in from it, and every page it opens:
// subscribing, removing an account from this phone and deleting it.

#Preview("Settings") { StoryScreen("settings.signed-in") }
#Preview("Settings · signed out") { StoryScreen("settings.signed-out") }
#Preview("Settings · large text") { StoryScreen("settings.large-text") }
#Preview("Settings · remove account") {
    let stores = StoryFleet.busy(accounts: 2)
    NavigationStack {
        YouScreen(accounts: stores.accounts, removing: AccountId("ada")) { _ in }
    }
    .environment(\.design, Design.app)
}
#Preview("Settings · delete account") { StoryScreen("settings.delete") }
#Preview("Settings · signing in") {
    StoryApp(settingsOpen: true, signIn: SignInStore(phase: .handingOff)) { StoryFleet.busy(accounts: 0) }
}
#Preview("Settings · sign-in refused") {
    StoryApp(settingsOpen: true, signIn: SignInStore(phase: .failed("amux.sh did not answer"))) {
        StoryFleet.busy(accounts: 0)
    }
}
#Preview("Settings · signed in as another account") {
    StoryApp(
        settingsOpen: true,
        signIn: SignInStore(
            phase: .mismatched(wanted: CatalogFixtures.ada, got: CatalogFixtures.lovelace),
            intent: .returning(CatalogFixtures.ada))
    ) { StoryFleet.busy(accounts: 1) }
}
#Preview("Paywall") { StoryScreen("settings.paywall") }

#Preview("Settings · every example") { StoryGallery(.settings) }
