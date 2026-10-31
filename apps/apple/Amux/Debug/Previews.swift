import AmuxCore
import AmuxDesign
import AmuxFeatures
import SwiftUI

// Xcode previews: the whole app, one gallery per family of the shared story
// list, the visual foundation, and the few pages no story draws.

#Preview("App") { StoryApp() }
#Preview("App · chat open") { StoryApp(chatOpen: true) }
#Preview("App · settings open") { StoryApp(settingsOpen: true) }
#Preview("App · review open") { StoryApp(reviewOpen: true) }
#Preview("App · new agent") { StoryApp(newAgentOpen: true) }

#Preview("Fleet · every example") { StoryGallery(.fleet) }
#Preview("Asks") { StoryGallery(.asks) }
#Preview("Chat states") { StoryGallery(.chat) }
#Preview("Composer") { StoryGallery(.composer) }
#Preview("Rows") { StoryGallery(.rows) }
#Preview("Hosts · every example") { StoryGallery(.hosts) }
#Preview("Pairing · every example") { StoryGallery(.pairing) }
#Preview("Settings · every example") { StoryGallery(.settings) }
#Preview("Review · every example") { StoryGallery(.review) }
#Preview("Report · every example") { StoryGallery(.report) }

#Preview("Foundation") { FoundationSheet() }
#Preview("Working marks") { WorkingMarks() }

#Preview("Host · confirm revoke") {
    ComponentCatalog.hostPageView(host: CatalogFixtures.deskHost, confirming: true)
        .environment(\.design, Design.app)
}
#Preview("Settings · remove account") {
    let stores = StoryFleet.busy(accounts: 2)
    NavigationStack {
        YouScreen(accounts: stores.accounts, removing: AccountId("ada")) { _ in }
    }
    .environment(\.design, Design.app)
}
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
