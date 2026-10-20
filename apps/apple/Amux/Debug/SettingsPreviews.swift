import AmuxCore
import AmuxDesign
import AmuxFeatures
import SwiftUI

// The settings sheet, and every page it opens: signing in, subscribing,
// removing an account from this phone and deleting it.

#Preview("Settings") { StoryApp(settingsOpen: true) }
#Preview("Settings · two accounts") { StoryScreen("settings.two-accounts") }
#Preview("Settings · signed out") { StoryScreen("settings.signed-out") }
#Preview("Settings · remove account") {
    let stores = StoryFleet.busy(accounts: 2)
    NavigationStack {
        YouScreen(accounts: stores.accounts, removing: AccountId("ada")) { _ in }
    }
    .environment(\.design, Design.app)
}
#Preview("Settings · delete account") { StoryScreen("settings.delete") }

#Preview("Sign in") { StoryScreen("settings.sign-in") }
#Preview("Sign in · waiting") { StoryScreen("settings.sign-in-waiting") }
#Preview("Sign in · failed") { StoryScreen("settings.sign-in-failed") }
#Preview("Sign in · another account") { StoryScreen("settings.sign-in-mismatched") }

#Preview("Paywall") { StoryScreen("settings.paywall") }
#Preview("Paywall · confirming") { StoryScreen("settings.paywall-confirming") }
#Preview("Paywall · not confirmed") { StoryScreen("settings.paywall-unconfirmed") }
#Preview("Paywall · subscribed") { StoryScreen("settings.paywall-subscribed") }
