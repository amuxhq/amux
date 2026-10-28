import AmuxCore
import AmuxDesign
import AmuxFeatures
import SwiftUI

// The settings sheet, and every page it opens: signing in, subscribing,
// removing an account from this phone and deleting it.

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
#Preview("Sign in") { StoryScreen("settings.sign-in") }
#Preview("Paywall") { StoryScreen("settings.paywall") }

#Preview("Settings · every example") { StoryGallery(.settings) }
