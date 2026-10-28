import AmuxDesign
import AmuxFeatures
import SwiftUI

// Hosts, one host's page, and pairing with a machine.

#Preview("Hosts") { StoryScreen("hosts.list") }
#Preview("Hosts · empty") { StoryScreen("hosts.empty") }
#Preview("Hosts · network refused") { StoryScreen("hosts.network-refused") }
#Preview("Hosts · on the account, free") { StoryScreen("hosts.on-the-account-free") }
#Preview("Hosts · large text") { StoryScreen("hosts.large-text") }
#Preview("Host") { StoryScreen("host.page") }
#Preview("Host · confirm revoke") {
    ComponentCatalog.hostPageView(host: CatalogFixtures.deskHost, confirming: true)
        .environment(\.design, Design.app)
}

#Preview("Pair · code") { StoryScreen("pair.code-empty") }
#Preview("Pair · checking") { StoryScreen("pair.code-checking") }
#Preview("Pair · code refused") { StoryScreen("pair.code-refused") }
#Preview("Pair · needs a subscription") { StoryScreen("pair.code-subscribe") }
#Preview("Pair · needs an account") { StoryScreen("pair.needs-account") }
#Preview("Pair · confirm") { StoryScreen("pair.confirm") }
#Preview("Pair · paired") { StoryScreen("pair.trusted") }

#Preview("Hosts · every example") { StoryGallery(.hosts) }

#Preview("Pairing · every example") { StoryGallery(.pairing) }
