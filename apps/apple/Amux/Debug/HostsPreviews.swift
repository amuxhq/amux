import AmuxDesign
import AmuxFeatures
import SwiftUI

// Hosts, one host's page, and pairing with a machine.

#Preview("Hosts") { StoryScreen("hosts.list") }
#Preview("Hosts · empty") { StoryScreen("hosts.empty") }
#Preview("Hosts · network refused") { StoryScreen("hosts.network-refused") }

#Preview("Host") { StoryScreen("host.page") }
#Preview("Host · revoked") { StoryScreen("host.revoked") }
#Preview("Host · confirm revoke") {
    ComponentCatalog.hostPageView(host: CatalogFixtures.deskHost, confirming: true)
        .environment(\.design, Design.app)
}

#Preview("Pair · code") { StoryScreen("pair.code-empty") }
#Preview("Pair · typing") { StoryScreen("pair.code-typing") }
#Preview("Pair · checking") { StoryScreen("pair.code-checking") }
#Preview("Pair · code refused") { StoryScreen("pair.code-refused") }
#Preview("Pair · needs a subscription") { StoryScreen("pair.code-subscribe") }
#Preview("Pair · confirm") { StoryScreen("pair.confirm") }
#Preview("Pair · paired") { StoryScreen("pair.trusted") }
#Preview("Pair · link checking") { StoryScreen("pair.link-checking") }
#Preview("Pair · link refused") { StoryScreen("pair.link-refused") }
#Preview("Pair · needs an account") { StoryScreen("pair.needs-account") }
